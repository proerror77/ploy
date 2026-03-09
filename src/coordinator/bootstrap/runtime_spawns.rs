use super::*;
use crate::agents::{GovernanceAgent, GovernanceContext, OpenClawAgent};

#[derive(Debug, Clone)]
pub(super) struct ManagedStrategyRuntimeSpawn {
    pub(super) strategy_label: &'static str,
    pub(super) agent_id: String,
    pub(super) domain: Domain,
    pub(super) risk_params: AgentRiskParams,
    pub(super) strategy_config_toml: String,
}

pub(super) fn spawn_managed_strategy_runtime_task(
    spec: ManagedStrategyRuntimeSpawn,
    coordinator: &mut Coordinator,
    shutdown_tx: &broadcast::Sender<()>,
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    dry_run: bool,
    pm_client: Option<&PolymarketClient>,
    pm_ws_url: &str,
    data_plane: Option<Arc<PlatformDataPlane>>,
    observability_pool: Option<PgPool>,
    observability_account_id: &str,
) -> bool {
    let Some(strategy_pm_client) = pm_client.cloned() else {
        warn!(
            agent = %spec.agent_id,
            strategy = spec.strategy_label,
            "managed strategy runtime requested but pm client not configured; skipping"
        );
        return false;
    };

    let strategy_label = spec.strategy_label;
    let agent_id = spec.agent_id;
    let strategy_cmd_rx =
        coordinator.register_agent(agent_id.clone(), spec.domain, spec.risk_params);
    let strategy_shutdown_rx = shutdown_tx.subscribe();
    let strategy_ws_url = pm_ws_url.to_string();
    let strategy_data_plane = data_plane;
    let strategy_observability_pool = observability_pool;
    let strategy_account_id = observability_account_id.to_string();
    let strategy_config_toml = spec.strategy_config_toml;
    let runtime_agent_id = agent_id.clone();

    let jh = tokio::spawn(async move {
        if let Err(e) = run_managed_strategy_runtime(
            strategy_label,
            &runtime_agent_id,
            spec.domain,
            strategy_config_toml,
            dry_run,
            strategy_pm_client,
            strategy_ws_url,
            strategy_data_plane,
            strategy_observability_pool,
            strategy_account_id,
            strategy_cmd_rx,
            strategy_shutdown_rx,
        )
        .await
        {
            error!(
                agent = strategy_label,
                runtime_agent_id = %runtime_agent_id,
                error = %e,
                "managed strategy runtime exited with error"
            );
        }
    });
    agent_handles.push(jh);
    info!(
        agent = %agent_id,
        strategy = strategy_label,
        "managed strategy runtime spawned"
    );
    true
}

pub(super) fn spawn_openclaw_governance_agent(
    config: &PlatformBootstrapConfig,
    freshness: &Arc<crate::platform::DataPlaneFreshness>,
    coordinator: &mut Coordinator,
    handle: &CoordinatorHandle,
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
) {
    let openclaw_enabled = env_bool(
        "PLOY_OPENCLAW__ENABLED",
        config.enable_openclaw || config.openclaw.enabled,
    );
    if !openclaw_enabled {
        return;
    }

    let oc_symbols = vec![config.openclaw.btc_symbol.clone()];
    let oc_binance_ws = Arc::new(BinanceWebSocket::new(oc_symbols));
    oc_binance_ws.set_freshness(Arc::clone(freshness));

    let oc_ws = oc_binance_ws.clone();
    tokio::spawn(async move {
        if let Err(e) = oc_ws.run().await {
            tracing::error!(error = %e, "openclaw binance ws exited");
        }
    });

    let oc_risk_params = AgentRiskParams {
        max_order_value: Decimal::ZERO,
        max_total_exposure: Decimal::ZERO,
        max_unhedged_positions: 0,
        max_daily_loss: Decimal::ZERO,
        allow_overnight: false,
        allowed_markets: vec![],
    };
    let oc_agent_id = config.openclaw.agent_id.clone();
    let cmd_rx = coordinator.register_agent(oc_agent_id.clone(), Domain::Custom(0), oc_risk_params);

    let oc_market_data = BinanceDataPlaneHandle::new(oc_binance_ws);
    let agent = OpenClawAgent::new(config.openclaw.clone(), oc_market_data);
    let ctx = GovernanceContext::new(
        oc_agent_id.clone(),
        Domain::Custom(0),
        handle.clone(),
        cmd_rx,
    );

    let jh = tokio::spawn(async move {
        if let Err(e) = agent.run(ctx).await {
            tracing::error!(agent = "openclaw", error = %e, "openclaw meta-agent exited with error");
        }
    });
    agent_handles.push(jh);
    info!(
        agent_id = %oc_agent_id,
        regime_tick = config.openclaw.regime_tick_secs,
        "openclaw meta-agent spawned"
    );
}

