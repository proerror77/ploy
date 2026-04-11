pub mod bundle;
pub mod config;
pub mod engine;
pub mod executor;
pub mod feed;
pub mod recorder;
pub mod runtime;
pub mod signals;
pub mod strategies;
pub mod traits;

pub use bundle::StrategyBundle;
pub use config::FullConfig;
pub use engine::{RuntimeConfig, RuntimeMode, RuntimeResult, StrategyRuntime};
pub use executor::{CallbackExecutor, SimulatedExecutor, SimulatedExecutorConfig};
pub use feed::{
    HistoricalFeed, LiveFeed, RecordedFeed, RecordedFeedError, RecordedMarketUpdate, RecordingFeed,
};
pub use recorder::BufferedRecorder;
pub use runtime::emit_intents;
pub use signals::{MarketSignal, SignalConfig};
pub use strategies::DirectionalStrategy;
pub use strategies::BayesianDirectionalStrategy;
pub use strategies::MeanReversionStrategy;
pub use traits::{
    ExecutionReport, Executor, Feed, MarketUpdate, NullRecorder, Recorder, SignalRecord,
    StrategyDecision, StrategyLogic,
};

pub const CRATE_MARKER: &str = "ploy-strategy-bundles";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
