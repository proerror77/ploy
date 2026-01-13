//! Unified validation module for trading strategies
//!
//! Consolidates validation logic from risk.rs, signal.rs, and engine.rs
//! into reusable, composable validators.

use crate::domain::{Round, RiskState};
use crate::error::{PloyError, Result};
use rust_decimal::Decimal;
use std::fmt;

// =============================================================================
// Validation Errors
// =============================================================================

/// Validation error with context
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub validator: String,
    pub reason: String,
    pub details: Option<String>,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ref details) = self.details {
            write!(f, "[{}] {}: {}", self.validator, self.reason, details)
        } else {
            write!(f, "[{}] {}", self.validator, self.reason)
        }
    }
}

impl std::error::Error for ValidationError {}

impl From<ValidationError> for PloyError {
    fn from(e: ValidationError) -> Self {
        PloyError::Validation(e.to_string())
    }
}

// =============================================================================
// Validation Context
// =============================================================================

/// Context passed to validators
#[derive(Debug, Clone)]
pub struct ValidationContext {
    /// Number of shares to trade
    pub shares: Option<u64>,
    /// Entry price
    pub price: Option<Decimal>,
    /// Leg1 fill price (for Leg2 validation)
    pub leg1_price: Option<Decimal>,
    /// Opposite side ask price
    pub opposite_ask: Option<Decimal>,
    /// Current spread in basis points
    pub spread_bps: Option<u32>,
    /// Current round info
    pub round: Option<Round>,
    /// Current risk state
    pub risk_state: Option<RiskState>,
    /// Sum target for arbitrage
    pub sum_target: Option<Decimal>,
}

impl ValidationContext {
    pub fn new() -> Self {
        Self {
            shares: None,
            price: None,
            leg1_price: None,
            opposite_ask: None,
            spread_bps: None,
            round: None,
            risk_state: None,
            sum_target: None,
        }
    }

    pub fn with_trade(mut self, shares: u64, price: Decimal) -> Self {
        self.shares = Some(shares);
        self.price = Some(price);
        self
    }

    pub fn with_leg1(mut self, leg1_price: Decimal) -> Self {
        self.leg1_price = Some(leg1_price);
        self
    }

    pub fn with_opposite_ask(mut self, ask: Decimal) -> Self {
        self.opposite_ask = Some(ask);
        self
    }

    pub fn with_spread(mut self, spread_bps: u32) -> Self {
        self.spread_bps = Some(spread_bps);
        self
    }

    pub fn with_round(mut self, round: Round) -> Self {
        self.round = Some(round);
        self
    }

    pub fn with_risk_state(mut self, state: RiskState) -> Self {
        self.risk_state = Some(state);
        self
    }

    pub fn with_sum_target(mut self, target: Decimal) -> Self {
        self.sum_target = Some(target);
        self
    }
}

impl Default for ValidationContext {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Validator Trait
// =============================================================================

/// Trait for all validators
pub trait Validator: Send + Sync {
    /// Name of this validator
    fn name(&self) -> &str;

    /// Validate the context
    fn validate(&self, ctx: &ValidationContext) -> Result<()>;

    /// Check if this validator is applicable to the context
    fn is_applicable(&self, ctx: &ValidationContext) -> bool;
}

// =============================================================================
// Time Remaining Validator
// =============================================================================

/// Validates sufficient time remaining before round end
pub struct TimeRemainingValidator {
    pub min_seconds: u64,
}

impl TimeRemainingValidator {
    pub fn new(min_seconds: u64) -> Self {
        Self { min_seconds }
    }
}

impl Validator for TimeRemainingValidator {
    fn name(&self) -> &str {
        "TimeRemaining"
    }

    fn validate(&self, ctx: &ValidationContext) -> Result<()> {
        let round = ctx.round.as_ref().ok_or_else(|| ValidationError {
            validator: self.name().to_string(),
            reason: "Missing round info".to_string(),
            details: None,
        })?;

        let remaining = round.seconds_remaining() as u64;
        if remaining < self.min_seconds {
            return Err(ValidationError {
                validator: self.name().to_string(),
                reason: "Insufficient time remaining".to_string(),
                details: Some(format!(
                    "remaining={}s, min={}s",
                    remaining, self.min_seconds
                )),
            }
            .into());
        }

        Ok(())
    }

