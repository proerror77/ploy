pub mod attribution;
pub mod backtest;
pub mod backtesting;
pub mod dataset;
pub mod factors;
pub mod factors_new;
#[cfg(any(feature = "ml", feature = "rl"))]
pub mod model;
pub mod replay;
pub mod signal;

pub use backtesting::{run_backtest, BacktestReport};
pub use dataset::{
    CANONICAL_REGIME_VERSION, ChronologyAnchor, ChronologyOrdering, DATASET_MANIFEST_VERSION,
    DatasetArtifacts, DatasetBuildError, DatasetBuildManifest, DatasetBuildStats,
    DatasetLabelContract, DatasetSkipCounts, DatasetSourceWindow, DatasetSplit,
    DatasetSplitArtifactPaths, DatasetSplitAssignment, DatasetSplitCounts,
    DatasetSplitDerivedArtifacts, DatasetSplitPolicy, EventChronologyBuild, EventChronologyKey,
    EventIndexEntry, EventMetadataChronologyInput, EventRootDatasetBuild,
    EventRootDatasetBuildRequest, REPRICING_LABEL_30S, SETTLEMENT_LABEL, SplitBuildError,
    assign_chronological_event_splits, build_canonical_event_chronology,
    build_event_root_dataset, standard_event_root_dataset_artifacts,
};
pub use factors::{
    aggregate_factor_metrics, build_event_summaries, build_factor_observations,
    build_factor_observations_with_lob, factor_metrics, AggregatedFactorMetric, EventFactorSummary,
    FactorMetric, FactorObservation, ResearchLobSnapshot,
};
#[cfg(feature = "polars-export")]
pub use factors::{export_observations_parquet, observations_to_frame};
#[cfg(feature = "db")]
pub use factors::{load_research_lob_snapshots, load_research_lob_snapshots_sampled};
pub use replay::replay_fills;

pub const CRATE_MARKER: &str = "ploy-research";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}

// New layered pipeline exports.
//
// Keep `Regime` as a single root-level export from operator contracts. The
// factor registry uses the same type internally, but does not re-export its own
// `factors_new::Regime` alias.
pub use attribution::{factor_pnl, regime_pnl, AttributionReport, RegimePnl};
pub use backtest::{run_binary_backtest, BacktestMetrics, SimulatedFill};
pub use factors_new::{scan_into_registry, FactorMeta, FactorRegistry};
#[cfg(feature = "rl")]
pub use model::rl::{BinaryEventEnv, DqnAgent, Environment, ReplayBuffer};
#[cfg(any(feature = "ml", feature = "rl"))]
pub use model::{RlAgent, StrategyModel, Transition};
pub use ploy_operator_contracts::Regime;
pub use signal::{RegimeRouter, Signal, SignalSource, ThresholdRule};
