pub mod backtesting;
pub mod factors;
pub mod replay;
pub mod data;
pub mod factors_new;
pub mod signal;
pub mod backtest;
pub mod attribution;
pub mod model;

pub use backtesting::{run_backtest, BacktestReport};
pub use factors::{
    aggregate_factor_metrics, build_event_summaries, build_factor_observations,
    build_factor_observations_with_lob, factor_metrics, load_research_lob_snapshots,
    load_research_lob_snapshots_sampled, observations_to_frame, AggregatedFactorMetric,
    EventFactorSummary, FactorMetric, FactorObservation, ResearchLobSnapshot,
};
pub use replay::replay_fills;

pub const CRATE_MARKER: &str = "ploy-research";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
