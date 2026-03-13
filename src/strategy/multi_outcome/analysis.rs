use chrono::Duration;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Polymarket fee rate (approximately 2%)
pub const POLYMARKET_FEE_RATE: Decimal = dec!(0.02);

/// Expected Value calculation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedValue {
    pub entry_price: Decimal,
    pub true_probability: Decimal,
    pub win_payout: Decimal,
    pub fee_rate: Decimal,
    pub gross_ev: Decimal,
    pub net_ev: Decimal,
    pub roi: Decimal,
    pub kelly_fraction: Decimal,
    pub breakeven_prob: Decimal,
    pub is_positive_ev: bool,
}

impl ExpectedValue {
    pub fn calculate(
        entry_price: Decimal,
        true_probability: Decimal,
        fee_rate: Option<Decimal>,
    ) -> Self {
        let fee = fee_rate.unwrap_or(POLYMARKET_FEE_RATE);
        let win_payout = Decimal::ONE;

        let gross_profit_on_win = win_payout - entry_price;
        let net_profit_on_win = gross_profit_on_win * (Decimal::ONE - fee);
        let loss_on_lose = entry_price;

        let gross_ev = true_probability * gross_profit_on_win
            - (Decimal::ONE - true_probability) * loss_on_lose;
        let net_ev =
            true_probability * net_profit_on_win - (Decimal::ONE - true_probability) * loss_on_lose;

        let roi = if entry_price > Decimal::ZERO {
            net_ev / entry_price
        } else {
            Decimal::ZERO
        };

        let b = net_profit_on_win / entry_price;
        let p = true_probability;
        let q = Decimal::ONE - true_probability;
        let kelly = if b > Decimal::ZERO {
            (b * p - q) / b
        } else {
            Decimal::ZERO
        };

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

    pub fn min_probability_for_positive_ev(
        entry_price: Decimal,
        fee_rate: Option<Decimal>,
    ) -> Decimal {
        let fee = fee_rate.unwrap_or(POLYMARKET_FEE_RATE);
        let profit_on_win = (Decimal::ONE - entry_price) * (Decimal::ONE - fee);
        let loss_on_lose = entry_price;

        if profit_on_win + loss_on_lose > Decimal::ZERO {
            loss_on_lose / (profit_on_win + loss_on_lose)
        } else {
            Decimal::ONE
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitMergeOpportunity {
    pub opportunity_type: SplitMergeType,
    pub yes_ask: Decimal,
    pub no_ask: Decimal,
    pub yes_bid: Decimal,
    pub no_bid: Decimal,
    pub profit_per_dollar: Decimal,
    pub estimated_slippage: Decimal,
    pub net_profit: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitMergeType {
    SplitAndSell,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NearSettlementAnalysis {
    pub time_to_settlement: Duration,
    pub hours_remaining: f64,
    pub yes_price: Decimal,
    pub min_probability_for_ev: Decimal,
    pub ev_analysis: ExpectedValue,
    pub risk_level: RiskLevel,
    pub recommendation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    VeryHigh,
    High,
    Medium,
    Low,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketMakingConfig {
    pub target_spread_min: Decimal,
    pub target_spread_max: Decimal,
    pub max_exposure_per_outcome: Decimal,
    pub max_total_exposure: Decimal,
    pub rebalance_threshold: Decimal,
    pub min_profit_margin: Decimal,
}

impl Default for MarketMakingConfig {
    fn default() -> Self {
        Self {
            target_spread_min: dec!(1.02),
            target_spread_max: dec!(1.08),
            max_exposure_per_outcome: dec!(500),
            max_total_exposure: dec!(2000),
            rebalance_threshold: dec!(0.2),
            min_profit_margin: dec!(0.01),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketMakingOpportunity {
    pub yes_token_id: String,
    pub no_token_id: Option<String>,
    pub yes_bid: Decimal,
    pub yes_ask: Decimal,
    pub no_bid: Decimal,
    pub no_ask: Decimal,
    pub current_spread: Decimal,
    pub spread_in_range: bool,
    pub estimated_profit: Decimal,
    pub split_merge: Option<SplitMergeOpportunity>,
    pub recommendation: MarketMakingAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketMakingAction {
    PostBothSides {
        yes_quote: (Decimal, Decimal),
        no_quote: (Decimal, Decimal),
    },
    SplitAndSell,
    BuyAndMerge,
    Rebalance {
        sell_side: String,
        buy_side: String,
    },
    Wait {
        reason: String,
    },
}

pub fn detect_split_merge_opportunity(
    yes_bid: Decimal,
    yes_ask: Decimal,
    no_bid: Decimal,
    no_ask: Decimal,
    slippage_estimate: Decimal,
) -> Option<SplitMergeOpportunity> {
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

    let split_merge =
        detect_split_merge_opportunity(yes_bid, yes_ask, no_bid, no_ask, dec!(0.005));

    let target_mid = (config.target_spread_min + config.target_spread_max) / dec!(2);
    let profit_margin = (target_mid - Decimal::ONE) / dec!(2);

    let our_yes_ask = yes_bid + profit_margin;
    let our_no_ask = no_bid + profit_margin;
    let our_yes_bid = yes_ask - profit_margin;
    let our_no_bid = no_ask - profit_margin;

    let estimated_profit = if our_yes_ask + our_no_ask > Decimal::ONE {
        (our_yes_ask + our_no_ask - Decimal::ONE) * config.max_exposure_per_outcome
    } else {
        Decimal::ZERO
    };

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

pub fn analyze_near_settlement(
    yes_price: Decimal,
    estimated_true_probability: Decimal,
    hours_to_settlement: f64,
) -> NearSettlementAnalysis {
    let time_to_settlement = Duration::hours(hours_to_settlement as i64);

    let ev_analysis = ExpectedValue::calculate(yes_price, estimated_true_probability, None);
    let min_prob = ExpectedValue::min_probability_for_positive_ev(yes_price, None);

    let risk_level = if hours_to_settlement < 1.0 {
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
    fn test_detect_split_merge_opportunity_flags_buy_and_merge() {
        let opportunity = detect_split_merge_opportunity(
            dec!(0.48),
            dec!(0.47),
            dec!(0.46),
            dec!(0.48),
            dec!(0.005),
        )
        .expect("buy-and-merge opportunity");

        assert_eq!(opportunity.opportunity_type, SplitMergeType::BuyAndMerge);
        assert!(opportunity.net_profit > Decimal::ZERO);
    }

    #[test]
    fn test_analyze_near_settlement_marks_negative_ev_as_high_risk() {
        let analysis = analyze_near_settlement(dec!(0.95), dec!(0.94), 2.0);

        assert!(!analysis.ev_analysis.is_positive_ev);
        assert!(matches!(
            analysis.risk_level,
            RiskLevel::VeryHigh | RiskLevel::High
        ));
        assert!(analysis.recommendation.starts_with("AVOID:"));
    }
}
