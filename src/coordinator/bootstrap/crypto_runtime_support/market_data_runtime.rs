use super::*;
use chrono::Utc;

pub(super) async fn initialize_crypto_market_data_runtime(
    shared_pool: Option<&PgPool>,
    freshness: &Arc<crate::data_plane::DataPlaneFreshness>,
    use_data_plane: bool,
    data_plane: Option<&Arc<PlatformDataPlane>>,
    binance_ws: Arc<BinanceWebSocket>,
    pm_ws: Arc<PolymarketWebSocket>,
    event_matcher: Arc<EventMatcher>,
    crypto_cfg: &crate::strategy::CryptoTradingConfig,
    all_coins: &[String],
    lob_agent_enabled: bool,
    rl_agent_enabled: bool,
) {
    let crypto_persistence_pipeline = initialize_crypto_persistence_pipeline(
        shared_pool,
        freshness,
        use_data_plane,
        data_plane,
        &binance_ws,
        &pm_ws,
        event_matcher,
        crypto_cfg,
        all_coins,
    )
    .await;

    maybe_start_binance_lob_runtime(
        shared_pool,
        freshness,
        crypto_persistence_pipeline.clone(),
        crypto_cfg,
        all_coins,
        lob_agent_enabled,
        rl_agent_enabled,
    )
    .await;

    if !use_data_plane {
        spawn_raw_websocket_tasks(binance_ws, pm_ws);
    }
}

async fn initialize_crypto_persistence_pipeline(
    shared_pool: Option<&PgPool>,
    freshness: &Arc<crate::data_plane::DataPlaneFreshness>,
    use_data_plane: bool,
    data_plane: Option<&Arc<PlatformDataPlane>>,
    binance_ws: &Arc<BinanceWebSocket>,
    pm_ws: &Arc<PolymarketWebSocket>,
    event_matcher: Arc<EventMatcher>,
    crypto_cfg: &crate::strategy::CryptoTradingConfig,
    all_coins: &[String],
) -> Option<crate::persistence::PersistencePipelineHandle> {
    let Some(pool) = shared_pool else {
        return None;
    };

    let (orderbook_levels_default, orderbook_snapshot_secs_default) = (20usize, 60i64);
    let orderbook_levels = env_usize("PM_ORDERBOOK_LEVELS", orderbook_levels_default).clamp(1, 200);
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

    let pipeline_handle = if quote_table_ready || price_table_ready || orderbook_table_ready {
        let pipeline_config = crate::persistence::PersistenceConfig {
            clob_quote_min_interval_secs: CLOB_PERSIST_MIN_INTERVAL_SECS,
            binance_price_min_interval_secs: BINANCE_PERSIST_MIN_INTERVAL_SECS,
            binance_lob_snapshot_interval_ms: env_u64("BN_LOB_SNAPSHOT_MS", 0) as i64,
            binance_lob_max_levels: env_usize("BN_LOB_LEVELS", 20).clamp(0, 200),
            clob_orderbook_snapshot_interval_ms: orderbook_snapshot_ms as i64,
            clob_orderbook_max_levels: orderbook_levels,
            clob_orderbook_require_hash_change: orderbook_require_hash_change,
            ..Default::default()
        };
        let pipeline_handle = crate::persistence::PersistencePipeline::spawn_with_freshness(
            pool.clone(),
            pipeline_config,
            Some(Arc::clone(freshness)),
        );

        if quote_table_ready {
            let quote_rx = if use_data_plane {
                data_plane.and_then(|dp| dp.subscribe_quotes())
            } else {
                Some(pm_ws.subscribe_updates())
            };
            if let Some(quote_rx) = quote_rx {
                pipeline_handle.spawn_bridge(
                    quote_rx,
                    format!("{}.quote", crypto_cfg.agent_id),
                    |update| {
                        Some(crate::persistence::PersistenceEvent::ClobQuote(
                            crate::persistence::ClobQuoteTick {
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
                data_plane.and_then(|dp| dp.subscribe_prices())
            } else {
                Some(binance_ws.subscribe())
            };
            if let Some(price_rx) = price_rx {
                pipeline_handle.spawn_bridge(
                    price_rx,
                    format!("{}.price", crypto_cfg.agent_id),
                    |update| {
                        Some(crate::persistence::PersistenceEvent::BinancePrice(
                            crate::persistence::BinancePriceTick {
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
                data_plane.and_then(|dp| dp.subscribe_books())
            } else {
                Some(pm_ws.subscribe_books())
            };
            if let Some(book_rx) = book_rx {
                pipeline_handle.spawn_bridge(
                    book_rx,
                    format!("{}.orderbook", crypto_cfg.agent_id),
                    |book_msg| {
                        use sha2::{Digest, Sha256};
                        let bids_json = serde_json::to_value(&book_msg.bids).unwrap_or_default();
                        let asks_json = serde_json::to_value(&book_msg.asks).unwrap_or_default();
                        let mut hasher = Sha256::new();
                        hasher.update(bids_json.to_string().as_bytes());
                        hasher.update(asks_json.to_string().as_bytes());
                        let hash = format!("{:x}", hasher.finalize());
                        Some(crate::persistence::PersistenceEvent::ClobOrderbook(
                            crate::persistence::ClobOrderbookSnapshot {
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
        Some(pipeline_handle)
    } else {
        warn!(
            agent = crypto_cfg.agent_id,
            "all market-data persistence tables unavailable; WS persistence bridges disabled"
        );
        None
    };

    spawn_polymarket_trade_persistence(
        event_matcher,
        pool.clone(),
        crypto_cfg.agent_id.clone(),
        all_coins.to_vec(),
        Domain::Crypto,
    );
    info!(
        agent = crypto_cfg.agent_id,
        "market data persistence tasks started"
    );

    pipeline_handle
}

async fn maybe_start_binance_lob_runtime(
    shared_pool: Option<&PgPool>,
    freshness: &Arc<crate::data_plane::DataPlaneFreshness>,
    crypto_persistence_pipeline: Option<crate::persistence::PersistencePipelineHandle>,
    crypto_cfg: &crate::strategy::CryptoTradingConfig,
    all_coins: &[String],
    lob_agent_enabled: bool,
    rl_agent_enabled: bool,
) {
    let mut enable_binance_lob = lob_agent_enabled || rl_agent_enabled;
    if let Ok(raw) = std::env::var("PLOY_BINANCE_LOB__ENABLED") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => enable_binance_lob = true,
            "0" | "false" | "no" | "off" => enable_binance_lob = false,
            _ => {}
        }
    }

    if !enable_binance_lob {
        return;
    }

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
                if let Some(ph) = crypto_persistence_pipeline {
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
                        Some(crate::persistence::PersistenceEvent::BinanceLob(
                            crate::persistence::BinanceLobTick {
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

fn spawn_raw_websocket_tasks(binance_ws: Arc<BinanceWebSocket>, pm_ws: Arc<PolymarketWebSocket>) {
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
