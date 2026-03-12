//! Multi-outcome market analysis for non-binary markets like BTC price levels.
//!
//! Unlike binary markets (UP/DOWN), multi-outcome markets have many possible outcomes
//! that may or may not be mutually exclusive. This module provides:
//! - Price fetching for all outcomes
//! - Monotonicity violation detection
//! - Cross-outcome arbitrage analysis
//! - Bid-ask spread analysis
//! - Expected Value (EV) calculations with fee adjustment
//! - Split/Merge market making arbitrage detection
//! - Near-settlement opportunity scanning

use chrono::Duration;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

mod monitor;

pub use monitor::{
    fetch_multi_outcome_event, ArbitrageType, MultiOutcomeArbitrage, MultiOutcomeMonitor, Outcome,
    OutcomeDirection, OutcomeSummary,
};

/// Polymarket fee rate (approximately 2%)
pub const POLYMARKET_FEE_RATE: Decimal = dec!(0.02);

/// Expected Value calculation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedValue {
    /// Entry price (what you pay per share)
    pub entry_price: Decimal,
    /// True probability estimate
    pub true_probability: Decimal,
    /// Win payout (usually $1 per share)
    pub win_payout: Decimal,
    /// Fee rate applied
    pub fee_rate: Decimal,
    /// Gross expected value (before fees)
    pub gross_ev: Decimal,
    /// Net expected value (after fees)
    pub net_ev: Decimal,
    /// Return on investment (net_ev / entry_price)
    pub roi: Decimal,
    /// Kelly criterion bet fraction
    pub kelly_fraction: Decimal,
    /// Break-even probability required
    pub breakeven_prob: Decimal,
    /// Is this a +EV opportunity?
    pub is_positive_ev: bool,
}

impl ExpectedValue {
    /// Calculate expected value for a bet
    ///
    /// # Arguments
    /// * `entry_price` - Price to buy Yes shares (e.g., 0.95 for 95¢)
    /// * `true_probability` - Your estimate of true win probability (e.g., 0.97)
    /// * `fee_rate` - Platform fee (default: POLYMARKET_FEE_RATE = 0.02)
    pub fn calculate(
        entry_price: Decimal,
        true_probability: Decimal,
        fee_rate: Option<Decimal>,
    ) -> Self {
        let fee = fee_rate.unwrap_or(POLYMARKET_FEE_RATE);
        let win_payout = Decimal::ONE; // $1 per share on win

        // Gross profit per share on win (before fees)
        let gross_profit_on_win = win_payout - entry_price;
        // Net profit on win (after fee on profit)
        let net_profit_on_win = gross_profit_on_win * (Decimal::ONE - fee);
        // Loss on lose (full entry price)
        let loss_on_lose = entry_price;

        // Expected value = P(win) * profit - P(lose) * loss
        let gross_ev = true_probability * gross_profit_on_win
            - (Decimal::ONE - true_probability) * loss_on_lose;
        let net_ev =
            true_probability * net_profit_on_win - (Decimal::ONE - true_probability) * loss_on_lose;

        // ROI = EV / cost
        let roi = if entry_price > Decimal::ZERO {
            net_ev / entry_price
        } else {
            Decimal::ZERO
        };

        // Kelly criterion: f* = (bp - q) / b
        // where b = net profit on win / entry price, p = prob win, q = prob lose
        let b = net_profit_on_win / entry_price;
        let p = true_probability;
        let q = Decimal::ONE - true_probability;
        let kelly = if b > Decimal::ZERO {
            (b * p - q) / b
        } else {
            Decimal::ZERO
        };

        // Break-even probability: entry_price / (1 - fee * profit_margin)
        // Simplified: need P where P * net_profit = (1-P) * loss
        // P = loss / (net_profit + loss)
        let breakeven = if net_profit_on_win + loss_on_lose > Decimal::ZERO {
            loss_on_lose / (net_profit_on_win + loss_on_lose)
        } else {
            Decimal::ONE
        };

        ExpectedValue {
            entry_price,
            true_probability,
            win_payout,
            fee_rate: fee,
            gross_ev,
            net_ev,
            roi,
            kelly_fraction: kelly.max(Decimal::ZERO),
            breakeven_prob: breakeven,
            is_positive_ev: net_ev > Decimal::ZERO,
        }
    }

