//! Ploy Risk — unified risk management for the ploy trading system.
//!
//! This crate contains pure risk logic: slippage protection, validation chains,
//! circuit breaker, and risk error types. It intentionally avoids I/O (no DB,
//! no HTTP) so that risk checks remain fast and testable.

pub mod circuit_breaker;
pub mod error;
pub mod slippage;
pub mod validation;

pub use circuit_breaker::{
    CircuitBreakerStats, CircuitState, TradingCircuitBreaker, TradingCircuitBreakerConfig,
    TripReason,
};
pub use error::{RiskError, RiskResult};
pub use slippage::{MarketDepth, SlippageCheck, SlippageConfig, SlippageProtection};
pub use validation::{
    leg1_entry_chain, leg2_entry_chain, ExposureValidator, RiskStateValidator, SpreadValidator,
    SumTargetValidator, TimeRemainingValidator, ValidationChain, ValidationContext,
    ValidationError, Validator,
};
