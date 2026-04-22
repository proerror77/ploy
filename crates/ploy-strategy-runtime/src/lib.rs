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
use tracing::info;

pub use ploy_strategy_bundles::RuntimeMode as StrategyRuntimeMode;

pub async fn run_strategy(config: FullConfig, config_path: &str, force_dry_run: bool) {
    let mut runtime_config = config.runtime_config();
    if force_dry_run {
        runtime_config.mode = RuntimeMode::DryRun;
    }

    info!(
        mode = ?runtime_config.mode,
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
            run_live_or_dry_run_entry(&config, &symbols, strategy, runtime_config.clone()).await
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
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    eprintln!("Live and dry-run modes require the `live` feature");
    std::process::exit(1);
}

type RuntimeModeConfig = ploy_strategy_bundles::RuntimeConfig;

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
    use super::{database_unavailable_is_fatal, prepare_feed_symbols};
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
}
