//! Simulated order executor for backtesting and dry-run.
//!
//! Models realistic execution with:
//! - Bid-ask spread impact
//! - Partial fills based on market depth
//! - Market impact on large orders
//! - Fill delay simulation
//!
//! Used by both backtest (`HistoricalFeed + SimulatedExecutor`) and
//! dry-run (`LiveFeed + SimulatedExecutor`) modes.

use async_trait::async_trait;
use ploy_trading::{FillRecord, IntentPurpose, TradeSide, TradingIntent};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::traits::{ExecutionReport, Executor};

const MIN_BINARY_PRICE: Decimal = dec!(0.01);
const MAX_BINARY_PRICE: Decimal = dec!(0.99);

/// Execution simulation configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedExecutorConfig {
    /// Apply bid-ask spread to fill price.
    pub use_spread: bool,
    /// Spread width as fraction of mid (e.g. 0.02 = 2%).
    pub spread_pct: Decimal,
    /// Enable partial fills based on market depth.
    pub enable_partial_fills: bool,
    /// Available depth as multiple of typical order (e.g. 5.0).
    pub depth_multiple: Decimal,
    /// Minimum fill percentage when partial (e.g. 0.5 = 50%).
    pub min_fill_pct: Decimal,
    /// Enable market impact modelling.
    pub enable_market_impact: bool,
    /// Price impact coefficient per depth ratio.
    pub impact_coefficient: Decimal,
    /// Default market depth in shares when unknown.
    pub default_depth_shares: u64,
}

impl Default for SimulatedExecutorConfig {
    fn default() -> Self {
        Self {
            use_spread: true,
            spread_pct: dec!(0.02),
            enable_partial_fills: true,
            depth_multiple: dec!(5.0),
            min_fill_pct: dec!(0.5),
            enable_market_impact: true,
            impact_coefficient: dec!(0.1),
            default_depth_shares: 500,
        }
    }
}

/// Simulated executor that models realistic fills.
pub struct SimulatedExecutor {
    config: SimulatedExecutorConfig,
}

impl SimulatedExecutor {
    pub fn new(config: SimulatedExecutorConfig) -> Self {
        Self { config }
    }

    fn clamp_price(price: Decimal) -> Decimal {
        price.max(MIN_BINARY_PRICE).min(MAX_BINARY_PRICE)
    }

    fn crypto_trade_fee(fill_price: Decimal) -> Decimal {
        let p_factor = fill_price * (Decimal::ONE - fill_price);
        let fee_rate = dec!(0.25) * p_factor * p_factor;
        fill_price * fee_rate
    }

    /// Simulate a buy fill from a quoted executable price.
    fn simulate_buy(
        &self,
        signal_price: Decimal,
        quantity: Decimal,
        synthetic_mid: bool,
    ) -> (Decimal, Decimal, Decimal, Decimal) {
        let signal_price = Self::clamp_price(signal_price);
        let shares = quantity.max(Decimal::ZERO);
        let depth = Decimal::from(self.config.default_depth_shares);

        // Only synthesize spread when we do not already have an executable quote.
        let half_spread = if self.config.use_spread && synthetic_mid {
            signal_price * self.config.spread_pct / dec!(2)
        } else {
            Decimal::ZERO
        };
        let ask = Self::clamp_price(signal_price + half_spread);

        // Partial fill
        let (filled_qty, _is_partial) = self.fill_quantity(shares, depth);

        // Market impact
        let impact = if self.config.enable_market_impact && depth > Decimal::ZERO {
            let ratio = filled_qty / depth;
            ask * self.config.impact_coefficient * ratio
        } else {
            Decimal::ZERO
        };

        let fill_price = Self::clamp_price(ask + impact);
        let slippage = fill_price - signal_price;

        (fill_price, filled_qty, slippage, impact)
    }

    /// Simulate a sell fill from a quoted executable price.
    fn simulate_sell(
        &self,
        signal_price: Decimal,
        quantity: Decimal,
        synthetic_mid: bool,
    ) -> (Decimal, Decimal, Decimal, Decimal) {
        let signal_price = Self::clamp_price(signal_price);
        let shares = quantity.max(Decimal::ZERO);
        let depth = Decimal::from(self.config.default_depth_shares);

        let half_spread = if self.config.use_spread && synthetic_mid {
            signal_price * self.config.spread_pct / dec!(2)
        } else {
            Decimal::ZERO
        };
        let bid = Self::clamp_price(signal_price - half_spread);

        let (filled_qty, _is_partial) = self.fill_quantity(shares, depth);

        let impact = if self.config.enable_market_impact && depth > Decimal::ZERO {
            let ratio = filled_qty / depth;
            bid * self.config.impact_coefficient * ratio
        } else {
            Decimal::ZERO
        };

        let fill_price = Self::clamp_price(bid - impact);
        let slippage = signal_price - fill_price;

        (fill_price, filled_qty, slippage, impact)
    }

    /// Determine fill quantity given requested shares and market depth.
    fn fill_quantity(&self, requested: Decimal, depth: Decimal) -> (Decimal, bool) {
        if !self.config.enable_partial_fills || depth <= Decimal::ZERO {
            return (requested, false);
        }

        let depth_multiple = self.config.depth_multiple.max(dec!(0.000001));
        let typical = (depth / depth_multiple).max(dec!(100));

        if requested <= typical {
            (requested, false)
        } else if requested <= depth {
            let ratio = (depth / requested).min(Decimal::ONE);
            let min = requested * self.config.min_fill_pct;
            let filled = (requested * ratio).max(min).min(requested);
            (filled, filled < requested)
        } else {
            let min = requested * self.config.min_fill_pct;
            let filled = depth.max(min).min(requested);
            (filled, true)
        }
    }
}

