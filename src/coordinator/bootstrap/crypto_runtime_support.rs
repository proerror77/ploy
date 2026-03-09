use super::*;

#[path = "crypto_runtime_support/market_data_runtime.rs"]
mod market_data_runtime;

use self::market_data_runtime::initialize_crypto_market_data_runtime;

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

    initialize_crypto_market_data_runtime(
        shared_pool,
        freshness,
        use_data_plane,
        data_plane.as_ref(),
        binance_ws.clone(),
        pm_ws.clone(),
        event_matcher.clone(),
        &crypto_cfg,
        &all_coins,
        lob_agent_enabled,
        rl_agent_enabled,
    )
    .await;

    Ok(CryptoRuntimeSupport {
        managed_runtime_data_plane: data_plane.clone(),
        shared_crypto_data_plane: data_plane,
    })
}
