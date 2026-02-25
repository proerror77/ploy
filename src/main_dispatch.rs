#[cfg(feature = "api")]
use ploy::adapters::start_api_server;
#[cfg(feature = "api")]
use ploy::adapters::PostgresStore;
#[cfg(feature = "api")]
use ploy::api::state::StrategyConfigState;
use ploy::cli::runtime::{Cli, Commands};
#[cfg(feature = "api")]
use ploy::config::AppConfig;
use ploy::error::{PloyError, Result};
#[cfg(feature = "api")]
use std::sync::Arc;
#[cfg(feature = "api")]
use tracing::warn;

pub(crate) async fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Some(Commands::Serve { port }) => {
            run_serve(cli, *port).await?;
        }
        Some(Commands::Account { orders, positions }) => {
            crate::main_runtime::init_logging_simple();
            crate::main_modes::run_account_mode(*orders, *positions).await?;
        }
        Some(Commands::Rpc) => {
            crate::main_runtime::init_logging_simple();
            ploy::cli::rpc::run_rpc(&cli.config).await?;
        }
        Some(Commands::Agent {
            mode,
            market,
            sports_url,
            max_trade,
            max_exposure,
            enable_trading,
            chat,
        }) => {
            crate::main_runtime::init_logging();
            if *enable_trading {
                crate::main_runtime::enforce_coordinator_only_live("ploy agent --enable-trading")?;
            }
            crate::main_agent_mode::run_agent_mode(
                mode,
                market.as_deref(),
                sports_url.as_deref(),
                *max_trade,
                *max_exposure,
                *enable_trading,
                *chat,
            )
            .await?;
        }
        Some(Commands::Dashboard { series, demo }) => {
            if *demo {
                ploy::tui::run_demo().await?;
            } else {
                crate::main_runtime::init_logging();
                ploy::tui::run_dashboard_auto(series.as_deref(), cli.dry_run.unwrap_or(true))
                    .await?;
            }
        }
        Some(Commands::Collect {
            symbols,
            markets,
            duration,
        }) => {
            crate::main_runtime::init_logging();
            crate::main_modes::run_collect_mode(symbols, markets.as_deref(), *duration).await?;
        }
        Some(Commands::OrderbookHistory {
            asset_ids,
            start_ms,
            end_ms,
            lookback_secs,
            levels,
            sample_ms,
            limit,
            max_pages,
            base_url,
            resume_from_db,
        }) => {
            crate::main_runtime::init_logging();
            crate::main_modes::run_orderbook_history_mode(
                &cli.config,
                asset_ids,
                *start_ms,
                *end_ms,
                *lookback_secs,
                *levels,
                *sample_ms,
                *limit,
                *max_pages,
                base_url,
                *resume_from_db,
            )
            .await?;
        }
        Some(Commands::Crypto(crypto_cmd)) => {
            crate::main_runtime::init_logging();
            crate::main_commands::crypto::run_crypto_command(crypto_cmd).await?;
        }
        Some(Commands::Sports(sports_cmd)) => {
            crate::main_runtime::init_logging();
            crate::main_commands::sports::run_sports_command(sports_cmd).await?;
        }
        Some(Commands::Strategy(strategy_cmd)) => {
            crate::main_runtime::init_logging();
            strategy_cmd.clone().run().await?;
        }
        #[cfg(feature = "rl")]
        Some(Commands::Rl(rl_cmd)) => {
            crate::main_runtime::init_logging();
            crate::main_commands::rl::run_rl_command(rl_cmd).await?;
        }
        Some(Commands::Claim {
            check_only,
            min_size,
            interval,
        }) => {
            crate::main_runtime::init_logging();
            crate::main_modes::run_claimer(*check_only, *min_size, *interval).await?;
        }
        Some(Commands::History {
            limit,
            symbol,
            stats_only,
            open_only,
        }) => {
            crate::main_modes::run_history(*limit, symbol.clone(), *stats_only, *open_only).await?;
        }
        Some(Commands::Paper {
            symbols,
            min_vol_edge,
            min_price_edge,
            log_file,
            stats_interval,
        }) => {
            crate::main_runtime::init_logging();
            crate::main_modes::run_paper_trading(
                symbols.clone(),
                *min_vol_edge,
                *min_price_edge,
                log_file.clone(),
                *stats_interval,
            )
            .await?;
        }
        Some(Commands::Platform {
            action,
            crypto,
            sports,
            dry_run,
            pause,
            resume,
        }) => {
            crate::main_runtime::init_logging();
            crate::main_modes::run_platform_mode(
                action,
                *crypto,
                *sports,
                *dry_run,
                pause.clone(),
                resume.clone(),
                cli,
            )
            .await?;
        }
        Some(Commands::Pm(pm_cli)) => {
            crate::main_runtime::init_logging_simple();
            let mut pm_args = pm_cli.args.clone();
            // Merge top-level --dry-run if set
            pm_args.dry_run = pm_args.dry_run || cli.dry_run.unwrap_or(false);
            ploy::cli::pm::run(pm_cli.command.clone(), &pm_args)
                .await
                .map_err(|e| PloyError::Validation(format!("pm command failed: {e}")))?;
        }
        None => {
            return Err(PloyError::Validation(
                "no command provided; use `ploy platform start` to launch coordinator + agents"
                    .to_string(),
            ));
        }
    }

    Ok(())
}

async fn run_serve(cli: &Cli, port: Option<u16>) -> Result<()> {
    crate::main_runtime::init_logging_simple();

    #[cfg(feature = "api")]
    {
        use rust_decimal::prelude::ToPrimitive;

        let config = AppConfig::load_from(&cli.config).unwrap_or_else(|e| {
            warn!("Failed to load config: {}, using defaults", e);
            AppConfig::default_config(true, "btc-price-series-15m")
        });

        let api_port = port
            .or_else(|| std::env::var("API_PORT").ok().and_then(|v| v.parse().ok()))
            .or(config.api_port)
            .unwrap_or(8081);

        let store =
            PostgresStore::new(&config.database.url, config.database.max_connections).await?;

        let api_config = StrategyConfigState {
            symbols: vec![config.market.market_slug.clone()],
            min_move: config.strategy.move_pct.to_f64().unwrap_or(0.0),
            max_entry: config.strategy.sum_target.to_f64().unwrap_or(1.0),
            shares: i32::try_from(config.strategy.shares).unwrap_or(i32::MAX),
            predictive: false,
            exit_edge_floor: None,
            exit_price_band: None,
            time_decay_exit_secs: None,
            liquidity_exit_spread_bps: None,
        };

        start_api_server(Arc::new(store), api_port, api_config).await?;
        Ok(())
    }

    #[cfg(not(feature = "api"))]
    {
        let _ = (cli, port);
        Err(PloyError::Validation(
            "API feature not enabled. Rebuild with --features api".to_string(),
        ))
    }
}