    /// Calculate minimum probability needed for +EV at a given price
    pub fn min_probability_for_positive_ev(
        entry_price: Decimal,
        fee_rate: Option<Decimal>,
    ) -> Decimal {
        let fee = fee_rate.unwrap_or(POLYMARKET_FEE_RATE);
        let profit_on_win = (Decimal::ONE - entry_price) * (Decimal::ONE - fee);
        let loss_on_lose = entry_price;

        // P * profit = (1-P) * loss
        // P = loss / (profit + loss)
        if profit_on_win + loss_on_lose > Decimal::ZERO {
            loss_on_lose / (profit_on_win + loss_on_lose)
        } else {
            Decimal::ONE
        }
    }
}

/// Split/Merge market making opportunity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitMergeOpportunity {
    /// Type of opportunity
    pub opportunity_type: SplitMergeType,
    /// Best Yes ask price
    pub yes_ask: Decimal,
    /// Best No ask price
    pub no_ask: Decimal,
    /// Best Yes bid price
    pub yes_bid: Decimal,
    /// Best No bid price
    pub no_bid: Decimal,
    /// Profit per $1 (split if > 1, merge if < 1)
    pub profit_per_dollar: Decimal,
    /// Estimated slippage
    pub estimated_slippage: Decimal,
    /// Net profit after estimated slippage
    pub net_profit: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitMergeType {
    /// Split $1 → Yes + No when market asks sum > 1
    /// Sell both sides for profit
    SplitAndSell,
    /// Buy Yes + No when market bids sum < 1
    /// Merge to redeem $1
    BuyAndMerge,
}

impl std::fmt::Display for SplitMergeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SplitMergeType::SplitAndSell => write!(f, "Split & Sell"),
            SplitMergeType::BuyAndMerge => write!(f, "Buy & Merge"),
        }
    }
}

/// Near-settlement market analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NearSettlementAnalysis {
    /// Time until settlement
    pub time_to_settlement: Duration,
    /// Hours remaining
    pub hours_remaining: f64,
    /// Current Yes price (implied probability)
    pub yes_price: Decimal,
    /// Required true probability for +EV
    pub min_probability_for_ev: Decimal,
    /// Expected value analysis
    pub ev_analysis: ExpectedValue,
    /// Risk assessment
    pub risk_level: RiskLevel,
    /// Strategy recommendation
    pub recommendation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    /// Very risky (EV << 0 or high probability of failure)
    VeryHigh,
    /// Risky but potentially profitable
    High,
    /// Moderate risk
    Medium,
    /// Low risk, likely profitable
    Low,
    /// Minimal risk
    VeryLow,
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RiskLevel::VeryHigh => write!(f, "🔴 Very High"),
            RiskLevel::High => write!(f, "🟠 High"),
            RiskLevel::Medium => write!(f, "🟡 Medium"),
            RiskLevel::Low => write!(f, "🟢 Low"),
            RiskLevel::VeryLow => write!(f, "✅ Very Low"),
        }
    }
}

/// Market making strategy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketMakingConfig {
    /// Target spread (Yes_ask + No_ask, e.g., 1.02 to 1.08)
    pub target_spread_min: Decimal,
    pub target_spread_max: Decimal,
    /// Maximum exposure per outcome
    pub max_exposure_per_outcome: Decimal,
    /// Maximum total exposure
    pub max_total_exposure: Decimal,
    /// Rebalance threshold (when to hedge)
    pub rebalance_threshold: Decimal,
    /// Minimum profit margin to enter
    pub min_profit_margin: Decimal,
}

impl Default for MarketMakingConfig {
    fn default() -> Self {
        Self {
            target_spread_min: dec!(1.02),
            target_spread_max: dec!(1.08),
            max_exposure_per_outcome: dec!(500),
            max_total_exposure: dec!(2000),
            rebalance_threshold: dec!(0.2), // 20% imbalance triggers hedge
            min_profit_margin: dec!(0.01),  // 1% minimum
        }
    }
}

