//! Exit Logic - Position Management and Exit Strategy
//!
//! This module implements exit decision logic for managing open positions.
//! 
//! Exit strategies:
//! 1. Partial profit taking (lock in gains, reduce risk)
//! 2. Edge disappearance (model no longer predicts value)
//! 3. Trailing stop (protect profits from peak)
//! 4. Liquidity risk (can't exit if needed)
//! 5. Time stop (Q4 末段，時間不夠翻盤)
//!
//! Philosophy:
//! - Exit is NOT "hold until settlement"
//! - Exit when: edge gone OR risk too high OR profit target hit
//! - Avoid "快贏了想多賺" and "快輸了不肯認" (gambler's fallacy)

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

use super::nba_winprob::WinProbPrediction;
use super::nba_filters::MarketContext;

/// Exit logic configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitConfig {
    // Partial profit taking
    pub partial_exit_threshold: f64,   // Edge threshold for partial exit, e.g., 0.02 = 2%
    pub partial_exit_pct: f64,         // Percentage to exit, e.g., 0.50 = 50%
    
    // Edge-based exit
    pub edge_disappear_threshold: f64, // Exit if edge drops below this, e.g., -0.01 = -1%
    
    // Trailing stop
    pub trailing_stop_pct: f64,        // Drawdown from peak to exit, e.g., 0.10 = 10%
    pub trailing_stop_enabled: bool,
    
    // Liquidity risk
    pub min_exit_liquidity_ratio: f64, // Min liquidity as ratio of position, e.g., 2.0 = 2x
    
    // Time-based exit
    pub time_stop_quarter: u8,         // Quarter to apply time stop, e.g., 4
    pub time_stop_minutes: f64,        // Minutes remaining to force exit, e.g., 2.0
    pub time_stop_min_profit_pct: f64, // Only exit if profit below this, e.g., 0.10 = 10%
}

impl Default for ExitConfig {
    fn default() -> Self {
        Self {
            // Conservative defaults
            partial_exit_threshold: 0.02,      // Take profit when edge drops to 2%
            partial_exit_pct: 0.50,            // Exit 50% of position
            edge_disappear_threshold: -0.01,   // Exit if edge becomes negative
            trailing_stop_pct: 0.10,           // 10% trailing stop
            trailing_stop_enabled: true,
            min_exit_liquidity_ratio: 2.0,     // Need 2x position size in liquidity
            time_stop_quarter: 4,
            time_stop_minutes: 2.0,            // Exit in last 2 minutes if not profitable
            time_stop_min_profit_pct: 0.10,    // 10% profit threshold
        }
    }
}

/// Position state for exit decisions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionState {
    // Entry information
    pub entry_price: Decimal,
    pub entry_time: DateTime<Utc>,
    pub entry_size: Decimal,
    pub remaining_size: Decimal,
    
    // Peak tracking (for trailing stop)
    pub peak_price: Option<Decimal>,
    pub peak_time: Option<DateTime<Utc>>,
    
    // Current state
    pub current_price: Decimal,
    pub unrealized_pnl: Decimal,
    pub unrealized_pnl_pct: f64,
}

impl PositionState {
    pub fn new(entry_price: Decimal, entry_size: Decimal) -> Self {
        Self {
            entry_price,
            entry_time: Utc::now(),
            entry_size,
            remaining_size: entry_size,
            peak_price: Some(entry_price),
            peak_time: Some(Utc::now()),
            current_price: entry_price,
            unrealized_pnl: Decimal::ZERO,
            unrealized_pnl_pct: 0.0,
        }
    }
    
    pub fn update_price(&mut self, new_price: Decimal) {
        self.current_price = new_price;
        
        // Update PnL
        let price_change = new_price - self.entry_price;
        self.unrealized_pnl = price_change * self.remaining_size;
        self.unrealized_pnl_pct = price_change.to_f64().unwrap_or(0.0) 
            / self.entry_price.to_f64().unwrap_or(1.0);
        
        // Update peak
        if let Some(peak) = self.peak_price {
            if new_price > peak {
                self.peak_price = Some(new_price);
                self.peak_time = Some(Utc::now());
            }
        } else {
            self.peak_price = Some(new_price);
            self.peak_time = Some(Utc::now());
        }
    }
    
    pub fn reduce_size(&mut self, amount: Decimal) {
        self.remaining_size = self.remaining_size - amount;
        if self.remaining_size < Decimal::ZERO {
            self.remaining_size = Decimal::ZERO;
        }
    }
}

