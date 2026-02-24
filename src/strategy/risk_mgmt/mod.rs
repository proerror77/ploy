//! Risk management, validation, and slippage protection.

pub mod risk;
pub mod slippage;
pub mod validation;

pub use risk::RiskManager;
pub use slippage::{MarketDepth, SlippageCheck, SlippageConfig, SlippageProtection};
pub use validation::{
    leg1_entry_chain, leg2_entry_chain, ExposureValidator, RiskStateValidator, SpreadValidator,
    SumTargetValidator, TimeRemainingValidator, ValidationChain, ValidationContext,
    ValidationError, Validator,
};
