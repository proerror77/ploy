use super::*;
use chrono::Utc;

#[derive(Default)]
pub(super) struct CryptoRuntimeSupport {
    pub(super) managed_runtime_data_plane: Option<Arc<PlatformDataPlane>>,
    pub(super) shared_crypto_data_plane: Option<Arc<PlatformDataPlane>>,
}

pub(super) async fn initialize_crypto_runtime_support(
    config: &PlatformBootstrapConfig,
    app_config: &AppConfig,
    runtime_crypto_targets: &strategy_deployments::RuntimeCryptoStrategyTargets,
    shared_pool: Option<&PgPool>,
    pm_client: Option<&PolymarketClient>,
    freshness: &Arc<crate::platform::DataPlaneFreshness>,
) -> Result<CryptoRuntimeSupport> {
    let use_data_plane = env_bool("PLOY_DATA_PLANE", false);
    let crypto_cfg = config.crypto.clone();
    let momentum_enabled = config.enable_crypto_momentum;
    let pattern_memory_enabled = config.enable_crypto_pattern_memory;
    let split_arb_enabled = config.enable_crypto_split_arb;
    let lob_cfg = config.managed_crypto.lob_ml.clone();
    let lob_agent_enabled = config.managed_crypto.enable_lob_ml;
    #[cfg(feature = "rl")]
    let rl_cfg = config.managed_crypto.rl_policy.clone();
    #[cfg(feature = "rl")]
    let rl_agent_enabled = config.managed_crypto.enable_rl_policy;
    #[cfg(not(feature = "rl"))]
    let rl_agent_enabled = false;

    let pm_client_ref = pm_client.ok_or_else(|| {
        crate::error::PloyError::Validation(
            "crypto domain requires a Polymarket client, but none was initialized".to_string(),
        )
    })?;
    let event_matcher = Arc::new(EventMatcher::new(pm_client_ref.clone()));
    if let Err(e) = event_matcher.refresh().await {
        warn!(error = %e, "crypto event matcher refresh failed (continuing)");
    }

    let mut all_coins: Vec<String> = Vec::new();
    let mut planner_requirements: Vec<(crate::platform::ConsumerId, Domain, Vec<DataFeed>)> =
        Vec::new();
    if momentum_enabled {
        let symbols: Vec<String> = crypto_cfg
            .coins
            .iter()
            .map(|c| format!("{}USDT", c))
            .collect();
        planner_requirements.push((
            crate::platform::ConsumerId::from(format!("momentum-{}", crypto_cfg.agent_id)),
            Domain::Crypto,
            vec![DataFeed::BinanceSpot { symbols }],
        ));
        for coin in &crypto_cfg.coins {
            if !all_coins.contains(coin) {
                all_coins.push(coin.clone());
            }
        }
    }
    if lob_agent_enabled {
        let symbols: Vec<String> = lob_cfg.coins.iter().map(|c| format!("{}USDT", c)).collect();
        planner_requirements.push((
            crate::platform::ConsumerId::from("lob-ml"),
            Domain::Crypto,
            vec![DataFeed::BinanceSpot { symbols }],
        ));
        for coin in &lob_cfg.coins {
            if !all_coins.contains(coin) {
                all_coins.push(coin.clone());
            }
        }
    }
    #[cfg(feature = "rl")]
    if rl_agent_enabled {
        let symbols: Vec<String> = rl_cfg.coins.iter().map(|c| format!("{}USDT", c)).collect();
        planner_requirements.push((
            crate::platform::ConsumerId::from("rl-policy"),
            Domain::Crypto,
            vec![DataFeed::BinanceSpot { symbols }],
        ));
        for coin in &rl_cfg.coins {
            if !all_coins.contains(coin) {
                all_coins.push(coin.clone());
            }
        }
    }
    if use_data_plane && pattern_memory_enabled {
        let mut coins: Vec<String> = if runtime_crypto_targets.pattern_memory_coins.is_empty() {
            crypto_cfg.coins.clone()
        } else {
            runtime_crypto_targets
                .pattern_memory_coins
                .iter()
                .cloned()
                .collect()
        };
        coins.sort();
        coins.dedup();
        for coin in coins {
            if !all_coins.contains(&coin) {
                all_coins.push(coin);
            }
        }
    }
    if use_data_plane && split_arb_enabled {
        let mut coins: Vec<String> = if runtime_crypto_targets.split_arb_coins.is_empty() {
            crypto_cfg.coins.clone()
        } else {
            runtime_crypto_targets
                .split_arb_coins
                .iter()
                .cloned()
                .collect()
        };
        coins.sort();
        coins.dedup();
        for coin in coins {
            if !all_coins.contains(&coin) {
                all_coins.push(coin);
            }
        }
    }
    if all_coins.is_empty() {
        warn!("crypto domain enabled but no crypto agents are active (coins set is empty)");
    }

    let subscription_plan = crate::platform::SubscriptionPlanner::build_plan(planner_requirements);
    info!(
        unique_keys = subscription_plan.key_count(),
        total_refs = subscription_plan.ref_count(),
        binance_symbols = subscription_plan.binance_symbols().len(),
        "subscription plan built (shadow audit)"
    );

    let symbols: Vec<String> = all_coins.iter().map(|c| format!("{}USDT", c)).collect();
    let mut data_plane: Option<Arc<PlatformDataPlane>> = None;
    let (binance_ws, pm_ws) = if use_data_plane {
        let data_plane_config = DataPlaneConfig {
            polymarket_ws_url: app_config.market.ws_url.clone(),
            binance_spot_symbols: symbols.clone(),
            binance_kline_symbols: symbols.clone(),
            binance_kline_intervals: vec!["5m".to_string(), "15m".to_string()],
            binance_kline_closed_only: true,
            chainlink_symbols: vec![],
        };
        let dp = Arc::new(PlatformDataPlane::new(
            data_plane_config,
            Arc::clone(freshness),
        ));
        dp.start(Vec::new()).await?;
        info!("PlatformDataPlane started");

        let binance_ws = dp.binance_ws().ok_or_else(|| {
            crate::error::PloyError::Validation(
                "PLOY_DATA_PLANE=1 but PlatformDataPlane has no Binance WS adapter".to_string(),
            )
        })?;
        let pm_ws = dp.polymarket_ws().ok_or_else(|| {
            crate::error::PloyError::Validation(
                "PLOY_DATA_PLANE=1 but PlatformDataPlane has no Polymarket WS adapter".to_string(),
            )
        })?;
        data_plane = Some(dp);
        (binance_ws, pm_ws)
    } else {
        let binance_ws = Arc::new(BinanceWebSocket::new(symbols));
        let pm_ws = Arc::new(PolymarketWebSocket::new(&app_config.market.ws_url));

        binance_ws.set_freshness(Arc::clone(freshness));
        pm_ws.set_freshness(Arc::clone(freshness));
        info!("data plane freshness tracker attached to WS adapters");
        (binance_ws, pm_ws)
    };

    let collector_min_remaining_secs = env_i64("PM_COLLECTOR_MIN_REMAINING_SECS", 0)
        .max(-86400)
        .min(86400);
    let mut desired: HashMap<String, Side> = HashMap::new();
    let mut collector_targets: Vec<crate::collector::CollectorTokenTarget> = Vec::new();
    for coin in &all_coins {
        let symbol = format!("{}USDT", coin.to_uppercase());
        for ev in event_matcher
            .get_events_with_min_remaining(&symbol, collector_min_remaining_secs)
            .await
        {
            desired.insert(ev.up_token_id.clone(), Side::Up);
            desired.insert(ev.down_token_id.clone(), Side::Down);

            let expires_at = Some(ev.end_time + chrono::Duration::hours(1));
            collector_targets.push(
                crate::collector::CollectorTokenTarget::new(ev.up_token_id.clone(), "CRYPTO")
                    .with_expires_at(expires_at)
                    .with_metadata(serde_json::json!({
                        "symbol": symbol.as_str(),
                        "side": "UP",
                        "condition_id": ev.condition_id.as_str(),
                        "slug": ev.slug.as_str(),
                        "title": ev.title.as_str(),
                        "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                    })),
            );
            collector_targets.push(
                crate::collector::CollectorTokenTarget::new(ev.down_token_id.clone(), "CRYPTO")
                    .with_expires_at(expires_at)
                    .with_metadata(serde_json::json!({
                        "symbol": symbol.as_str(),
                        "side": "DOWN",
                        "condition_id": ev.condition_id.as_str(),
                        "slug": ev.slug.as_str(),
                        "title": ev.title.as_str(),
                        "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                    })),
            );
        }
    }
    if use_data_plane {
        for (token, side) in &desired {
            pm_ws.register_token(token, *side).await;
        }
        pm_ws.request_resubscribe();
        info!(
            agent = %crypto_cfg.agent_id,
            token_count = desired.len(),
            "seeded PM token mappings for crypto data collection"
        );
    } else {
        let (_added, _removed, _updated, total) = pm_ws.reconcile_token_sides(&desired).await;
        info!(
            agent = %crypto_cfg.agent_id,
            token_count = total,
            "seeded PM token mappings for crypto data collection"
        );
    }

    if let Some(pool) = shared_pool {
        if let Err(e) = crate::collector::ensure_collector_token_targets_table(pool).await {
            warn!(
                agent = %crypto_cfg.agent_id,
                error = %e,
                "failed to ensure collector_token_targets table"
            );
        }

        if let Err(e) =
            crate::collector::upsert_collector_token_targets(pool, &collector_targets).await
        {
            warn!(
                agent = %crypto_cfg.agent_id,
                error = %e,
                "failed to upsert collector token targets (crypto)"
            );
        }
    }

    let pm_ws_collector = pm_ws.clone();
    let matcher_collector = event_matcher.clone();
    let coins_collector = all_coins.clone();
    let agent_id_collector = crypto_cfg.agent_id.clone();
    let pool_collector = shared_pool.cloned();
    let use_data_plane_collector = use_data_plane;
    let initial_last_desired = if use_data_plane_collector {
        desired.clone()
    } else {
        HashMap::new()
    };
    tokio::spawn(async move {
        let refresh_secs = env_u64("PM_COLLECTOR_REFRESH_SECS", PM_COLLECTOR_REFRESH_SECS).max(10);
        let mut tick = tokio::time::interval(Duration::from_secs(refresh_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_desired = initial_last_desired;

        loop {
            tick.tick().await;

            if let Err(e) = matcher_collector.refresh().await {
                warn!(agent = %agent_id_collector, error = %e, "pm token collector refresh failed");
                continue;
            }

            let mut desired: HashMap<String, Side> = HashMap::new();
            let mut collector_targets: Vec<crate::collector::CollectorTokenTarget> = Vec::new();
            for coin in &coins_collector {
                let symbol = format!("{}USDT", coin.to_uppercase());
                for ev in matcher_collector
                    .get_events_with_min_remaining(&symbol, collector_min_remaining_secs)
                    .await
                {
                    desired.insert(ev.up_token_id.clone(), Side::Up);
                    desired.insert(ev.down_token_id.clone(), Side::Down);

                    let expires_at = Some(ev.end_time + chrono::Duration::hours(1));
                    collector_targets.push(
                        crate::collector::CollectorTokenTarget::new(
                            ev.up_token_id.clone(),
                            "CRYPTO",
                        )
                        .with_expires_at(expires_at)
                        .with_metadata(serde_json::json!({
                            "symbol": symbol.as_str(),
                            "side": "UP",
                            "condition_id": ev.condition_id.as_str(),
                            "slug": ev.slug.as_str(),
                            "title": ev.title.as_str(),
                            "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                        })),
                    );
                    collector_targets.push(
                        crate::collector::CollectorTokenTarget::new(
                            ev.down_token_id.clone(),
                            "CRYPTO",
                        )
                        .with_expires_at(expires_at)
                        .with_metadata(serde_json::json!({
                            "symbol": symbol.as_str(),
                            "side": "DOWN",
                            "condition_id": ev.condition_id.as_str(),
                            "slug": ev.slug.as_str(),
                            "title": ev.title.as_str(),
                            "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                        })),
                    );
                }
            }

            if use_data_plane_collector {
                if desired != last_desired {
                    let previous_token_count = last_desired.len();
                    for (token, side) in &desired {
                        pm_ws_collector.register_token(token, *side).await;
                    }
                    pm_ws_collector.request_resubscribe();
                    info!(
                        agent = %agent_id_collector,
                        previous_token_count,
                        token_count = desired.len(),
                        "pm token collector refreshed token set on shared data-plane ws; resubscribe requested"
                    );
                    last_desired = desired;
                }
            } else {
                let (added, removed, updated, total) =
                    pm_ws_collector.reconcile_token_sides(&desired).await;
                if added > 0 || removed > 0 {
                    pm_ws_collector.request_resubscribe();
                    info!(
                        agent = %agent_id_collector,
                        added,
                        removed,
                        updated,
                        token_count = total,
                        "pm token collector reconciled token set; resubscribe requested"
                    );
                }
            }

            if let Some(pool) = pool_collector.as_ref() {
                let ensured = crate::collector::ensure_collector_token_targets_table(pool).await;
                if let Err(e) = ensured {
                    warn!(
                        agent = %agent_id_collector,
                        error = %e,
                        "failed to ensure collector_token_targets table"
                    );
                }

                if let Err(e) =
                    crate::collector::upsert_collector_token_targets(pool, &collector_targets).await
                {
                    warn!(
                        agent = %agent_id_collector,
                        error = %e,
                        "failed to upsert collector token targets (crypto)"
                    );
                }
            }
        }
    });

    let mut crypto_persistence_pipeline: Option<crate::platform::PersistencePipelineHandle> = None;
    if let Some(pool) = shared_pool {
        let (orderbook_levels_default, orderbook_snapshot_secs_default) = (20usize, 60i64);
        let orderbook_levels =
            env_usize("PM_ORDERBOOK_LEVELS", orderbook_levels_default).clamp(1, 200);
        let orderbook_snapshot_ms = match std::env::var("PM_ORDERBOOK_SNAPSHOT_MS") {
            Ok(raw) => raw.parse::<u64>().unwrap_or(0),
            Err(_) => (env_i64(
                "PM_ORDERBOOK_SNAPSHOT_SECS",
                orderbook_snapshot_secs_default,
            )
            .max(0) as u64)
                .saturating_mul(1000),
        };
        let orderbook_require_hash_change = env_bool("PM_ORDERBOOK_REQUIRE_HASH_CHANGE", true);

        let quote_table_ready = match ensure_clob_quote_ticks_table(pool).await {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    agent = crypto_cfg.agent_id,
                    error = %e,
                    "failed to ensure clob_quote_ticks table; quote persistence bridge disabled"
                );
                false
            }
        };
        let price_table_ready = match ensure_binance_price_ticks_table(pool).await {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    agent = crypto_cfg.agent_id,
                    error = %e,
                    "failed to ensure binance_price_ticks table; Binance price persistence bridge disabled"
                );
                false
            }
        };
        let orderbook_table_ready = match ensure_clob_orderbook_snapshots_table(pool).await {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    agent = crypto_cfg.agent_id,
                    error = %e,
                    "failed to ensure clob_orderbook_snapshots table; orderbook persistence bridge disabled"
                );
                false
            }
        };

        if quote_table_ready || price_table_ready || orderbook_table_ready {
            let pipeline_config = crate::platform::PersistenceConfig {
                clob_quote_min_interval_secs: CLOB_PERSIST_MIN_INTERVAL_SECS,
                binance_price_min_interval_secs: BINANCE_PERSIST_MIN_INTERVAL_SECS,
                binance_lob_snapshot_interval_ms: env_u64("BN_LOB_SNAPSHOT_MS", 1000).max(100)
                    as i64,
                binance_lob_max_levels: env_usize("BN_LOB_LEVELS", 20).clamp(0, 200),
                clob_orderbook_snapshot_interval_ms: orderbook_snapshot_ms as i64,
                clob_orderbook_max_levels: orderbook_levels,
                clob_orderbook_require_hash_change: orderbook_require_hash_change,
                ..Default::default()
            };
            let pipeline_handle = crate::platform::PersistencePipeline::spawn_with_freshness(
                pool.clone(),
                pipeline_config,
                Some(Arc::clone(freshness)),
            );
            crypto_persistence_pipeline = Some(pipeline_handle.clone());

            if quote_table_ready {
                let quote_rx = if use_data_plane {
                    data_plane.as_ref().and_then(|dp| dp.subscribe_quotes())
                } else {
                    Some(pm_ws.subscribe_updates())
                };
                if let Some(quote_rx) = quote_rx {
                    pipeline_handle.spawn_bridge(
                        quote_rx,
                        format!("{}.quote", crypto_cfg.agent_id),
                        |update| {
                            Some(crate::platform::PersistenceEvent::ClobQuote(
                                crate::platform::ClobQuoteTick {
                                    token_id: update.token_id.clone(),
                                    side: update.side.as_str().to_string(),
                                    best_bid: update.quote.best_bid,
                                    best_ask: update.quote.best_ask,
                                    bid_size: update.quote.bid_size,
                                    ask_size: update.quote.ask_size,
                                    domain: Domain::Crypto,
                                    received_at: Utc::now(),
                                },
                            ))
                        },
                    );
                } else {
                    warn!("persistence quote bridge unavailable: no quote receiver");
                }
            }

            if price_table_ready {
                let price_rx = if use_data_plane {
                    data_plane.as_ref().and_then(|dp| dp.subscribe_prices())
                } else {
                    Some(binance_ws.subscribe())
                };
                if let Some(price_rx) = price_rx {
                    pipeline_handle.spawn_bridge(
                        price_rx,
                        format!("{}.price", crypto_cfg.agent_id),
                        |update| {
                            Some(crate::platform::PersistenceEvent::BinancePrice(
                                crate::platform::BinancePriceTick {
                                    symbol: update.symbol.clone(),
                                    price: Some(update.price),
                                    quantity: update.quantity,
                                    trade_time: update.timestamp,
                                },
                            ))
                        },
                    );
                } else {
                    warn!("persistence price bridge unavailable: no price receiver");
                }
            }

            if orderbook_table_ready {
                let book_rx = if use_data_plane {
                    data_plane.as_ref().and_then(|dp| dp.subscribe_books())
                } else {
                    Some(pm_ws.subscribe_books())
                };
                if let Some(book_rx) = book_rx {
                    pipeline_handle.spawn_bridge(
                        book_rx,
                        format!("{}.orderbook", crypto_cfg.agent_id),
                        |book_msg| {
                            use sha2::{Digest, Sha256};
                            let bids_json =
                                serde_json::to_value(&book_msg.bids).unwrap_or_default();
                            let asks_json =
                                serde_json::to_value(&book_msg.asks).unwrap_or_default();
                            let mut hasher = Sha256::new();
                            hasher.update(bids_json.to_string().as_bytes());
                            hasher.update(asks_json.to_string().as_bytes());
                            let hash = format!("{:x}", hasher.finalize());
                            Some(crate::platform::PersistenceEvent::ClobOrderbook(
                                crate::platform::ClobOrderbookSnapshot {
                                    domain: Domain::Crypto,
                                    token_id: book_msg.asset_id.clone(),
                                    market: Some(book_msg.market.clone()),
                                    bids: bids_json,
                                    asks: asks_json,
                                    book_timestamp: book_msg
                                        .timestamp
                                        .as_deref()
                                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                                        .map(|dt| dt.with_timezone(&Utc)),
                                    hash,
                                    source: "polymarket_ws".into(),
                                    context: None,
                                },
                            ))
                        },
                    );
                } else {
                    warn!("persistence orderbook bridge unavailable: no book receiver");
                }
            }

            info!(agent = crypto_cfg.agent_id, "persistence pipeline started");
        } else {
            warn!(
                agent = crypto_cfg.agent_id,
                "all market-data persistence tables unavailable; WS persistence bridges disabled"
            );
        }

        spawn_polymarket_trade_persistence(
            event_matcher.clone(),
            pool.clone(),
            crypto_cfg.agent_id.clone(),
            all_coins.clone(),
            Domain::Crypto,
        );
        info!(
            agent = crypto_cfg.agent_id,
            "market data persistence tasks started"
        );
    }

    let mut enable_binance_lob = lob_agent_enabled || rl_agent_enabled;
    if let Ok(raw) = std::env::var("PLOY_BINANCE_LOB__ENABLED") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => enable_binance_lob = true,
            "0" | "false" | "no" | "off" => enable_binance_lob = false,
            _ => {}
        }
    }

    if enable_binance_lob {
        let depth_symbols: Vec<String> = match std::env::var("PLOY_BINANCE_LOB__SYMBOLS") {
            Ok(raw) => raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_uppercase())
                .collect(),
            Err(_) => all_coins.iter().map(|c| format!("{}USDT", c)).collect(),
        };

        let depth_stream = Arc::new(
            crate::collector::BinanceDepthStream::new(depth_symbols)
                .with_freshness(Arc::clone(freshness)),
        );
        if let Some(pool) = shared_pool {
            match ensure_binance_lob_ticks_table(pool).await {
                Ok(()) => {
                    if let Some(ph) = crypto_persistence_pipeline.clone() {
                        let rx = depth_stream.subscribe();
                        let agent_id = crypto_cfg.agent_id.clone();
                        let max_levels = env_usize("BN_LOB_LEVELS", 20).clamp(0, 200);
                        ph.spawn_bridge(rx, format!("{}.binance_lob", agent_id), move |update| {
                            let symbol = update.symbol.clone();
                            let (bids, asks) = if max_levels == 0 {
                                (Vec::new(), Vec::new())
                            } else {
                                (
                                    lob_levels_json(&update.raw_state, true, max_levels),
                                    lob_levels_json(&update.raw_state, false, max_levels),
                                )
                            };
                            Some(crate::platform::PersistenceEvent::BinanceLob(
                                crate::platform::BinanceLobTick {
                                    symbol,
                                    update_id: update.snapshot.update_id,
                                    best_bid: Some(update.snapshot.best_bid),
                                    best_ask: Some(update.snapshot.best_ask),
                                    mid_price: Some(update.snapshot.mid_price),
                                    spread_bps: Some(update.snapshot.spread_bps),
                                    obi_5: update.snapshot.obi_5.to_f64(),
                                    obi_10: update.snapshot.obi_10.to_f64(),
                                    bid_volume_5: Some(update.snapshot.bid_volume_5),
                                    ask_volume_5: Some(update.snapshot.ask_volume_5),
                                    bids: serde_json::to_value(&bids).unwrap_or_default(),
                                    asks: serde_json::to_value(&asks).unwrap_or_default(),
                                    event_time: update.snapshot.timestamp,
                                },
                            ))
                        });
                    } else {
                        warn!(
                            agent = crypto_cfg.agent_id,
                            "Binance LOB persistence requested but pipeline handle unavailable"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        agent = crypto_cfg.agent_id,
                        error = %e,
                        "failed to ensure binance_lob_ticks table; Binance LOB persistence bridge disabled"
                    );
                }
            }
        }

        let ds = depth_stream.clone();
        tokio::spawn(async move {
            if let Err(e) = ds.run().await {
                error!(error = %e, "binance depth stream error");
            }
        });

        info!(
            agent = crypto_cfg.agent_id,
            "binance LOB depth stream started"
        );
    }

    if !use_data_plane {
        let bws = binance_ws.clone();
        tokio::spawn(async move {
            if let Err(e) = bws.run().await {
                error!(error = %e, "binance websocket error");
            }
        });

        let pws = pm_ws.clone();
        tokio::spawn(async move {
            if let Err(e) = pws.run(Vec::new()).await {
                error!(error = %e, "polymarket websocket error");
            }
        });
    }

    Ok(CryptoRuntimeSupport {
        managed_runtime_data_plane: data_plane.clone(),
        shared_crypto_data_plane: data_plane,
    })
}
