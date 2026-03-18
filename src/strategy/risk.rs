//! Consolidated risk-domain facade.
//!
//! This module is the canonical public surface for strategy-level risk
//! components. It wraps legacy `risk_mgmt/*` internals behind a coherent
//! `strategy::risk` namespace.

pub use super::risk_mgmt::risk::RiskManager;
pub use super::risk_mgmt::slippage::{
    MarketDepth, SlippageCheck, SlippageConfig, SlippageProtection,
};
pub use super::risk_mgmt::validation::{
    leg1_entry_chain, leg2_entry_chain, ExposureValidator, RiskStateValidator, SpreadValidator,
    SumTargetValidator, TimeRemainingValidator, ValidationChain, ValidationContext,
    ValidationError, Validator,
};

pub mod manager {
    pub use super::super::risk_mgmt::risk::*;
}

pub mod slippage {
    pub use super::super::risk_mgmt::slippage::*;
}

pub mod validation {
    pub use super::super::risk_mgmt::validation::*;
}
