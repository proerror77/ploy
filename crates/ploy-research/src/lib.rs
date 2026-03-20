pub mod backtesting;
pub mod replay;

pub use backtesting::{run_backtest, BacktestReport};
pub use replay::replay_fills;

pub const CRATE_MARKER: &str = "ploy-research";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
