pub mod attribution;
pub mod backtest;
pub mod backtesting;
pub mod dataset;
pub mod factors;
pub mod factors_new;
pub mod factors_v2;
#[cfg(any(feature = "ml", feature = "rl"))]
pub mod model;
pub mod replay;
pub mod signal;

pub use backtesting::{BacktestReport, run_backtest};
pub use dataset::{
    CANONICAL_REGIME_VERSION, ChronologyAnchor, ChronologyOrdering, DATASET_MANIFEST_VERSION,
    DatasetArtifacts, DatasetBuildManifest, DatasetBuildStats, DatasetLabelContract,
    DatasetSkipCounts, DatasetSourceWindow, DatasetSplit, DatasetSplitArtifactPaths,
    DatasetSplitAssignment, DatasetSplitCounts, DatasetSplitPolicy, EventChronologyBuild,
    EventChronologyKey, EventIndexEntry, EventMetadataChronologyInput, REPRICING_LABEL_30S,
    SETTLEMENT_LABEL, SplitBuildError, assign_chronological_event_splits,
    build_canonical_event_chronology,
};
pub use factors::{
    AggregatedFactorMetric, EventFactorSummary, FactorMetric, FactorObservation,
    ResearchLobSnapshot, aggregate_factor_metrics, build_event_summaries,
    build_factor_observations, build_factor_observations_with_lob, factor_metrics,
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
pub use attribution::{AttributionReport, RegimePnl, factor_pnl, regime_pnl};
pub use backtest::{BacktestMetrics, SimulatedFill, run_binary_backtest};
pub use factors_new::{FactorMeta, FactorRegistry, scan_into_registry};
pub use factors_v2::{
    DataHealthReport, DeribitFeatureSnapshot, ExecutableEvBucketSummary, FactorFamily,
    FactorObservationV2, FactorReviewOptions, FactorReviewV2Report, FactorV2Descriptor, ReviewSide,
    SingleFactorReview, ThreeLayerArchive, build_data_health_report, build_factor_observations_v2,
    build_factor_observations_v2_with_deribit, factor_v2_descriptors,
    format_factor_review_v2_report, review_factors_v2, review_factors_v2_with_deribit,
};
#[cfg(feature = "rl")]
pub use model::rl::{BinaryEventEnv, DqnAgent, Environment, ReplayBuffer};
#[cfg(any(feature = "ml", feature = "rl"))]
pub use model::{RlAgent, StrategyModel, Transition};
pub use ploy_operator_contracts::Regime;
pub use signal::{RegimeRouter, Signal, SignalSource, ThresholdRule};
