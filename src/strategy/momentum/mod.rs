//! Momentum strategy for Polymarket trading.
//!
//! This module is split by subdomain:
//! - `config` for strategy defaults and config types
//! - `event_matcher` for Polymarket event discovery and mapping
//! - `signal` for direction/signal detection logic
//! - `position` for position and exit state
//! - `window_risk` for pending signal and cross-window exposure tracking
//! - `engine` for the live runtime orchestration loop

mod config;
mod engine;
mod event_matcher;
mod position;
mod signal;
mod window_risk;

pub use config::{ExitConfig, MomentumConfig};
pub use engine::MomentumEngine;
pub use event_matcher::{EventInfo, EventMatcher};
pub use position::{ExitManager, ExitReason, Position};
pub use signal::{Direction, MomentumDetector, MomentumSignal};
