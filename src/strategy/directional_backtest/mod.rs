//! Directional backtest engine for momentum-driven binary option trading.
//!
//! Uses weighted momentum (10s/30s/60s) → fair value estimation → edge filtering
//! to enter positions, mirroring the live MomentumDetector logic. Holds to
//! settlement by default (binary options settle at $1.00 or $0.00).
//!
//! Binance spot price serves as Chainlink proxy (>99.9% correlation on 5m/15m).
//!
//! Usage:
//!   ploy strategy backtest directional --symbols BTCUSDT --save --json

mod display;
mod engine;
mod state;

pub use engine::DirectionalBacktestEngine;
pub use ploy_backtest::strategies::{DirectionalBacktestConfig, DirectionalClosedTrade};
