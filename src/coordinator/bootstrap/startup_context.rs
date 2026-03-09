use super::*;

pub(super) struct BootstrapStartupContext {
    pub(super) exchange_client: Arc<dyn crate::exchange::ExchangeClient>,
    pub(super) pm_client: Option<PolymarketClient>,
    pub(super) account_id: String,
    pub(super) runtime_crypto_targets: strategy_deployments::RuntimeCryptoStrategyTargets,
    pub(super) allowed_domains: HashSet<Domain>,
    pub(super) shared_pool: Option<PgPool>,
}

pub(super) async fn initialize_startup_context(
    config: &PlatformBootstrapConfig,
    app_config: &AppConfig,
) -> Result<BootstrapStartupContext> {
    let exchange_kind = parse_exchange_kind(&app_config.execution.exchange)?;
    let exchange_client = build_exchange_client(app_config, config.dry_run).await?;
    let non_pm_builtin_agents_enabled = exchange_kind != ExchangeKind::Polymarket
        && (config.enable_crypto || config.enable_sports || config.enable_politics);
    if non_pm_builtin_agents_enabled {
        return Err(crate::error::PloyError::Validation(format!(
            "execution.exchange={} is not yet supported with built-in runtime loops (crypto managed + legacy compatibility paths). Disable those runtime loops or set execution.exchange=polymarket",
            exchange_kind
        )));
    }

    let needs_polymarket_client =
        config.enable_crypto || config.enable_sports || config.enable_politics;
    let pm_client = if needs_polymarket_client {
        let rest_url = app_config
            .market
            .exchange_rest_url
            .as_deref()
            .unwrap_or(&app_config.market.rest_url);

        if config.dry_run {
            Some(PolymarketClient::new(rest_url, true)?)
        } else {
            let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
            let funder = std::env::var("POLYMARKET_FUNDER").ok();
            if let Some(funder_addr) = funder {
                Some(
                    PolymarketClient::new_authenticated_proxy(rest_url, wallet, &funder_addr, true)
                        .await?,
                )
            } else {
                Some(PolymarketClient::new_authenticated(rest_url, wallet, true).await?)
            }
        }
    } else {
        None
    };

    let account_id = if app_config.account.id.trim().is_empty() {
        "default".to_string()
    } else {
        app_config.account.id.clone()
    };
    let runtime_crypto_targets =
        collect_runtime_crypto_strategy_targets(&account_id, config.dry_run);
    #[cfg(feature = "rl")]
    let crypto_rl_policy_enabled = config.managed_crypto.enable_rl_policy;
    #[cfg(not(feature = "rl"))]
    let crypto_rl_policy_enabled = false;

    info!(
        account_id = %account_id,
        crypto = config.enable_crypto,
        crypto_momentum = config.enable_crypto_momentum,
        crypto_pattern_memory = config.enable_crypto_pattern_memory,
        crypto_split_arb = config.enable_crypto_split_arb,
        crypto_lob_ml = config.managed_crypto.enable_lob_ml,
        crypto_rl_policy = crypto_rl_policy_enabled,
        sports = config.enable_sports,
        politics = config.enable_politics,
        economics = config.enable_economics,
        openclaw = config.enable_openclaw || config.openclaw.enabled,
        exchange = %exchange_kind,
        dry_run = config.dry_run,
        "starting multi-agent platform"
    );
    if config.enable_economics {
        warn!(
            "economics domain enabled, but no built-in economics agent is registered; coordinator-level risk and allocator gates remain active"
        );
    }

    let mut allowed_domains: HashSet<Domain> = HashSet::new();
    if config.enable_crypto {
        allowed_domains.insert(Domain::Crypto);
    }
    if config.enable_sports {
        allowed_domains.insert(Domain::Sports);
    }
    if config.enable_politics {
        allowed_domains.insert(Domain::Politics);
    }

    let db_required = env_bool(
        "PLOY_DB_REQUIRED",
        env_bool("PLOY_REQUIRE_DB", !app_config.dry_run.enabled),
    );
    let shared_pool = match PgPoolOptions::new()
        .max_connections(app_config.database.max_connections)
        .connect(&app_config.database.url)
        .await
    {
        Ok(pool) => Some(pool),
        Err(e) => {
            if db_required {
                return Err(crate::error::PloyError::Internal(format!(
                    "database connection is required but failed at startup: {}",
                    e
                )));
            }
            warn!(
                error = %e,
                "failed to connect DB at startup; continuing without shared pool"
            );
            None
        }
    };

    Ok(BootstrapStartupContext {
        exchange_client,
        pm_client,
        account_id,
        runtime_crypto_targets,
        allowed_domains,
        shared_pool,
    })
}
