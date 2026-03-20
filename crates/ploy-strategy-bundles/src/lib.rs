pub mod bundle;
pub mod runtime;
pub mod signals;

pub use bundle::StrategyBundle;
pub use runtime::emit_intents;
pub use signals::{MarketSignal, SignalConfig};

pub const CRATE_MARKER: &str = "ploy-strategy-bundles";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
