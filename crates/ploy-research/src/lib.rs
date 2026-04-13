pub mod backtesting;
pub mod factors;
pub mod replay;

pub use backtesting::{run_backtest, BacktestReport};
pub use factors::{
    build_event_summaries, build_factor_observations, factor_metrics, observations_to_frame,
    EventFactorSummary, FactorMetric, FactorObservation,
};
pub use replay::replay_fills;

pub const CRATE_MARKER: &str = "ploy-research";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
