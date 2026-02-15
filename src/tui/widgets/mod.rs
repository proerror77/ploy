//! TUI Widget components
//!
//! Modular widgets for the dashboard display.

pub mod agent_status;
pub mod footer;
pub mod market_analysis;
pub mod positions;
pub mod transactions;

pub use agent_status::render_agent_status;
pub use footer::render_footer;
pub use market_analysis::render_market_analysis;
pub use positions::render_positions;
pub use transactions::render_transactions;
