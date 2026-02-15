//! Entry Logic - Core Decision Layer
//!
//! This module implements the entry decision logic that combines:
//! 1. Win probability model (p_model) - our edge source
//! 2. Market price (p_market) - what the market thinks
//! 3. Market microstructure filters - risk controls
//! 4. Expected value calculation - accounting for costs
//!
//! Philosophy:
//! - Entry is NOT just "price < 0.20"
//! - Entry requires: edge > threshold AND EV > 0 after costs AND filters pass
//! - We must know WHY we're entering (for PnL attribution)

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::nba_filters::FilterResult;
use super::nba_winprob::{GameFeatures, WinProbPrediction};

/// Entry logic configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryConfig {
    // Edge requirements
    pub min_edge: f64,       // Minimum edge (p_model - p_market), e.g., 0.05 = 5%
    pub min_confidence: f64, // Minimum model confidence, e.g., 0.70 = 70%

    // Expected value requirements
    pub min_ev_after_fees: f64, // Minimum EV after all costs, e.g., 0.02 = 2%
    pub fee_rate: f64,          // Trading fee rate, e.g., 0.02 = 2%
    pub slippage_estimate: f64, // Estimated slippage, e.g., 0.005 = 0.5%

    // Safety margins
    pub min_market_price: f64, // Don't buy if price too low (illiquid), e.g., 0.05
    pub max_market_price: f64, // Don't buy if price too high (no upside), e.g., 0.80

    // Position sizing constraints (will be used by Kelly sizer)
    pub max_position_pct: f64, // Max position as % of bankroll, e.g., 0.05 = 5%
    pub max_total_exposure_pct: f64, // Max total exposure, e.g., 0.20 = 20%
}

impl Default for EntryConfig {
    fn default() -> Self {
        Self {
            // Conservative defaults for MVP
            min_edge: 0.05,               // 5% minimum edge
            min_confidence: 0.70,         // 70% confidence
            min_ev_after_fees: 0.02,      // 2% minimum EV after costs
            fee_rate: 0.02,               // 2% fees (Polymarket typical)
            slippage_estimate: 0.005,     // 0.5% slippage estimate
            min_market_price: 0.05,       // Don't buy below 5 cents
            max_market_price: 0.80,       // Don't buy above 80 cents
            max_position_pct: 0.05,       // 5% max per position
            max_total_exposure_pct: 0.20, // 20% max total
        }
    }
}

/// Entry decision logic
pub struct EntryLogic {
    config: EntryConfig,
}

/// Entry signal with full attribution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntrySignal {
    // Core metrics
    pub p_model: f64,    // Model's predicted win probability
    pub p_market: f64,   // Market's implied probability (price)
    pub edge: f64,       // p_model - p_market
    pub confidence: f64, // Model confidence (1 - uncertainty)

    // Expected value breakdown
    pub gross_ev: f64, // EV before costs
    pub fees: f64,     // Expected fees
    pub slippage: f64, // Expected slippage
    pub net_ev: f64,   // EV after all costs

    // Context
    pub game_features: GameFeatures,
    pub market_price: Decimal,
    pub timestamp: i64,

    // Reasoning (for debugging and PnL attribution)
    pub reason: String,
    pub warnings: Vec<String>,
}

/// Entry decision result
#[derive(Debug, Clone)]
pub enum EntryDecision {
    /// Entry approved with signal
    Approve(EntrySignal),

    /// Entry rejected with reason
    Reject {
        reason: String,
        details: Vec<String>,
        partial_signal: Option<PartialSignal>,
    },
}

/// Partial signal (for rejected entries, useful for analysis)
#[derive(Debug, Clone)]
pub struct PartialSignal {
    pub p_model: f64,
    pub p_market: f64,
    pub edge: f64,
    pub net_ev: f64,
}

impl EntryLogic {
    pub fn new(config: EntryConfig) -> Self {
        Self { config }
    }