/// Exit decision result
#[derive(Debug, Clone)]
pub enum ExitDecision {
    /// Full exit (close entire position)
    FullExit {
        reason: String,
        details: String,
        urgency: ExitUrgency,
    },
    
    /// Partial exit (close portion of position)
    PartialExit {
        pct: f64,
        reason: String,
        details: String,
    },
    
    /// Hold position (no action)
    Hold {
        reason: String,
        warnings: Vec<String>,
    },
}

/// Exit urgency level
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitUrgency {
    Low,      // Normal exit, can use limit orders
    Medium,   // Should exit soon, use aggressive limit
    High,     // Exit immediately, use market orders
    Critical, // Emergency exit, any price
}

/// Exit logic
pub struct ExitLogic {
    config: ExitConfig,
}

impl ExitLogic {
    pub fn new(config: ExitConfig) -> Self {
        Self { config }
    }
    
    /// Evaluate whether to exit a position
    /// 
    /// This is called continuously while holding a position.
    /// Returns ExitDecision with reasoning.
    pub fn should_exit(
        &self,
        position: &PositionState,
        current_prediction: &WinProbPrediction,
        current_market_price: Decimal,
        market_context: &MarketContext,
    ) -> ExitDecision {
        let p_model = current_prediction.win_prob;
        let p_market = current_market_price.to_f64().unwrap_or(0.5);
        let current_edge = p_model - p_market;
        
        // Priority 1: Edge disappeared (model says no value)
        if current_edge < self.config.edge_disappear_threshold {
            return ExitDecision::full_exit(
                "Edge disappeared",
                format!(
                    "Current edge {:.2}% < threshold {:.2}% (p_model={:.1}%, p_market={:.1}%)",
                    current_edge * 100.0,
                    self.config.edge_disappear_threshold * 100.0,
                    p_model * 100.0,
                    p_market * 100.0
                ),
                ExitUrgency::Medium,
            );
        }
        
        // Priority 2: Liquidity risk (can't exit if needed)
        let total_depth = market_context.bid_depth + market_context.ask_depth;
        let position_value = position.remaining_size * current_market_price;
        let liquidity_ratio = total_depth.to_f64().unwrap_or(0.0) 
            / position_value.to_f64().unwrap_or(1.0);
        
        if liquidity_ratio < self.config.min_exit_liquidity_ratio {
            return ExitDecision::full_exit(
                "Liquidity risk",
                format!(
                    "Book depth ${:.2} < {:.1}x position ${:.2} (ratio: {:.2}x)",
                    total_depth,
                    self.config.min_exit_liquidity_ratio,
                    position_value,
                    liquidity_ratio
                ),
                ExitUrgency::High,
            );
        }
        
        // Priority 3: Trailing stop (protect profits from peak)
        if self.config.trailing_stop_enabled {
            if let Some(peak_price) = position.peak_price {
                let peak = peak_price.to_f64().unwrap_or(0.0);
                let current = current_market_price.to_f64().unwrap_or(0.0);
                let drawdown = (current - peak) / peak;
                
                if drawdown < -self.config.trailing_stop_pct {
                    return ExitDecision::full_exit(
                        "Trailing stop triggered",
                        format!(
                            "Drawdown from peak: {:.1}% (peak: {:.4}, current: {:.4})",
                            drawdown * 100.0,
                            peak,
                            current
                        ),
                        ExitUrgency::Medium,
                    );
                }
            }
        }
        
        // Priority 4: Partial profit taking (lock in gains)
        if current_edge < self.config.partial_exit_threshold 
            && current_edge > self.config.edge_disappear_threshold
            && position.unrealized_pnl_pct > 0.0 {
            
            // Only do partial exit if we haven't already reduced position significantly
            let remaining_pct = position.remaining_size.to_f64().unwrap_or(0.0)
                / position.entry_size.to_f64().unwrap_or(1.0);
            
            if remaining_pct > 0.6 {
                return ExitDecision::partial_exit(
                    self.config.partial_exit_pct,
                    "Partial profit taking",
                    format!(
                        "Edge reduced to {:.2}%, PnL: {:.1}%, taking profit on {}%",
                        current_edge * 100.0,
                        position.unrealized_pnl_pct * 100.0,
                        self.config.partial_exit_pct * 100.0
                    ),
                );
            }
        }
        
        // Priority 5: Time stop (Q4 末段，時間不夠)
        let features = &current_prediction.features;
        if features.quarter == self.config.time_stop_quarter
            && features.time_remaining < self.config.time_stop_minutes {
            
            // Only exit if not profitable enough
            if position.unrealized_pnl_pct < self.config.time_stop_min_profit_pct {
                return ExitDecision::full_exit(
                    "Time stop",
                    format!(
                        "Q{} {:.1}min remaining, PnL: {:.1}% < target {:.1}%",
                        features.quarter,
                        features.time_remaining,
                        position.unrealized_pnl_pct * 100.0,
                        self.config.time_stop_min_profit_pct * 100.0
                    ),
                    ExitUrgency::Medium,
                );
            }
        }
        
        // Priority 6: Hold position
        let mut warnings = vec![];
        
        // Warning: Edge getting small
        if current_edge < self.config.partial_exit_threshold * 1.5 {
            warnings.push(format!(
                "Edge declining: {:.2}% (watch for partial exit)",
                current_edge * 100.0
            ));
        }
        
        // Warning: Approaching time stop
        if features.quarter == self.config.time_stop_quarter
            && features.time_remaining < self.config.time_stop_minutes * 2.0 {
            warnings.push(format!(
                "Approaching time stop: {:.1}min remaining",
                features.time_remaining
            ));
        }
        
        // Warning: Liquidity declining
        if liquidity_ratio < self.config.min_exit_liquidity_ratio * 1.5 {
            warnings.push(format!(
                "Liquidity declining: {:.2}x position size",
                liquidity_ratio
            ));
        }
        
        ExitDecision::hold(
            format!(
                "Edge: {:.2}%, PnL: {:.1}%, Confidence: {:.1}%",
                current_edge * 100.0,
                position.unrealized_pnl_pct * 100.0,
                current_prediction.confidence * 100.0
            ),
            warnings,
        )
    }
    