    fn is_applicable(&self, ctx: &ValidationContext) -> bool {
        ctx.round.is_some()
    }
}

// =============================================================================
// Exposure Validator
// =============================================================================

/// Validates trade exposure limits
pub struct ExposureValidator {
    pub max_single_exposure: Decimal,
    pub max_total_exposure: Option<Decimal>,
    pub current_exposure: Option<Decimal>,
}

impl ExposureValidator {
    pub fn new(max_single_exposure: Decimal) -> Self {
        Self {
            max_single_exposure,
            max_total_exposure: None,
            current_exposure: None,
        }
    }

    pub fn with_total_limit(mut self, max_total: Decimal, current: Decimal) -> Self {
        self.max_total_exposure = Some(max_total);
        self.current_exposure = Some(current);
        self
    }
}

impl Validator for ExposureValidator {
    fn name(&self) -> &str {
        "Exposure"
    }

    fn validate(&self, ctx: &ValidationContext) -> Result<()> {
        let shares = ctx.shares.ok_or_else(|| ValidationError {
            validator: self.name().to_string(),
            reason: "Missing shares".to_string(),
            details: None,
        })?;

        let price = ctx.price.ok_or_else(|| ValidationError {
            validator: self.name().to_string(),
            reason: "Missing price".to_string(),
            details: None,
        })?;

        let exposure = Decimal::from(shares) * price;

        // Check single trade limit
        if exposure > self.max_single_exposure {
            return Err(ValidationError {
                validator: self.name().to_string(),
                reason: "Single trade exposure exceeded".to_string(),
                details: Some(format!(
                    "exposure=${}, limit=${}",
                    exposure, self.max_single_exposure
                )),
            }
            .into());
        }

        // Check total exposure limit if configured
        if let (Some(max_total), Some(current)) = (self.max_total_exposure, self.current_exposure) {
            if current + exposure > max_total {
                return Err(ValidationError {
                    validator: self.name().to_string(),
                    reason: "Total exposure limit exceeded".to_string(),
                    details: Some(format!(
                        "current=${}, new=${}, limit=${}",
                        current, exposure, max_total
                    )),
                }
                .into());
            }
        }

        Ok(())
    }

    fn is_applicable(&self, ctx: &ValidationContext) -> bool {
        ctx.shares.is_some() && ctx.price.is_some()
    }
}

// =============================================================================
// Spread Validator
// =============================================================================

/// Validates spread is within acceptable range
pub struct SpreadValidator {
    pub max_spread_bps: u32,
}

impl SpreadValidator {
    pub fn new(max_spread_bps: u32) -> Self {
        Self { max_spread_bps }
    }
}

impl Validator for SpreadValidator {
    fn name(&self) -> &str {
        "Spread"
    }

    fn validate(&self, ctx: &ValidationContext) -> Result<()> {
        let spread = ctx.spread_bps.ok_or_else(|| ValidationError {
            validator: self.name().to_string(),
            reason: "Missing spread".to_string(),
            details: None,
        })?;

        if spread > self.max_spread_bps {
            return Err(ValidationError {
                validator: self.name().to_string(),
                reason: "Spread too wide".to_string(),
                details: Some(format!(
                    "spread={}bps, max={}bps",
                    spread, self.max_spread_bps
                )),
            }
            .into());
        }

        Ok(())
    }

    fn is_applicable(&self, ctx: &ValidationContext) -> bool {
        ctx.spread_bps.is_some()
    }
}

// =============================================================================
// Risk State Validator
// =============================================================================

/// Validates risk state allows trading
pub struct RiskStateValidator {
    pub allowed_states: Vec<RiskState>,
}

impl RiskStateValidator {
    pub fn new() -> Self {
        Self {
            allowed_states: vec![RiskState::Normal],
        }
    }

    pub fn allow_elevated(mut self) -> Self {
        self.allowed_states.push(RiskState::Elevated);
        self
    }
}

impl Default for RiskStateValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl Validator for RiskStateValidator {
    fn name(&self) -> &str {
        "RiskState"
    }

