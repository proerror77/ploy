pub mod attribution;
pub mod backtest;
pub mod backtesting;
pub mod dataset;
pub mod event_ml;
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
#[cfg(feature = "polars-export")]
pub use dataset::{
    DatasetExportError, event_index_to_frame, event_summaries_to_frame,
    export_event_root_dataset_parquet, split_assignments_to_frame,
};
pub use event_ml::{
    build_walk_forward_report, canonical_event_ml_architecture, event_ml_architecture_markdown,
    gate_matrix, walk_forward_report_markdown, ArchitectureArtifact, EventMlArchitecture,
    LaneReadiness, LaneStatus, LearningLane, LearningLaneId, PhaseId, ReadinessGate,
    WalkForwardAggregate, WalkForwardConfig, WalkForwardGate, WalkForwardGateStatus,
    WalkForwardMetric, WalkForwardReadiness, WalkForwardReport, WalkForwardWindow, WorkflowPhase,
    EVENT_ML_ARCHITECTURE_VERSION, WALK_FORWARD_REPORT_VERSION,
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
pub use factors_new::{
    register_automl_attributions, scan_into_registry, AutomlFactorAttribution, FactorMeta,
    FactorRegistry,
};
#[cfg(feature = "rl")]
pub use model::rl::{BinaryEventEnv, DqnAgent, Environment, ReplayBuffer};
#[cfg(any(feature = "ml", feature = "rl"))]
pub use model::{RlAgent, StrategyModel, Transition};
pub use ploy_operator_contracts::Regime;
pub use signal::{RegimeRouter, Signal, SignalSource, ThresholdRule};
