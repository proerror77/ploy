use super::*;
use crate::agent_runtime::AgentRiskParams;

pub(super) struct CoordinatorRuntimeBootstrap {
    pub(super) coordinator: Coordinator,
    pub(super) handle: CoordinatorHandle,
    pub(super) api_handle: Option<tokio::task::JoinHandle<crate::error::Result<()>>>,
}

pub(super) async fn initialize_coordinator_runtime(
    config: &PlatformBootstrapConfig,
    app_config: &AppConfig,
    exchange_client: Arc<dyn crate::exchange::ExchangeClient>,
    account_id: &str,
    allowed_domains: HashSet<Domain>,
    shared_pool: Option<&PgPool>,
) -> Result<CoordinatorRuntimeBootstrap> {
    let exec_config = app_config.execution.clone();
    let mut executor_builder = OrderExecutor::new_with_exchange(exchange_client, exec_config);
    if let Some(pool) = shared_pool {
        let idem_store = PostgresStore::from_pool(pool.clone());
        let idem_mgr = Arc::new(IdempotencyManager::new_with_account(
            idem_store,
            account_id.to_string(),
        ));
        executor_builder = executor_builder.with_idempotency(idem_mgr.clone());
        info!("order executor idempotency enabled");

        let cleanup_mgr = idem_mgr.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                match cleanup_mgr.cleanup_expired().await {
                    Ok(n) if n > 0 => info!("idempotency cleanup: removed {} expired keys", n),
                    Err(e) => warn!("idempotency cleanup error: {}", e),
                    _ => {}
                }
            }
        });
    } else {
        warn!("order executor idempotency disabled (no database connection)");
    }
    let executor = Arc::new(executor_builder);

    let mut coordinator = Coordinator::new(
        config.coordinator.clone(),
        executor,
        account_id.to_string(),
        allowed_domains,
    );
    if let Some(pool) = shared_pool {
        let mut run_sqlx_migrations = env_bool("PLOY_RUN_SQLX_MIGRATIONS", true);
        let require_sqlx_migrations = env_bool("PLOY_REQUIRE_SQLX_MIGRATIONS", true);
        if require_sqlx_migrations && !run_sqlx_migrations {
            warn!(
                "PLOY_RUN_SQLX_MIGRATIONS=false but PLOY_REQUIRE_SQLX_MIGRATIONS=true; forcing migrations"
            );
            run_sqlx_migrations = true;
        }
        let require_startup_schema =
            env_bool("PLOY_REQUIRE_STARTUP_SCHEMA", !app_config.dry_run.enabled);
        let require_runtime_restore = env_bool(
            "PLOY_REQUIRE_RUNTIME_STATE_RESTORE",
            !app_config.dry_run.enabled,
        );
        let migration_store = PostgresStore::from_pool(pool.clone());
        if run_sqlx_migrations {
            if let Err(e) = migration_store.migrate().await {
                if require_sqlx_migrations {
                    return Err(e);
                }
                warn!(
                    error = %e,
                    "sqlx migration runner failed at startup; continuing due to PLOY_REQUIRE_SQLX_MIGRATIONS=false"
                );
            }
        } else {
            info!("sqlx migration runner skipped at startup (PLOY_RUN_SQLX_MIGRATIONS=false)");
        }
        ensure_schema_repairs(pool).await?;
        if let Err(e) = ensure_accounts_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure accounts table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure accounts table");
        } else if let Err(e) =
            upsert_account_from_config(pool, account_id, &app_config.account).await
        {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to upsert account metadata: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to upsert account metadata");
        }
        if let Err(e) = ensure_coordinator_governance_policies_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure coordinator_governance_policies table: {}",
                    e
                )));
            }
            warn!(
                error = %e,
                "failed to ensure coordinator_governance_policies table; governance persistence disabled"
            );
        } else if let Err(e) = ensure_coordinator_governance_policy_history_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure coordinator_governance_policy_history table: {}",
                    e
                )));
            }
            warn!(
                error = %e,
                "failed to ensure coordinator_governance_policy_history table; governance history persistence disabled"
            );
        } else {
            coordinator.set_governance_store_pool(pool.clone());
            if let Err(e) = coordinator.load_persisted_governance_policy().await {
                if require_startup_schema {
                    return Err(crate::error::PloyError::Internal(format!(
                        "failed to restore coordinator governance policy: {}",
                        e
                    )));
                }
                warn!(
                    error = %e,
                    "failed to restore coordinator governance policy from DB"
                );
            }
        }
        if let Err(e) = ensure_agent_order_executions_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure agent_order_executions table: {}",
                    e
                )));
            }
            warn!(
                error = %e,
                "failed to ensure agent_order_executions table; execution logging disabled"
            );
        } else {
            coordinator.set_execution_log_pool(pool.clone());
            if let Err(e) = coordinator.restore_runtime_state_from_execution_log().await {
                if require_runtime_restore {
                    return Err(crate::error::PloyError::Internal(format!(
                        "failed to restore coordinator runtime state from execution log: {}",
                        e
                    )));
                }
                warn!(
                    error = %e,
                    "failed to restore coordinator runtime state from execution log"
                );
            }
        }
        if let Err(e) = ensure_strategy_observability_tables(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure strategy observability tables: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure strategy observability tables");
        }
        if let Err(e) = ensure_pm_market_metadata_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure pm_market_metadata table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure pm_market_metadata table");
        }
        if let Err(e) = ensure_pm_token_settlements_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure pm_token_settlements table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure pm_token_settlements table");
        }
        if let Err(e) = ensure_risk_runtime_state_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure risk_runtime_state table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure risk_runtime_state table");
        } else if let Err(e) = coordinator.restore_risk_runtime_state().await {
            warn!(error = %e, "failed to restore risk runtime state");
        }
        if config.enable_crypto {
            if let Err(e) = ensure_clob_trade_alerts_table(pool).await {
                if require_startup_schema {
                    return Err(crate::error::PloyError::Internal(format!(
                        "failed to ensure clob_trade_alerts table: {}",
                        e
                    )));
                }
                warn!(
                    error = %e,
                    "failed to ensure clob_trade_alerts table at startup"
                );
            }
        }
    }

    let ingress_agents = std::env::var("PLOY_EXTERNAL_INGRESS_AGENT_IDS")
        .unwrap_or_else(|_| "openclaw_rpc,sidecar".to_string());
    for agent_id in ingress_agents
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        coordinator
            .authorize_external_agent(agent_id, AgentRiskParams::conservative())
            .await;
    }
    let handle = coordinator.handle();
    let _global_state = coordinator.global_state();

    #[cfg(feature = "api")]
    let api_handle = {
        use crate::adapters::{start_api_server_platform_background, PostgresStore};
        use crate::ai_clients::grok::GrokClient;
        use crate::api::state::StrategyConfigState;

        let api_port = std::env::var("API_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(8081);

        let grok_client = std::env::var("GROK_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .and_then(|_| match GrokClient::from_env() {
                Ok(client) => {
                    info!("Grok client initialized for sidecar endpoints");
                    Some(Arc::new(client))
                }
                Err(e) => {
                    warn!(error = %e, "failed to initialize Grok client");
                    None
                }
            });

        if let Some(pool) = shared_pool {
            let store = Arc::new(PostgresStore::from_pool(pool.clone()));
            let api_config = StrategyConfigState {
                symbols: vec![],
                min_move: 0.0,
                max_entry: 1.0,
                shares: 0,
                predictive: false,
                exit_edge_floor: None,
                exit_price_band: None,
                time_decay_exit_secs: None,
                liquidity_exit_spread_bps: None,
            };

            match start_api_server_platform_background(
                store,
                api_port,
                api_config,
                Some(handle.clone()),
                grok_client,
                account_id.to_string(),
                config.dry_run,
            )
            .await
            {
                Ok(handle) => {
                    info!(
                        port = api_port,
                        "API server started in platform mode with sidecar endpoints"
                    );
                    Some(handle)
                }
                Err(e) => {
                    warn!(error = %e, "API server failed to start");
                    None
                }
            }
        } else {
            warn!("API server not started: no database connection");
            None
        }
    };
    #[cfg(not(feature = "api"))]
    let api_handle: Option<tokio::task::JoinHandle<crate::error::Result<()>>> = None;

    Ok(CoordinatorRuntimeBootstrap {
        coordinator,
        handle,
        api_handle,
    })
}