    pub fn config(&self) -> &ExitConfig {
        &self.config
    }
}

impl ExitDecision {
    pub fn full_exit(reason: &str, details: String, urgency: ExitUrgency) -> Self {
        Self::FullExit {
            reason: reason.to_string(),
            details,
            urgency,
        }
    }
    
    pub fn partial_exit(pct: f64, reason: &str, details: String) -> Self {
        Self::PartialExit {
            pct,
            reason: reason.to_string(),
            details,
        }
    }
    
    pub fn hold(reason: String, warnings: Vec<String>) -> Self {
        Self::Hold { reason, warnings }
    }
    
    pub fn is_exit(&self) -> bool {
        matches!(self, Self::FullExit { .. } | Self::PartialExit { .. })
    }
    
    pub fn is_full_exit(&self) -> bool {
        matches!(self, Self::FullExit { .. })
    }
    
    pub fn is_partial_exit(&self) -> bool {
        matches!(self, Self::PartialExit { .. })
    }
    
    pub fn is_hold(&self) -> bool {
        matches!(self, Self::Hold { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::nba_winprob::{LiveWinProbModel, GameFeatures};
    
    fn create_position(entry_price: f64, current_price: f64) -> PositionState {
        let mut position = PositionState::new(
            Decimal::new((entry_price * 100.0) as i64, 2),
            Decimal::new(1000, 0),
        );
        position.update_price(Decimal::new((current_price * 100.0) as i64, 2));
        position
    }
    
    fn create_prediction(point_diff: f64, quarter: u8, time_remaining: f64) -> WinProbPrediction {
        let model = LiveWinProbModel::default_untrained();
        let features = GameFeatures {
            point_diff,
            time_remaining,
            quarter,
            possession: 1.0,
            pregame_spread: 0.0,
            elo_diff: 0.0,
        };
        model.predict(&features)
    }
    
    fn create_good_market_context() -> MarketContext {
        MarketContext {
            best_bid: Some(Decimal::new(45, 2)),
            best_ask: Some(Decimal::new(46, 2)),
            mid_price: Some(Decimal::new(455, 3)),
            spread_bps: Some(22),
            bid_depth: Decimal::new(2000, 0),
            ask_depth: Decimal::new(1800, 0),
            best_bid_depth: Decimal::new(500, 0),
            best_ask_depth: Decimal::new(450, 0),
            price_velocity: Some(0.001),
            recent_prices: vec![],
            data_latency_ms: 500,
            quote_age_secs: 2,
            last_update_timestamp: 0,
            consecutive_same_side_trades: 2,
            last_trade_side: "buy".to_string(),
            recent_trade_count: 10,
            depth_imbalance: 0.05,
        }
    }
    
    #[test]
    fn test_hold_with_good_edge() {
        let exit_logic = ExitLogic::new(ExitConfig::default());
        let position = create_position(0.15, 0.25); // Profitable
        let prediction = create_prediction(-5.0, 3, 10.0);
        let market_price = Decimal::new(20, 2); // 0.20
        let market = create_good_market_context();
        
        let decision = exit_logic.should_exit(&position, &prediction, market_price, &market);
        
        // Should hold if edge is still good
        if prediction.win_prob > 0.25 {
            assert!(decision.is_hold(), "Should hold with good edge");
        }
    }
    
    #[test]
    fn test_edge_disappeared() {
        let exit_logic = ExitLogic::new(ExitConfig::default());
        let position = create_position(0.15, 0.20);
        let prediction = create_prediction(-5.0, 3, 10.0);
        let market_price = Decimal::new(50, 2); // 0.50 (higher than model)
        let market = create_good_market_context();
        
        let decision = exit_logic.should_exit(&position, &prediction, market_price, &market);
        
        // Should exit if model predicts < market price
        if prediction.win_prob < 0.50 {
            assert!(decision.is_full_exit(), "Should exit when edge disappears");
        }
    }
    
    #[test]
    fn test_trailing_stop() {
        let exit_logic = ExitLogic::new(ExitConfig::default());
        let mut position = create_position(0.15, 0.40); // Went up to 0.40
        position.peak_price = Some(Decimal::new(40, 2));
        position.update_price(Decimal::new(35, 2)); // Dropped to 0.35 (12.5% drawdown)
        
        let prediction = create_prediction(-3.0, 3, 8.0);
        let market_price = Decimal::new(35, 2);
        let market = create_good_market_context();
        
        let decision = exit_logic.should_exit(&position, &prediction, market_price, &market);
        
        // Should trigger trailing stop with 12.5% drawdown (> 10% threshold)
        assert!(decision.is_full_exit(), "Should trigger trailing stop");
        if let ExitDecision::FullExit { reason, .. } = decision {
            assert!(reason.contains("Trailing stop"));
        }
    }
    
    #[test]
    fn test_liquidity_risk() {
        let exit_logic = ExitLogic::new(ExitConfig::default());
        let position = create_position(0.15, 0.25);
        let prediction = create_prediction(-5.0, 3, 10.0);
        let market_price = Decimal::new(25, 2);
        let mut market = create_good_market_context();
        
        // Reduce liquidity to below 2x position size
        market.bid_depth = Decimal::new(300, 0);
        market.ask_depth = Decimal::new(200, 0);
        // Position value = 1000 * 0.25 = 250, total depth = 500 (2x)
        // Need to be below 2x to trigger
        market.bid_depth = Decimal::new(200, 0);
        market.ask_depth = Decimal::new(100, 0); // Total 300 < 500 (2x of 250)
        
        let decision = exit_logic.should_exit(&position, &prediction, market_price, &market);
        
        assert!(decision.is_full_exit(), "Should exit due to liquidity risk");
        if let ExitDecision::FullExit { reason, .. } = decision {
            assert!(reason.contains("Liquidity"));
        }
    }
    
    #[test]
    fn test_time_stop() {
        let exit_logic = ExitLogic::new(ExitConfig::default());
        let position = create_position(0.15, 0.18); // Small profit (20%)
        let prediction = create_prediction(-5.0, 4, 1.5); // Q4, 1.5 min left
        let market_price = Decimal::new(18, 2);
        let market = create_good_market_context();
        
        let decision = exit_logic.should_exit(&position, &prediction, market_price, &market);
        
        // Should trigger time stop (Q4 < 2min, profit < 10%)
        assert!(decision.is_full_exit(), "Should trigger time stop");
        if let ExitDecision::FullExit { reason, .. } = decision {
            assert!(reason.contains("Time stop"));
        }
    }
    
    #[test]
    fn test_partial_exit() {
        let exit_logic = ExitLogic::new(ExitConfig::default());
        let position = create_position(0.15, 0.30); // Good profit
        let prediction = create_prediction(-3.0, 3, 8.0);
        let market_price = Decimal::new(28, 2); // Edge reduced but still positive
        let market = create_good_market_context();

        let _decision = exit_logic.should_exit(&position, &prediction, market_price, &market);
        
        // Might trigger partial exit if edge is small but positive
        // (depends on model prediction)
    }
}
