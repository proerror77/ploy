use super::*;

pub(super) struct CryptoRuntimePreflight {
    pub(super) use_data_plane: bool,
    pub(super) crypto_cfg: crate::strategy::CryptoTradingConfig,
    pub(super) all_coins: Vec<String>,
    pub(super) data_plane: Option<Arc<PlatformDataPlane>>,
    pub(super) binance_ws: Arc<BinanceWebSocket>,
    pub(super) pm_ws: Arc<PolymarketWebSocket>,
    pub(super) lob_agent_enabled: bool,
    pub(super) rl_agent_enabled: bool,
}

pub(super) async fn initialize_crypto_runtime_preflight(
    config: &PlatformBootstrapConfig,
    app_config: &AppConfig,
    runtime_crypto_targets: &strategy_deployments::RuntimeCryptoStrategyTargets,
    freshness: &Arc<crate::platform::DataPlaneFreshness>,
) -> Result<CryptoRuntimePreflight> {
    let use_data_plane = env_bool("PLOY_DATA_PLANE", false);
    let crypto_cfg = config.crypto.clone();
    let momentum_enabled = config.enable_crypto_momentum;
    let pm_5m_directional_enabled = config.enable_crypto_pm_5m_directional;
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
    if pm_5m_directional_enabled {
        let mut coins: Vec<String> = if runtime_crypto_targets.pm_5m_directional_coins.is_empty() {
            crypto_cfg.coins.clone()
        } else {
            runtime_crypto_targets
                .pm_5m_directional_coins
                .iter()
                .cloned()
                .collect()
        };
        coins.sort();
        coins.dedup();
        let symbols: Vec<String> = coins.iter().map(|c| format!("{}USDT", c)).collect();
        planner_requirements.push((
            crate::platform::ConsumerId::from("pm-5m-directional"),
            Domain::Crypto,
            vec![DataFeed::BinanceSpot {
                symbols: symbols.clone(),
            }],
        ));
        for coin in coins {
            if !all_coins.contains(&coin) {
                all_coins.push(coin);
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

    Ok(CryptoRuntimePreflight {
        use_data_plane,
        crypto_cfg,
        all_coins,
        data_plane,
        binance_ws,
        pm_ws,
        lob_agent_enabled,
        rl_agent_enabled,
    })
}
