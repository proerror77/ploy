pub mod fills;
pub mod intents;
pub mod orders;
pub mod pnl;
pub mod positions;
pub mod risk;

pub use fills::{FillLedger, FillRecord};
pub use intents::{IntentPurpose, TradeSide, TradingIntent};
pub use orders::{OrderLedger, OrderRecord, OrderState};
pub use pnl::PnlSnapshot;
pub use positions::{PositionLedger, PositionSnapshot};
pub use risk::{snapshot_from_state, RiskSnapshot};

pub const CRATE_MARKER: &str = "ploy-trading";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
