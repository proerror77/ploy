#[cfg(feature = "backtest-db")]
mod backtest;
#[cfg(feature = "live")]
mod live;
#[cfg(feature = "db-recorder")]
mod recording;
#[cfg(feature = "replay")]
mod replay;

#[cfg(feature = "backtest-db")]
use backtest::run_backtest_entry;
#[cfg(feature = "live")]
use live::run_live_or_dry_run_entry;
#[cfg(any(
    not(feature = "backtest-db"),
    not(feature = "live"),
    not(feature = "replay")
))]
use ploy_strategy_bundles::StrategyLogic;
use ploy_strategy_bundles::{FullConfig, RuntimeMode};
#[cfg(feature = "replay")]
use replay::run_replay_entry;
use rust_decimal::Decimal;
use serde_json::json;
use std::path::Path;
use tracing::info;

pub use ploy_strategy_bundles::RuntimeMode as StrategyRuntimeMode;

pub async fn run_strategy(config: FullConfig, config_path: &str, force_dry_run: bool) {
    run_strategy_with_deployment_id(config, config_path, force_dry_run, None).await;
}

pub async fn run_strategy_with_deployment_id(
    config: FullConfig,
    config_path: &str,
    force_dry_run: bool,
    deployment_id: Option<String>,
) {
    run_strategy_with_deployment_id_and_output(
        config,
        config_path,
        force_dry_run,
        deployment_id,
        None,
    )
    .await;
}

pub async fn run_strategy_with_deployment_id_and_output(
    config: FullConfig,
    config_path: &str,
    force_dry_run: bool,
    deployment_id: Option<String>,
    output_json: Option<&Path>,
) {
    let mut runtime_config = config.runtime_config();
    if force_dry_run {
        runtime_config.mode = RuntimeMode::DryRun;
    }
    let deployment_id = resolve_deployment_id(deployment_id);
    let deployment_label = deployment_id.clone();
    if matches!(runtime_config.mode, RuntimeMode::Live | RuntimeMode::DryRun)
        && deployment_id.is_none()
    {
        eprintln!(
            "Live and dry-run strategy runtime requires --deployment-id or PLOY_DEPLOYMENT_ID"
        );
        std::process::exit(1);
    }

    info!(
        mode = ?runtime_config.mode,
        deployment_id = deployment_id.as_deref().unwrap_or(""),
        config = %config_path,
        symbols = ?config.strategy.symbols,
        "ploy-runner starting",
    );

    let symbols = prepare_feed_symbols(runtime_config.mode, &config.strategy.symbols);
    let strategy = ploy_strategy_bundles::build_strategy(&config);

    let (result, snapshot) = match runtime_config.mode {
        RuntimeMode::Backtest => {
            run_backtest_entry(&config, &symbols, strategy, runtime_config.clone()).await
        }
        RuntimeMode::Replay => run_replay_entry(&config, strategy, runtime_config.clone()).await,
        RuntimeMode::Live | RuntimeMode::DryRun => {
            run_live_or_dry_run_entry(
                &config,
                &symbols,
                strategy,
                runtime_config.clone(),
                deployment_id.expect("deployment_id checked for live/dry-run"),
            )
            .await
        }
    };

    info!(
        updates = result.updates_processed,
        intents = result.intents_submitted,
        fills = result.fills_recorded,
        net_pnl = %result.pnl.net_pnl(),
        elapsed = format!("{:.1}s", result.elapsed_secs),
        "ploy-runner finished",
    );

    if matches!(
        runtime_config.mode,
        RuntimeMode::Backtest | RuntimeMode::Replay
    ) {
        let cashflow = snapshot.fill_cashflow_summary();
        let roi_on_deployed_capital = cashflow
            .roi_on_deployed_capital()
            .map(|roi| format!("{}%", (roi * Decimal::from(100)).round_dp(2)))
            .unwrap_or_else(|| "n/a".to_string());

        info!(
            buy_shares = %cashflow.buy_shares,
            sell_shares = %cashflow.sell_shares,
            deployed_capital = %cashflow.deployed_capital(),
            gross_sell_proceeds = %cashflow.gross_sell_proceeds,
            fees = %cashflow.total_fees,
            roi_on_deployed_capital = %roi_on_deployed_capital,
            "Replay/backtest cashflow summary",
        );
    }

    if let Some(output_path) = output_json {
        write_strategy_evaluation(
            output_path,
            config_path,
            deployment_label.as_deref(),
            &result,
            &snapshot,
        );
    }
}

