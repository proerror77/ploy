//! Dynamic Fee Model for backtest cost estimation.
//!
//! Polymarket's parabolic fee curve for binary markets.
//! Fee formula: `shares * fee_rate * (p * (1 - p))^exponent`

use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Parabolic fee model matching Polymarket's actual fee curve.
///
/// `effective_rate(p) = fee_rate * (p * (1 - p))^exponent`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeModel {
    /// Base fee rate coefficient (e.g. 0.25 for crypto)
    pub fee_rate: Decimal,
    /// Exponent applied to `p * (1 - p)` (e.g. 2 for crypto)
    pub exponent: u32,
}

impl FeeModel {
    /// Crypto 5m/15m market parameters (fee_rate=0.25, exponent=2).
    pub fn crypto() -> Self {
        FeeModel {
            fee_rate: dec!(0.25),
            exponent: 2,
        }
    }

    /// Sports market parameters (fee_rate=0.0175, exponent=1).
    pub fn sports() -> Self {
        FeeModel {
            fee_rate: dec!(0.0175),
            exponent: 1,
        }
    }

    /// Fee in shares for buying `shares` at price `p`.
    pub fn fee_shares(&self, shares: Decimal, price: Decimal) -> Decimal {
        let p_factor = price * (Decimal::ONE - price);
        let p_powered = match self.exponent {
            1 => p_factor,
            2 => p_factor * p_factor,
            n => p_factor.powd(Decimal::from(n)),
        };
        shares * self.fee_rate * p_powered
    }

    /// Effective fee rate at price `p`.
    pub fn effective_rate(&self, price: Decimal) -> Decimal {
        let p_factor = price * (Decimal::ONE - price);
        match self.exponent {
            1 => self.fee_rate * p_factor,
            2 => self.fee_rate * p_factor * p_factor,
            n => self.fee_rate * p_factor.powd(Decimal::from(n)),
        }
    }
}
