//! Signal detectors for various trading strategies
//!
//! This module contains signal detection logic used by strategies:
//! - Dump detector: Identifies price dumps for two-leg arbitrage
//! - Momentum detector: Identifies momentum/trend signals

pub mod dump;
pub mod momentum;

pub use dump::{DumpDetector, DumpDetectorConfig, DumpSignal};
pub use momentum::{MomentumDetector, MomentumDetectorConfig, MomentumSignal, TrendDirection};