#[async_trait]
impl Executor for SimulatedExecutor {
    async fn submit(&mut self, intent: &TradingIntent, order_id: &str) -> ExecutionReport {
        let signal_price = intent.limit_price.unwrap_or(dec!(0.50));
        let synthetic_mid = intent.limit_price.is_none();

        // Settlement exits bypass spread/impact simulation
        let is_settlement = intent.purpose == IntentPurpose::Exit
            && (signal_price == Decimal::ZERO || signal_price == Decimal::ONE);

        let (fill_price, filled_qty, slippage, impact) = if is_settlement {
            (signal_price, intent.quantity, Decimal::ZERO, Decimal::ZERO)
        } else {
            match intent.side {
                TradeSide::Buy => self.simulate_buy(signal_price, intent.quantity, synthetic_mid),
                TradeSide::Sell => self.simulate_sell(signal_price, intent.quantity, synthetic_mid),
            }
        };

        if filled_qty <= Decimal::ZERO {
            return ExecutionReport {
                order_id: order_id.to_string(),
                fill: None,
                rejected: true,
                rejection_reason: Some("No liquidity".into()),
                slippage: None,
                market_impact: None,
            };
        }

        let fee = if is_settlement {
            Decimal::ZERO
        } else {
            Self::crypto_trade_fee(fill_price) * filled_qty
        };

        let fill = FillRecord {
            fill_id: Uuid::new_v4().to_string(),
            order_id: order_id.to_string(),
            token_id: intent.token_id.clone(),
            side: intent.side.clone(),
            quantity: filled_qty,
            price: fill_price,
            fee,
            timestamp: intent.created_at, // Use intent time for backtest consistency
        };

        ExecutionReport {
            order_id: order_id.to_string(),
            fill: Some(fill),
            rejected: false,
            rejection_reason: None,
            slippage: Some(slippage),
            market_impact: Some(impact),
        }
    }

    async fn cancel(&mut self, _order_id: &str) -> bool {
        true // Simulated cancel always succeeds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ploy_trading::IntentPurpose;

    fn test_intent(side: TradeSide, price: Decimal, qty: Decimal) -> TradingIntent {
        TradingIntent {
            intent_id: "test-intent".into(),
            deployment_id: "test".into(),
            market_id: "market-1".into(),
            token_id: "token-1".into(),
            side,
            quantity: qty,
            limit_price: Some(price),
            purpose: IntentPurpose::Entry,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn buy_applies_quote_price_and_impact() {
        let mut exec = SimulatedExecutor::new(SimulatedExecutorConfig::default());
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(25));

        let report = exec.submit(&intent).await;
        assert!(!report.rejected);
        let fill = report.fill.unwrap();
        // Fill price should be above the quoted ask only because of impact.
        assert!(
            fill.price > dec!(0.50),
            "fill={} should be > 0.50",
            fill.price
        );
        assert!(report.slippage.unwrap() > Decimal::ZERO);
        assert_eq!(fill.price.round_dp(4), dec!(0.5025));
    }

    #[tokio::test]
    async fn sell_applies_quote_price_and_impact() {
        let mut exec = SimulatedExecutor::new(SimulatedExecutorConfig::default());
        let intent = test_intent(TradeSide::Sell, dec!(0.50), dec!(25));

        let report = exec.submit(&intent).await;
        assert!(!report.rejected);
        let fill = report.fill.unwrap();
        // Fill price should be below the quoted bid only because of impact.
        assert!(
            fill.price < dec!(0.50),
            "fill={} should be < 0.50",
            fill.price
        );
        assert_eq!(fill.price.round_dp(4), dec!(0.4975));
    }

    #[tokio::test]
    async fn no_spread_means_signal_price() {
        let config = SimulatedExecutorConfig {
            use_spread: false,
            enable_market_impact: false,
            enable_partial_fills: false,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        let intent = test_intent(TradeSide::Buy, dec!(0.60), dec!(10));

        let report = exec.submit(&intent).await;
        let fill = report.fill.unwrap();
        assert_eq!(fill.price, dec!(0.60));
        assert_eq!(report.slippage.unwrap(), Decimal::ZERO);
    }

    #[tokio::test]
    async fn settlement_exit_has_no_fee() {
        let mut exec = SimulatedExecutor::new(SimulatedExecutorConfig::default());
        let mut intent = test_intent(TradeSide::Sell, dec!(1.00), dec!(10));
        intent.purpose = IntentPurpose::Exit;

        let report = exec.submit(&intent).await;
        let fill = report.fill.expect("settlement fill");
        assert_eq!(fill.price, dec!(1.00));
        assert_eq!(fill.fee, Decimal::ZERO);
    }

    #[tokio::test]
    async fn entry_fee_uses_pm_parabolic_curve() {
        let config = SimulatedExecutorConfig {
            use_spread: false,
            enable_market_impact: false,
            enable_partial_fills: false,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(10));

        let report = exec.submit(&intent).await;
        let fill = report.fill.expect("fill");
        assert_eq!(fill.fee.round_dp(6), dec!(0.078125));
    }

    #[tokio::test]
    async fn preserves_fractional_share_quantity() {
        let config = SimulatedExecutorConfig {
            use_spread: false,
            enable_market_impact: false,
            enable_partial_fills: false,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        let intent = test_intent(TradeSide::Buy, dec!(0.60), dec!(41.666667));

        let report = exec.submit(&intent).await;
        let fill = report.fill.expect("fill");
        assert_eq!(fill.quantity, dec!(41.666667));
        assert_eq!(fill.price, dec!(0.60));
    }
}