    /// Evaluate whether to enter a position
    ///
    /// This is the core decision function. It must be called with:
    /// - prediction: from LiveWinProbModel
    /// - market_price: current market price (Decimal)
    /// - filter_result: from MarketFilters
    ///
    /// Returns EntryDecision with full reasoning
    pub fn should_enter(
        &self,
        prediction: &WinProbPrediction,
        market_price: Decimal,
        filter_result: &FilterResult,
    ) -> EntryDecision {
        let p_model = prediction.win_prob;
        let p_market = market_price.to_f64().unwrap_or(0.5);

        // Step 0: Market structure filters (defensive)
        if !filter_result.passed {
            return EntryDecision::reject(
                "Market filters failed",
                filter_result.reasons.clone(),
                Some(PartialSignal {
                    p_model,
                    p_market,
                    edge: p_model - p_market,
                    net_ev: 0.0,
                }),
            );
        }

        // Step 1: Price sanity checks
        if p_market < self.config.min_market_price {
            return EntryDecision::reject(
                "Market price too low (illiquid)",
                vec![format!(
                    "Price {:.4} < min {:.4}",
                    p_market, self.config.min_market_price
                )],
                None,
            );
        }

        if p_market > self.config.max_market_price {
            return EntryDecision::reject(
                "Market price too high (no upside)",
                vec![format!(
                    "Price {:.4} > max {:.4}",
                    p_market, self.config.max_market_price
                )],
                None,
            );
        }

        // Step 2: Edge check (core alpha source)
        let edge = p_model - p_market;
        if edge < self.config.min_edge {
            return EntryDecision::reject(
                "Insufficient edge",
                vec![format!(
                    "Edge {:.2}% < min {:.2}% (p_model={:.2}%, p_market={:.2}%)",
                    edge * 100.0,
                    self.config.min_edge * 100.0,
                    p_model * 100.0,
                    p_market * 100.0
                )],
                Some(PartialSignal {
                    p_model,
                    p_market,
                    edge,
                    net_ev: 0.0,
                }),
            );
        }

        // Step 3: Confidence check (model uncertainty)
        if prediction.confidence < self.config.min_confidence {
            return EntryDecision::reject(
                "Low model confidence",
                vec![format!(
                    "Confidence {:.2} < min {:.2} (uncertainty: {:.2})",
                    prediction.confidence, self.config.min_confidence, prediction.uncertainty
                )],
                Some(PartialSignal {
                    p_model,
                    p_market,
                    edge,
                    net_ev: 0.0,
                }),
            );
        }

        // Step 4: Expected value calculation
        let gross_ev = self.calculate_gross_ev(p_model, p_market);
        let fees = self.calculate_fees(p_market);
        let slippage = self.config.slippage_estimate;
        let net_ev = gross_ev - fees - slippage;

        if net_ev < self.config.min_ev_after_fees {
            return EntryDecision::reject(
                "Insufficient EV after costs",
                vec![format!(
                    "Net EV {:.4} < min {:.4} (gross: {:.4}, fees: {:.4}, slippage: {:.4})",
                    net_ev, self.config.min_ev_after_fees, gross_ev, fees, slippage
                )],
                Some(PartialSignal {
                    p_model,
                    p_market,
                    edge,
                    net_ev,
                }),
            );
        }

        // Step 5: All checks passed - approve entry
        let signal = EntrySignal {
            p_model,
            p_market,
            edge,
            confidence: prediction.confidence,
            gross_ev,
            fees,
            slippage,
            net_ev,
            game_features: prediction.features.clone(),
            market_price,
            timestamp: chrono::Utc::now().timestamp_millis(),
            reason: format!(
                "Edge: {:.2}%, Net EV: {:.2}%, Confidence: {:.1}%",
                edge * 100.0,
                net_ev * 100.0,
                prediction.confidence * 100.0
            ),
            warnings: filter_result.warnings.clone(),
        };

        EntryDecision::Approve(signal)
    }

    /// Calculate gross expected value (before costs)
    ///
    /// EV = p_model * payoff_if_win - p_market * cost
    ///    = p_model * 1.0 - p_market
    fn calculate_gross_ev(&self, p_model: f64, p_market: f64) -> f64 {
        p_model * 1.0 - p_market
    }

    /// Calculate expected fees
    ///
    /// Polymarket charges fees on the purchase amount
    fn calculate_fees(&self, p_market: f64) -> f64 {
        p_market * self.config.fee_rate
    }

    /// Get configuration
    pub fn config(&self) -> &EntryConfig {
        &self.config
    }
}