    fn validate(&self, ctx: &ValidationContext) -> Result<()> {
        let state = ctx.risk_state.as_ref().ok_or_else(|| ValidationError {
            validator: self.name().to_string(),
            reason: "Missing risk state".to_string(),
            details: None,
        })?;

        if !self.allowed_states.contains(state) {
            return Err(ValidationError {
                validator: self.name().to_string(),
                reason: "Trading not allowed in current risk state".to_string(),
                details: Some(format!("state={:?}", state)),
            }
            .into());
        }

        Ok(())
    }

    fn is_applicable(&self, ctx: &ValidationContext) -> bool {
        ctx.risk_state.is_some()
    }
}

// =============================================================================
// Sum Target Validator (Leg2 Arbitrage)
// =============================================================================

/// Validates sum of leg1 + opposite_ask meets target
pub struct SumTargetValidator {
    /// Base target (typically 1.0)
    pub base_target: Decimal,
    /// Fee buffer to subtract
    pub fee_buffer: Decimal,
    /// Slippage buffer to subtract
    pub slippage_buffer: Decimal,
    /// Profit buffer to subtract
    pub profit_buffer: Decimal,
}

impl SumTargetValidator {
    pub fn new(base_target: Decimal) -> Self {
        Self {
            base_target,
            fee_buffer: Decimal::ZERO,
            slippage_buffer: Decimal::ZERO,
            profit_buffer: Decimal::ZERO,
        }
    }

    pub fn with_buffers(
        mut self,
        fee: Decimal,
        slippage: Decimal,
        profit: Decimal,
    ) -> Self {
        self.fee_buffer = fee;
        self.slippage_buffer = slippage;
        self.profit_buffer = profit;
        self
    }

    /// Calculate effective target after buffers
    pub fn effective_target(&self) -> Decimal {
        self.base_target - self.fee_buffer - self.slippage_buffer - self.profit_buffer
    }
}

impl Validator for SumTargetValidator {
    fn name(&self) -> &str {
        "SumTarget"
    }

    fn validate(&self, ctx: &ValidationContext) -> Result<()> {
        let leg1_price = ctx.leg1_price.ok_or_else(|| ValidationError {
            validator: self.name().to_string(),
            reason: "Missing leg1 price".to_string(),
            details: None,
        })?;

        let opposite_ask = ctx.opposite_ask.ok_or_else(|| ValidationError {
            validator: self.name().to_string(),
            reason: "Missing opposite ask".to_string(),
            details: None,
        })?;

        let sum = leg1_price + opposite_ask;
        let target = self.effective_target();

        if sum > target {
            return Err(ValidationError {
                validator: self.name().to_string(),
                reason: "Sum exceeds target".to_string(),
                details: Some(format!(
                    "sum={}, target={} (leg1={}, opp_ask={})",
                    sum, target, leg1_price, opposite_ask
                )),
            }
            .into());
        }

        Ok(())
    }

    fn is_applicable(&self, ctx: &ValidationContext) -> bool {
        ctx.leg1_price.is_some() && ctx.opposite_ask.is_some()
    }
}

// =============================================================================
// Validation Chain
// =============================================================================

/// Chain of validators to run in sequence
pub struct ValidationChain {
    validators: Vec<Box<dyn Validator>>,
    /// Skip validators that are not applicable
    skip_inapplicable: bool,
}

impl ValidationChain {
    pub fn new() -> Self {
        Self {
            validators: Vec::new(),
            skip_inapplicable: true,
        }
    }

    /// Add a validator to the chain
    pub fn add<V: Validator + 'static>(mut self, validator: V) -> Self {
        self.validators.push(Box::new(validator));
        self
    }

    /// Set whether to skip inapplicable validators
    pub fn strict(mut self) -> Self {
        self.skip_inapplicable = false;
        self
    }

    /// Run all validators in the chain
    pub fn validate(&self, ctx: &ValidationContext) -> Result<()> {
        for validator in &self.validators {
            if self.skip_inapplicable && !validator.is_applicable(ctx) {
                continue;
            }
            validator.validate(ctx)?;
        }
        Ok(())
    }