/// Market making opportunity analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketMakingOpportunity {
    /// Token ID for Yes side
    pub yes_token_id: String,
    /// Token ID for No side
    pub no_token_id: Option<String>,
    /// Current best bid for Yes
    pub yes_bid: Decimal,
    /// Current best ask for Yes
    pub yes_ask: Decimal,
    /// Current best bid for No
    pub no_bid: Decimal,
    /// Current best ask for No
    pub no_ask: Decimal,
    /// Current spread (yes_ask + no_ask)
    pub current_spread: Decimal,
    /// Is spread within target range?
    pub spread_in_range: bool,
    /// Estimated profit if both sides fill
    pub estimated_profit: Decimal,
    /// Split/Merge opportunity if any
    pub split_merge: Option<SplitMergeOpportunity>,
    /// Recommendation
    pub recommendation: MarketMakingAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketMakingAction {
    /// Post both bid and ask, capture spread
    PostBothSides {
        yes_quote: (Decimal, Decimal),
        no_quote: (Decimal, Decimal),
    },
    /// Split and sell both sides
    SplitAndSell,
    /// Buy both and merge
    BuyAndMerge,
    /// Rebalance inventory
    Rebalance { sell_side: String, buy_side: String },
    /// Wait for better opportunity
    Wait { reason: String },
}

/// Detect Split/Merge arbitrage opportunity for a binary market
///
/// Split Arbitrage: When Yes_bid + No_bid > 1, split $1 into Yes+No, sell both
/// Merge Arbitrage: When Yes_ask + No_ask < 1, buy both, merge to redeem $1
pub fn detect_split_merge_opportunity(
    yes_bid: Decimal,
    yes_ask: Decimal,
    no_bid: Decimal,
    no_ask: Decimal,
    slippage_estimate: Decimal,
) -> Option<SplitMergeOpportunity> {
    // Check for Split & Sell opportunity
    // If we can sell Yes + No for more than $1, we profit
    let sell_sum = yes_bid + no_bid;
    if sell_sum > Decimal::ONE {
        let gross_profit = sell_sum - Decimal::ONE;
        let net_profit = gross_profit - slippage_estimate;
        if net_profit > Decimal::ZERO {
            return Some(SplitMergeOpportunity {
                opportunity_type: SplitMergeType::SplitAndSell,
                yes_ask,
                no_ask,
                yes_bid,
                no_bid,
                profit_per_dollar: gross_profit,
                estimated_slippage: slippage_estimate,
                net_profit,
            });
        }
    }

    // Check for Buy & Merge opportunity
    // If we can buy Yes + No for less than $1, merge for profit
    let buy_sum = yes_ask + no_ask;
    if buy_sum < Decimal::ONE {
        let gross_profit = Decimal::ONE - buy_sum;
        let net_profit = gross_profit - slippage_estimate;
        if net_profit > Decimal::ZERO {
            return Some(SplitMergeOpportunity {
                opportunity_type: SplitMergeType::BuyAndMerge,
                yes_ask,
                no_ask,
                yes_bid,
                no_bid,
                profit_per_dollar: gross_profit,
                estimated_slippage: slippage_estimate,
                net_profit,
            });
        }
    }

    None
}

