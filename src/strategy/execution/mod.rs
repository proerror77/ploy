//! Order execution pipeline.
//!
//! Contains the strategy engine state machine, order executor with retry logic,
//! fund management, idempotency protection, and the EngineStore trait for DI.

pub mod engine;
pub mod engine_store;
pub mod executor;
pub mod fund_manager;
pub mod idempotency;

pub use engine::StrategyEngine;
pub use engine_store::EngineStore;
pub use executor::OrderExecutor;
pub use fund_manager::{FundManager, FundStatus, PositionSizeResult};
pub use idempotency::{IdempotencyManager, IdempotencyResult};