    /// Run all validators and collect all errors (doesn't short-circuit)
    pub fn validate_all(&self, ctx: &ValidationContext) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        for validator in &self.validators {
            if self.skip_inapplicable && !validator.is_applicable(ctx) {
                continue;
            }
            if let Err(e) = validator.validate(ctx) {
                if let PloyError::Validation(msg) = e {
                    errors.push(ValidationError {
                        validator: validator.name().to_string(),
                        reason: msg,
                        details: None,
                    });
                }
            }
        }
        errors
    }
}

impl Default for ValidationChain {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Pre-built Validation Chains
// =============================================================================

/// Create a validation chain for Leg1 entry
pub fn leg1_entry_chain(
    max_exposure: Decimal,
    min_time_seconds: u64,
    max_spread_bps: u32,
) -> ValidationChain {
    ValidationChain::new()
        .add(RiskStateValidator::new())
        .add(ExposureValidator::new(max_exposure))
        .add(TimeRemainingValidator::new(min_time_seconds))
        .add(SpreadValidator::new(max_spread_bps))
}

/// Create a validation chain for Leg2 entry
pub fn leg2_entry_chain(
    sum_target: Decimal,
    fee_buffer: Decimal,
    slippage_buffer: Decimal,
    profit_buffer: Decimal,
) -> ValidationChain {
    ValidationChain::new()
        .add(SumTargetValidator::new(sum_target).with_buffers(
            fee_buffer,
            slippage_buffer,
            profit_buffer,
        ))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn test_round(seconds_remaining: i64) -> Round {
        use chrono::{Duration, Utc};
        Round {
            id: None,
            slug: "test-round".to_string(),
            up_token_id: "up-token".to_string(),
            down_token_id: "down-token".to_string(),
            start_time: Utc::now() - Duration::hours(1),
            end_time: Utc::now() + Duration::seconds(seconds_remaining),
            outcome: None,
        }
    }

    #[test]
    fn test_time_validator() {
        let validator = TimeRemainingValidator::new(60);

        // Enough time
        let ctx = ValidationContext::new().with_round(test_round(120));
        assert!(validator.validate(&ctx).is_ok());

        // Not enough time
        let ctx = ValidationContext::new().with_round(test_round(30));
        assert!(validator.validate(&ctx).is_err());
    }

    #[test]
    fn test_exposure_validator() {
        let validator = ExposureValidator::new(dec!(100));

        // Within limit
        let ctx = ValidationContext::new().with_trade(100, dec!(0.50));
        assert!(validator.validate(&ctx).is_ok());

        // Exceeds limit
        let ctx = ValidationContext::new().with_trade(300, dec!(0.50));
        assert!(validator.validate(&ctx).is_err());
    }

    #[test]
    fn test_sum_target_validator() {
        let validator = SumTargetValidator::new(dec!(1.0))
            .with_buffers(dec!(0.005), dec!(0.02), dec!(0.01));

        // effective = 1 - 0.005 - 0.02 - 0.01 = 0.965

        // Sum within target
        let ctx = ValidationContext::new()
            .with_leg1(dec!(0.45))
            .with_opposite_ask(dec!(0.50)); // sum = 0.95 <= 0.965
        assert!(validator.validate(&ctx).is_ok());

        // Sum exceeds target
        let ctx = ValidationContext::new()
            .with_leg1(dec!(0.45))
            .with_opposite_ask(dec!(0.55)); // sum = 1.00 > 0.965
        assert!(validator.validate(&ctx).is_err());
    }

    #[test]
    fn test_validation_chain() {
        let chain = ValidationChain::new()
            .add(ExposureValidator::new(dec!(100)))
            .add(TimeRemainingValidator::new(60));

        // All pass
        let ctx = ValidationContext::new()
            .with_trade(100, dec!(0.50))
            .with_round(test_round(120));
        assert!(chain.validate(&ctx).is_ok());

        // Exposure fails
        let ctx = ValidationContext::new()
            .with_trade(300, dec!(0.50))
            .with_round(test_round(120));
        assert!(chain.validate(&ctx).is_err());
    }

    #[test]
    fn test_leg1_entry_chain() {
        let chain = leg1_entry_chain(dec!(100), 60, 200);

        let ctx = ValidationContext::new()
            .with_trade(100, dec!(0.50))
            .with_round(test_round(120))
            .with_spread(100)
            .with_risk_state(RiskState::Normal);

        assert!(chain.validate(&ctx).is_ok());
    }
}