/// Analyze a market for market making opportunities
///
/// Professional market making strategy:
/// 1. Split USDC into Yes + No shares
/// 2. Post asks on both sides at 1-10% markup (depending on volatility)
/// 3. When one side fills, rebalance by buying the other side
/// 4. Merge remaining inventory back to USDC
pub fn analyze_market_making_opportunity(
    yes_bid: Decimal,
    yes_ask: Decimal,
    no_bid: Decimal,
    no_ask: Decimal,
    config: &MarketMakingConfig,
) -> MarketMakingOpportunity {
    let current_spread = yes_ask + no_ask;
    let spread_in_range =
        current_spread >= config.target_spread_min && current_spread <= config.target_spread_max;

    // Check for immediate Split/Merge opportunities
    let split_merge = detect_split_merge_opportunity(yes_bid, yes_ask, no_bid, no_ask, dec!(0.005));

    // Calculate market making quotes
    // Goal: Yes_quote + No_quote = target_spread (e.g., 1.05)
    let target_mid = (config.target_spread_min + config.target_spread_max) / dec!(2);

    // Simple pricing: each side gets half the profit margin
    let profit_margin = (target_mid - Decimal::ONE) / dec!(2);

    // Our quotes: slightly above current best bids
    let our_yes_ask = yes_bid + profit_margin;
    let our_no_ask = no_bid + profit_margin;

    // Our bids: slightly below current best asks
    let our_yes_bid = yes_ask - profit_margin;
    let our_no_bid = no_ask - profit_margin;

    // Estimated profit if both sides fill
    let estimated_profit = if our_yes_ask + our_no_ask > Decimal::ONE {
        (our_yes_ask + our_no_ask - Decimal::ONE) * config.max_exposure_per_outcome
    } else {
        Decimal::ZERO
    };

    // Determine recommendation
    let recommendation = if let Some(ref sm) = split_merge {
        match sm.opportunity_type {
            SplitMergeType::SplitAndSell => MarketMakingAction::SplitAndSell,
            SplitMergeType::BuyAndMerge => MarketMakingAction::BuyAndMerge,
        }
    } else if spread_in_range
        && estimated_profit >= config.min_profit_margin * config.max_exposure_per_outcome
    {
        MarketMakingAction::PostBothSides {
            yes_quote: (our_yes_bid, our_yes_ask),
            no_quote: (our_no_bid, our_no_ask),
        }
    } else if current_spread < config.target_spread_min {
        MarketMakingAction::Wait {
            reason: format!(
                "Spread {:.2}% too tight (min {:.2}%)",
                (current_spread - Decimal::ONE) * dec!(100),
                (config.target_spread_min - Decimal::ONE) * dec!(100)
            ),
        }
    } else {
        MarketMakingAction::Wait {
            reason: format!(
                "Spread {:.2}% too wide (max {:.2}%)",
                (current_spread - Decimal::ONE) * dec!(100),
                (config.target_spread_max - Decimal::ONE) * dec!(100)
            ),
        }
    };

    MarketMakingOpportunity {
        yes_token_id: String::new(),
        no_token_id: None,
        yes_bid,
        yes_ask,
        no_bid,
        no_ask,
        current_spread,
        spread_in_range,
        estimated_profit,
        split_merge,
        recommendation,
    }
}

/// Analyze a market near settlement for betting opportunities
///
/// Strategy: Buy high-probability outcomes near settlement
/// Key insight: Need true probability > breakeven probability (usually ~98% for 95¢ entry)
pub fn analyze_near_settlement(
    yes_price: Decimal,
    estimated_true_probability: Decimal,
    hours_to_settlement: f64,
) -> NearSettlementAnalysis {
    let time_to_settlement = Duration::hours(hours_to_settlement as i64);

    // Calculate EV
    let ev_analysis = ExpectedValue::calculate(yes_price, estimated_true_probability, None);

    // Calculate minimum probability for +EV
    let min_prob = ExpectedValue::min_probability_for_positive_ev(yes_price, None);

    // Assess risk level
    let risk_level = if hours_to_settlement < 1.0 {
        // Very close to settlement - high risk of adverse events
        if estimated_true_probability > dec!(0.99) && ev_analysis.is_positive_ev {
            RiskLevel::Medium
        } else {
            RiskLevel::VeryHigh
        }
    } else if hours_to_settlement < 6.0 {
        if ev_analysis.is_positive_ev && ev_analysis.roi > dec!(0.01) {
            RiskLevel::High
        } else {
            RiskLevel::VeryHigh
        }
    } else if hours_to_settlement < 24.0 {
        if ev_analysis.is_positive_ev {
            RiskLevel::Medium
        } else {
            RiskLevel::High
        }
    } else if ev_analysis.is_positive_ev && ev_analysis.roi > dec!(0.02) {
        RiskLevel::Low
    } else if ev_analysis.is_positive_ev {
        RiskLevel::Medium
    } else {
        RiskLevel::High
    };

    // Generate recommendation
    let recommendation = if !ev_analysis.is_positive_ev {
        format!(
            "AVOID: Negative EV ({:.2}%). Need {:.1}% true probability for +EV at {:.1}¢.",
            ev_analysis.net_ev * dec!(100),
            min_prob * dec!(100),
            yes_price * dec!(100)
        )
    } else if ev_analysis.kelly_fraction < dec!(0.01) {
        format!(
            "MARGINAL: Barely +EV ({:.2}%). Kelly suggests {:.1}% of bankroll.",
            ev_analysis.net_ev * dec!(100),
            ev_analysis.kelly_fraction * dec!(100)
        )
    } else if risk_level == RiskLevel::VeryHigh || risk_level == RiskLevel::High {
        format!(
            "CAUTION: +EV ({:.2}% ROI) but {} risk. Kelly: {:.1}%. Consider smaller size.",
            ev_analysis.roi * dec!(100),
            risk_level,
            ev_analysis.kelly_fraction * dec!(100)
        )
    } else {
        format!(
            "GO: {:.2}% ROI, {} risk. Kelly suggests {:.1}% of bankroll.",
            ev_analysis.roi * dec!(100),
            risk_level,
            ev_analysis.kelly_fraction * dec!(100)
        )
    };

    NearSettlementAnalysis {
        time_to_settlement,
        hours_remaining: hours_to_settlement,
        yes_price,
        min_probability_for_ev: min_prob,
        ev_analysis,
        risk_level,
        recommendation,
    }
}

