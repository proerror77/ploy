pub mod attribution;
pub mod backtest;
pub mod backtesting;
pub mod dataset;
pub mod event_ml;
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
    DatasetArtifacts, DatasetBuildError, DatasetBuildManifest, DatasetBuildStats,
    DatasetLabelContract, DatasetSkipCounts, DatasetSourceWindow, DatasetSplit,
    DatasetSplitArtifactPaths, DatasetSplitAssignment, DatasetSplitCounts,
    DatasetSplitDerivedArtifacts, DatasetSplitPolicy, EventChronologyBuild, EventChronologyKey,
    EventIndexEntry, EventMetadataChronologyInput, EventRootDatasetBuild,
    EventRootDatasetBuildRequest, REPRICING_LABEL_30S, SETTLEMENT_LABEL, SplitBuildError,
    assign_chronological_event_splits, build_canonical_event_chronology, build_event_root_dataset,
    standard_event_root_dataset_artifacts,
};
#[cfg(feature = "polars-export")]
pub use dataset::{
    DatasetExportError, event_index_to_frame, event_summaries_to_frame,
    export_event_root_dataset_parquet, split_assignments_to_frame,
};
pub use event_ml::{
    ArchitectureArtifact, EVENT_ML_ARCHITECTURE_VERSION, EventMlArchitecture, LaneReadiness,
    LaneStatus, LearningLane, LearningLaneId, PhaseId, ReadinessGate, WALK_FORWARD_REPORT_VERSION,
    WalkForwardAggregate, WalkForwardConfig, WalkForwardGate, WalkForwardGateStatus,
    WalkForwardMetric, WalkForwardReadiness, WalkForwardReport, WalkForwardWindow, WorkflowPhase,
    build_walk_forward_report, canonical_event_ml_architecture, event_ml_architecture_markdown,
    gate_matrix, walk_forward_report_markdown,
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
pub use factors_new::{
    AutomlFactorAttribution, FactorMeta, FactorRegistry, register_automl_attributions,
    scan_into_registry,
};
pub use factors_v2::{
    DataHealthReport, DeribitFeatureSnapshot, FactorFamily, FactorObservationV2,
    FactorReviewOptions, FactorReviewV2Report, FactorV2Descriptor, ReviewSide, SingleFactorReview,
    ThreeLayerArchive, build_data_health_report, build_factor_observations_v2,
    build_factor_observations_v2_with_deribit, factor_v2_descriptors,
    format_factor_review_v2_report, review_factors_v2, review_factors_v2_with_deribit,
};
#[cfg(feature = "rl")]
pub use model::rl::{BinaryEventEnv, DqnAgent, Environment, ReplayBuffer};
#[cfg(any(feature = "ml", feature = "rl"))]
pub use model::{RlAgent, StrategyModel, Transition};
pub use ploy_operator_contracts::Regime;
pub use signal::{RegimeRouter, Signal, SignalSource, ThresholdRule};