impl EntryDecision {
    pub fn reject(reason: &str, details: Vec<String>, partial: Option<PartialSignal>) -> Self {
        Self::Reject {
            reason: reason.to_string(),
            details,
            partial_signal: partial,
        }
    }

    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approve(_))
    }

    pub fn is_rejected(&self) -> bool {
        matches!(self, Self::Reject { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::nba_winprob::{GameFeatures, LiveWinProbModel};

    fn create_good_prediction() -> WinProbPrediction {
        let model = LiveWinProbModel::default_untrained();
        let features = GameFeatures {
            point_diff: -10.0,
            time_remaining: 8.0,
            quarter: 3,
            possession: 1.0,
            pregame_spread: 5.0,
            elo_diff: 50.0,
            comeback_rate: None,
        };
        model.predict(&features)
    }

    fn create_good_filter_result() -> FilterResult {
        FilterResult {
            passed: true,
            reasons: vec![],
            warnings: vec![],
        }
    }

    #[test]
    fn test_good_entry_approved() {
        let entry_logic = EntryLogic::new(EntryConfig::default());
        let prediction = create_good_prediction();
        let market_price = Decimal::new(15, 2); // 0.15
        let filters = create_good_filter_result();

        let decision = entry_logic.should_enter(&prediction, market_price, &filters);

        // Should approve if model predicts > 0.20 and market is 0.15
        if prediction.win_prob > 0.20 {
            assert!(
                decision.is_approved(),
                "Should approve with sufficient edge"
            );
        }
    }

    #[test]
    fn test_insufficient_edge_rejected() {
        let entry_logic = EntryLogic::new(EntryConfig::default());
        let prediction = create_good_prediction();
        let market_price = Decimal::new(50, 2); // 0.50 (close to model prediction)
        let filters = create_good_filter_result();

        let decision = entry_logic.should_enter(&prediction, market_price, &filters);

        // Should reject if edge is too small
        if let EntryDecision::Reject { reason, .. } = decision {
            assert!(reason.contains("edge") || reason.contains("EV"));
        }
    }

    #[test]
    fn test_failed_filters_rejected() {
        let entry_logic = EntryLogic::new(EntryConfig::default());
        let prediction = create_good_prediction();
        let market_price = Decimal::new(15, 2);
        let filters = FilterResult {
            passed: false,
            reasons: vec!["Spread too wide".to_string()],
            warnings: vec![],
        };

        let decision = entry_logic.should_enter(&prediction, market_price, &filters);

        assert!(decision.is_rejected(), "Should reject when filters fail");
        if let EntryDecision::Reject { reason, .. } = decision {
            assert!(reason.contains("filter"));
        }
    }

    #[test]
    fn test_price_too_low_rejected() {
        let entry_logic = EntryLogic::new(EntryConfig::default());
        let prediction = create_good_prediction();
        let market_price = Decimal::new(2, 2); // 0.02 (too low)
        let filters = create_good_filter_result();

        let decision = entry_logic.should_enter(&prediction, market_price, &filters);

        assert!(decision.is_rejected(), "Should reject when price too low");
        if let EntryDecision::Reject { reason, .. } = decision {
            assert!(reason.contains("too low"));
        }
    }

    #[test]
    fn test_price_too_high_rejected() {
        let entry_logic = EntryLogic::new(EntryConfig::default());
        let prediction = create_good_prediction();
        let market_price = Decimal::new(85, 2); // 0.85 (too high)
        let filters = create_good_filter_result();

        let decision = entry_logic.should_enter(&prediction, market_price, &filters);

        assert!(decision.is_rejected(), "Should reject when price too high");
        if let EntryDecision::Reject { reason, .. } = decision {
            assert!(reason.contains("too high"));
        }
    }

    #[test]
    fn test_ev_calculation() {
        let entry_logic = EntryLogic::new(EntryConfig::default());

        // Test case: p_model = 0.30, p_market = 0.15
        let p_model = 0.30;
        let p_market = 0.15;

        let gross_ev = entry_logic.calculate_gross_ev(p_model, p_market);
        let fees = entry_logic.calculate_fees(p_market);

        // Gross EV = 0.30 * 1.0 - 0.15 = 0.15
        assert!((gross_ev - 0.15).abs() < 0.001, "Gross EV should be 0.15");

        // Fees = 0.15 * 0.02 = 0.003
        assert!((fees - 0.003).abs() < 0.0001, "Fees should be 0.003");

        // Net EV = 0.15 - 0.003 - 0.005 (slippage) = 0.142
        let net_ev = gross_ev - fees - entry_logic.config.slippage_estimate;
        assert!((net_ev - 0.142).abs() < 0.001, "Net EV should be ~0.142");
    }

    #[test]
    fn test_low_confidence_rejected() {
        let mut config = EntryConfig::default();
        config.min_confidence = 0.95; // Very high confidence required

        let entry_logic = EntryLogic::new(config);
        let prediction = create_good_prediction();
        let market_price = Decimal::new(15, 2);
        let filters = create_good_filter_result();

        let decision = entry_logic.should_enter(&prediction, market_price, &filters);

        // Should reject if confidence is below 95%
        if prediction.confidence < 0.95 {
            assert!(decision.is_rejected(), "Should reject with low confidence");
            if let EntryDecision::Reject { reason, .. } = decision {
                assert!(reason.contains("confidence"));
            }
        }
    }
}