/// Display EV calculation details
impl std::fmt::Display for ExpectedValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "EV Analysis @ {:.1}¢ (Est. {:.1}% true prob):\n",
            self.entry_price * dec!(100),
            self.true_probability * dec!(100)
        )?;
        write!(
            f,
            "  Gross EV: {:.4}  Net EV: {:.4}\n",
            self.gross_ev, self.net_ev
        )?;
        write!(
            f,
            "  ROI: {:.2}%  Kelly: {:.1}%\n",
            self.roi * dec!(100),
            self.kelly_fraction * dec!(100)
        )?;
        write!(
            f,
            "  Breakeven: {:.1}%  +EV: {}",
            self.breakeven_prob * dec!(100),
            if self.is_positive_ev { "YES" } else { "NO" }
        )
    }
}

/// EV calculation table for different price/probability scenarios
pub fn generate_ev_table() -> Vec<(Decimal, Vec<(Decimal, ExpectedValue)>)> {
    let prices = [
        dec!(0.90),
        dec!(0.92),
        dec!(0.94),
        dec!(0.95),
        dec!(0.96),
        dec!(0.97),
        dec!(0.98),
        dec!(0.99),
    ];
    let true_probs = [
        dec!(0.92),
        dec!(0.94),
        dec!(0.95),
        dec!(0.96),
        dec!(0.97),
        dec!(0.98),
        dec!(0.99),
        dec!(0.995),
    ];

    prices
        .iter()
        .map(|&price| {
            let evs: Vec<_> = true_probs
                .iter()
                .map(|&prob| (prob, ExpectedValue::calculate(price, prob, None)))
                .collect();
            (price, evs)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_price_level() {
        assert_eq!(Outcome::parse_price_level("↑ 94,000"), Some(dec!(94000)));
        assert_eq!(Outcome::parse_price_level("↓ 86,000"), Some(dec!(86000)));
        assert_eq!(Outcome::parse_price_level("↑ 104,000"), Some(dec!(104000)));
    }

    #[test]
    fn test_direction_parsing() {
        assert_eq!(
            OutcomeDirection::from_symbol("↑ 94,000"),
            Some(OutcomeDirection::Up)
        );
        assert_eq!(
            OutcomeDirection::from_symbol("↓ 86,000"),
            Some(OutcomeDirection::Down)
        );
    }

    #[test]
    fn test_monotonicity_detection() {
        let mut monitor = MultiOutcomeMonitor::new("test", "BTC Price Test");

        // Add outcomes
        monitor.add_outcome("token1".to_string(), "↓ 86,000".to_string());
        monitor.add_outcome("token2".to_string(), "↓ 84,000".to_string());
        monitor.add_outcome("token3".to_string(), "↓ 82,000".to_string());
        monitor.add_outcome("token4".to_string(), "↓ 80,000".to_string());

        // Set prices (with violation: 82k < 80k)
        monitor.update_quote("token1", Some(dec!(0.24)), None, None, None); // 86k: 24%
        monitor.update_quote("token2", Some(dec!(0.049)), None, None, None); // 84k: 4.9%
        monitor.update_quote("token3", Some(dec!(0.012)), None, None, None); // 82k: 1.2%
        monitor.update_quote("token4", Some(dec!(0.013)), None, None, None); // 80k: 1.3% - VIOLATION!

        let violations = monitor.find_monotonicity_violations();
        assert!(
            !violations.is_empty(),
            "Should detect monotonicity violation"
        );
    }
}
