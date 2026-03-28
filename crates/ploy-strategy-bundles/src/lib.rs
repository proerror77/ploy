pub mod bundle;
pub mod engine;
pub mod executor;
pub mod feed;
pub mod runtime;
pub mod signals;
pub mod strategies;
pub mod traits;

pub use bundle::StrategyBundle;
pub use engine::{RuntimeConfig, RuntimeMode, RuntimeResult, StrategyRuntime};
pub use executor::{SimulatedExecutor, SimulatedExecutorConfig};
pub use feed::{HistoricalFeed, LiveFeed};
pub use runtime::emit_intents;
pub use signals::{MarketSignal, SignalConfig};
pub use strategies::DirectionalStrategy;
pub use traits::{
    Executor, ExecutionReport, Feed, MarketUpdate, NullRecorder, Recorder, SignalRecord,
    StrategyDecision, StrategyLogic,
};

pub const CRATE_MARKER: &str = "ploy-strategy-bundles";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