pub(super) async fn prepare_sports_runtime_support(
    config: &PlatformBootstrapConfig,
    app_config: &AppConfig,
    shared_pool: Option<&PgPool>,
    freshness: &Arc<crate::platform::DataPlaneFreshness>,
) -> Result<()> {
    if app_config.nba_comeback.is_none() {
        return Ok(());
    }

    let sports_cfg = config.sports.clone();

    let pool = match shared_pool {
        Some(pool) => pool.clone(),
        None => {
            PgPoolOptions::new()
                .max_connections(app_config.database.max_connections)
                .connect(&app_config.database.url)
                .await?
        }
    };
    spawn_polymarket_trade_persistence_from_collector_targets(
        pool.clone(),
        sports_cfg.agent_id.clone(),
        Domain::Sports,
    );

    {
        let sports_data_plane_config = DataPlaneConfig {
            polymarket_ws_url: app_config.market.ws_url.clone(),
            ..DataPlaneConfig::default()
        };
        let sports_data_plane = Arc::new(PlatformDataPlane::new(
            sports_data_plane_config,
            Arc::clone(freshness),
        ));
        sports_data_plane.start(Vec::new()).await?;
        let sports_pm_ws = sports_data_plane.polymarket_ws().ok_or_else(|| {
            crate::error::PloyError::Validation(
                "sports data plane misconfigured: missing Polymarket WS adapter".to_string(),
            )
        })?;

        let mut sports_desired: HashMap<String, Side> = HashMap::new();
        if let Ok(rows) = sqlx::query_as::<_, (String, Option<String>)>(
            r#"
            SELECT token_id, metadata->>'side'
            FROM collector_token_targets
            WHERE domain = 'SPORTS_NBA'
              AND target_date BETWEEN (CURRENT_DATE - 1) AND (CURRENT_DATE + 1)
              AND (expires_at IS NULL OR expires_at > NOW())
            "#,
        )
        .fetch_all(&pool)
        .await
        {
            for (token_id, side_str) in rows {
                let side = match side_str.as_deref() {
                    Some("DOWN") | Some("NO") => Side::Down,
                    _ => Side::Up,
                };
                sports_desired.insert(token_id, side);
            }
        }

        let initial_count = sports_desired.len();
        if initial_count > 0 {
            sports_pm_ws.reconcile_token_sides(&sports_desired).await;
            info!(
                agent = sports_cfg.agent_id,
                token_count = initial_count,
                "seeded sports PM WS tokens for L2 data collection"
            );
        }

        let refresh_ws = sports_pm_ws.clone();
        let refresh_pool = pool.clone();
        let refresh_agent = sports_cfg.agent_id.clone();
        tokio::spawn(async move {
            let secs = env_u64("PM_SPORTS_COLLECTOR_REFRESH_SECS", 300).max(30);
            let mut tick = tokio::time::interval(Duration::from_secs(secs));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let mut desired: HashMap<String, Side> = HashMap::new();
                if let Ok(rows) = sqlx::query_as::<_, (String, Option<String>)>(
                    r#"
                    SELECT token_id, metadata->>'side'
                    FROM collector_token_targets
                    WHERE domain = 'SPORTS_NBA'
                      AND target_date BETWEEN (CURRENT_DATE - 1) AND (CURRENT_DATE + 1)
                      AND (expires_at IS NULL OR expires_at > NOW())
                    "#,
                )
                .fetch_all(&refresh_pool)
                .await
                {
                    for (token_id, side_str) in rows {
                        let side = match side_str.as_deref() {
                            Some("DOWN") | Some("NO") => Side::Down,
                            _ => Side::Up,
                        };
                        desired.insert(token_id, side);
                    }
                }
                let (_a, _r, _u, total) = refresh_ws.reconcile_token_sides(&desired).await;
                trace!(
                    agent = refresh_agent,
                    total,
                    "refreshed sports PM WS token subscriptions"
                );
            }
        });

        let sports_quote_table_ready = match ensure_clob_quote_ticks_table(&pool).await {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    agent = sports_cfg.agent_id,
                    error = %e,
                    "failed to ensure clob_quote_ticks table; sports quote persistence bridge disabled"
                );
                false
            }
        };
        let sports_orderbook_table_ready = match ensure_clob_orderbook_snapshots_table(&pool).await
        {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    agent = sports_cfg.agent_id,
                    error = %e,
                    "failed to ensure clob_orderbook_snapshots table; sports orderbook persistence bridge disabled"
                );
                false
            }
        };
        if sports_quote_table_ready || sports_orderbook_table_ready {
            let sports_orderbook_levels = env_usize("PM_ORDERBOOK_LEVELS", 20).clamp(1, 200);
            let sports_orderbook_snapshot_ms = match std::env::var("PM_ORDERBOOK_SNAPSHOT_MS") {
                Ok(raw) => raw.parse::<u64>().unwrap_or(0),
                Err(_) => {
                    (env_i64("PM_ORDERBOOK_SNAPSHOT_SECS", 60).max(0) as u64).saturating_mul(1000)
                }
            };
            let sports_orderbook_require_hash_change =
                env_bool("PM_ORDERBOOK_REQUIRE_HASH_CHANGE", true);
            let sports_pipeline_config = crate::platform::PersistenceConfig {
                clob_quote_min_interval_secs: CLOB_PERSIST_MIN_INTERVAL_SECS,
                clob_orderbook_snapshot_interval_ms: sports_orderbook_snapshot_ms as i64,
                clob_orderbook_max_levels: sports_orderbook_levels,
                clob_orderbook_require_hash_change: sports_orderbook_require_hash_change,
                ..Default::default()
            };
            let sports_pipeline = crate::platform::PersistencePipeline::spawn_with_freshness(
                pool.clone(),
                sports_pipeline_config,
                Some(Arc::clone(freshness)),
            );

            if sports_quote_table_ready {
                if let Some(quote_rx) = sports_data_plane.subscribe_quotes() {
                    sports_pipeline.spawn_bridge(
                        quote_rx,
                        format!("{}.sports_quote", sports_cfg.agent_id),
                        |update| {
                            Some(crate::platform::PersistenceEvent::ClobQuote(
                                crate::platform::ClobQuoteTick {
                                    token_id: update.token_id.clone(),
                                    side: update.side.as_str().to_string(),
                                    best_bid: update.quote.best_bid,
                                    best_ask: update.quote.best_ask,
                                    bid_size: update.quote.bid_size,
                                    ask_size: update.quote.ask_size,
                                    domain: Domain::Sports,
                                    received_at: Utc::now(),
                                },
                            ))
                        },
                    );
                } else {
                    warn!("sports quote bridge unavailable: no quote receiver");
                }
            }

            if sports_orderbook_table_ready {
                if let Some(book_rx) = sports_data_plane.subscribe_books() {
                    sports_pipeline.spawn_bridge(
                        book_rx,
                        format!("{}.sports_orderbook", sports_cfg.agent_id),
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
                                    domain: Domain::Sports,
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
                    warn!("sports orderbook bridge unavailable: no book receiver");
                }
            }
        } else {
            warn!(
                agent = sports_cfg.agent_id,
                "sports persistence tables unavailable; WS persistence bridges disabled"
            );
        }

        info!(
            agent = sports_cfg.agent_id,
            "sports PM WS L2 data collection started"
        );
    }
    Ok(())
}