fn write_strategy_evaluation(
    output_path: &Path,
    config_path: &str,
    deployment_id: Option<&str>,
    result: &ploy_strategy_bundles::RuntimeResult,
    snapshot: &ploy_trading::TradingRuntimeSnapshot,
) {
    let cashflow = snapshot.fill_cashflow_summary();
    let artifact = json!({
        "schema_version": 1,
        "artifact_type": "strategy_runtime_evaluation",
        "producer": "new-ploy-runner",
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "config_path": config_path,
        "deployment_id": deployment_id,
        "mode": format!("{:?}", result.mode),
        "result": {
            "updates_processed": result.updates_processed,
            "intents_submitted": result.intents_submitted,
            "fills_recorded": result.fills_recorded,
            "realized_pnl": result.pnl.realized_pnl,
            "unrealized_pnl": result.pnl.unrealized_pnl,
            "total_fees": result.pnl.total_fees,
            "net_pnl": result.pnl.net_pnl(),
            "risk": &result.risk,
            "elapsed_secs": result.elapsed_secs,
        },
        "cashflow": {
            "buy_shares": cashflow.buy_shares,
            "sell_shares": cashflow.sell_shares,
            "gross_buy_cost": cashflow.gross_buy_cost,
            "gross_sell_proceeds": cashflow.gross_sell_proceeds,
            "total_fees": cashflow.total_fees,
            "deployed_capital": cashflow.deployed_capital(),
            "net_pnl": cashflow.net_pnl(),
            "roi_on_deployed_capital": cashflow.roi_on_deployed_capital(),
        },
        "snapshot_counts": {
            "intents": snapshot.intents.len(),
            "orders": snapshot.orders.len(),
            "fills": snapshot.fills.len(),
            "positions": snapshot.positions.len(),
        },
    });

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "Failed to create output JSON directory {}: {error}",
                    parent.display()
                );
                std::process::exit(1);
            }
        }
    }

    match serde_json::to_vec_pretty(&artifact)
        .map(|mut bytes| {
            bytes.push(b'\n');
            bytes
        })
        .and_then(|bytes| std::fs::write(output_path, bytes).map_err(serde_json::Error::io))
    {
        Ok(()) => info!(path = %output_path.display(), "Wrote strategy runtime evaluation JSON"),
        Err(error) => {
            eprintln!(
                "Failed to write strategy runtime evaluation JSON {}: {error}",
                output_path.display()
            );
            std::process::exit(1);
        }
    }
}

#[cfg(not(feature = "backtest-db"))]
async fn run_backtest_entry(
    _config: &FullConfig,
    _symbols: &[String],
    _strategy: Box<dyn StrategyLogic>,
    _runtime_config: RuntimeModeConfig,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    eprintln!("Backtest mode requires the `backtest-db` feature");
    std::process::exit(1);
}

#[cfg(not(feature = "replay"))]
async fn run_replay_entry(
    _config: &FullConfig,
    _strategy: Box<dyn StrategyLogic>,
    _runtime_config: RuntimeModeConfig,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    eprintln!("Replay mode requires the `replay` feature");
    std::process::exit(1);
}

#[cfg(not(feature = "live"))]
async fn run_live_or_dry_run_entry(
    _config: &FullConfig,
    _symbols: &[String],
    _strategy: Box<dyn StrategyLogic>,
    _runtime_config: RuntimeModeConfig,
    _deployment_id: String,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    eprintln!("Live and dry-run modes require the `live` feature");
    std::process::exit(1);
}

type RuntimeModeConfig = ploy_strategy_bundles::RuntimeConfig;

fn resolve_deployment_id(cli_value: Option<String>) -> Option<String> {
    cli_value
        .or_else(|| std::env::var("PLOY_DEPLOYMENT_ID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn prepare_feed_symbols(mode: RuntimeMode, strategy_symbols: &[String]) -> Vec<String> {
    match mode {
        RuntimeMode::Backtest | RuntimeMode::Replay => strategy_symbols.to_vec(),
        RuntimeMode::Live | RuntimeMode::DryRun => strategy_symbols.to_vec(),
    }
}

#[cfg(any(feature = "live", test))]
fn database_unavailable_is_fatal(mode: RuntimeMode, database_url_present: bool) -> bool {
    database_url_present && matches!(mode, RuntimeMode::Live | RuntimeMode::DryRun)
}

#[cfg(test)]
mod tests {
    use super::{database_unavailable_is_fatal, prepare_feed_symbols, resolve_deployment_id};
    use ploy_strategy_bundles::RuntimeMode;

    #[test]
    fn keeps_strategy_symbols_canonical_for_live_feeds() {
        let symbols = vec!["BTCUSDT".to_string(), "ethusdt".to_string()];
        let prepared = prepare_feed_symbols(RuntimeMode::DryRun, &symbols);
        assert_eq!(prepared, vec!["BTCUSDT".to_string(), "ethusdt".to_string()]);
    }

    #[test]
    fn treats_live_and_dry_run_db_connection_failures_as_fatal_when_configured() {
        assert!(database_unavailable_is_fatal(RuntimeMode::Live, true));
        assert!(database_unavailable_is_fatal(RuntimeMode::DryRun, true));
        assert!(!database_unavailable_is_fatal(RuntimeMode::Backtest, true));
        assert!(!database_unavailable_is_fatal(RuntimeMode::Replay, true));
        assert!(!database_unavailable_is_fatal(RuntimeMode::DryRun, false));
    }

    #[test]
    fn trims_empty_deployment_ids() {
        assert_eq!(
            resolve_deployment_id(Some(" dep-1 ".to_string())).as_deref(),
            Some("dep-1")
        );
        assert!(resolve_deployment_id(Some(" ".to_string())).is_none());
    }
}
