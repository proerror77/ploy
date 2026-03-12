//! Delta rebalancing engine for gamma scalping straddles.
//!
//! Tracks straddle positions and decides when to rebalance delta exposure
//! by selling the winning token and buying the losing token.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::config::GammaScalpingConfig;
use super::greeks::{straddle_delta, BinaryGreeks};

/// A live straddle position (long UP + long DOWN tokens on the same event).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Straddle {
    pub event_id: String,
    pub symbol: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub up_shares: u64,
    pub down_shares: u64,
    pub up_entry_price: Decimal,
    pub down_entry_price: Decimal,
    pub entry_time: DateTime<Utc>,
    pub expiry_time: DateTime<Utc>,
    pub last_rebalance: DateTime<Utc>,
    pub rebalance_count: u32,
    /// Accumulated P&L from rebalance trades
    pub realized_pnl: Decimal,
    /// Total cost basis (entry cost of both legs)
    pub cost_basis: Decimal,
}

impl Straddle {
    /// Total shares across both legs.
    pub fn total_shares(&self) -> u64 {
        self.up_shares + self.down_shares
    }

    /// Time remaining until expiry in seconds.
    pub fn time_remaining_secs(&self, now: DateTime<Utc>) -> f64 {
        (self.expiry_time - now).num_milliseconds().max(0) as f64 / 1000.0
    }

    /// Infer the event window duration (5m or 15m) from the event title or default to 900s.
    pub fn window_secs(&self) -> f64 {
        900.0 // Default to 15m; can be extended to parse from event metadata
    }
}

/// What the rebalancer wants to do.
#[derive(Debug, Clone)]
pub enum RebalanceAction {
    /// Sell shares of the winning token, buy shares of the losing token.
    Rebalance {
        sell_token_id: String,
        sell_shares: u64,
        buy_token_id: String,
        buy_shares: u64,
    },
    /// Exit the entire straddle (sell both legs).
    Exit {
        sell_up_shares: u64,
        sell_down_shares: u64,
    },
}

/// Rebalancing engine — stateless decision-maker.
#[derive(Debug, Clone)]
pub struct Rebalancer {
    delta_threshold: f64,
    min_interval_secs: u64,
    exit_before_expiry_secs: u64,
}

impl Rebalancer {
    pub fn new(config: &GammaScalpingConfig) -> Self {
        Self {
            delta_threshold: config.rebalance_delta_threshold,
            min_interval_secs: config.rebalance_interval_secs,
            exit_before_expiry_secs: config.exit_before_expiry_secs,
        }
    }

    /// Should we exit this straddle? (time-based exit before expiry)
    pub fn should_exit(&self, straddle: &Straddle, now: DateTime<Utc>) -> bool {
        let remaining = straddle.time_remaining_secs(now);
        remaining <= self.exit_before_expiry_secs as f64
    }

    /// Should we rebalance? Checks delta threshold and minimum interval.
    pub fn should_rebalance(
        &self,
        straddle: &Straddle,
        greeks: &BinaryGreeks,
        now: DateTime<Utc>,
    ) -> bool {
        // Check minimum interval
        let secs_since_last = (now - straddle.last_rebalance).num_seconds().max(0) as u64;
        if secs_since_last < self.min_interval_secs {
            return false;
        }

        // Check delta threshold
        let net_delta = straddle_delta(
            greeks,
            straddle.up_shares as f64,
            straddle.down_shares as f64,
        );
        net_delta.abs() > self.delta_threshold
    }

    /// Compute the rebalance action to flatten delta.
    ///
    /// The idea: if net delta > 0, we're long the UP token relative to DOWN.
    /// Sell some UP shares and buy DOWN shares to bring delta back to ~0.
    /// The number of shares to trade = |net_delta| / (2 × delta_per_share).
    pub fn compute_rebalance(
        &self,
        straddle: &Straddle,
        greeks: &BinaryGreeks,
    ) -> Option<RebalanceAction> {
        if greeks.delta.abs() < 1e-12 {
            return None;
        }

        let net_delta = straddle_delta(
            greeks,
            straddle.up_shares as f64,
            straddle.down_shares as f64,
        );

        if net_delta.abs() <= self.delta_threshold {
            return None;
        }

        // Each share of UP has delta = +greeks.delta
        // Each share of DOWN has delta = -greeks.delta
        // Selling 1 UP and buying 1 DOWN changes delta by -2×greeks.delta
        // So shares_to_trade = |net_delta| / (2 × |greeks.delta|)
        let shares_to_trade = (net_delta.abs() / (2.0 * greeks.delta.abs()))
            .round()
            .max(1.0) as u64;

        if shares_to_trade == 0 {
            return None;
        }

        if net_delta > 0.0 {
            // Long delta → sell UP, buy DOWN
            let sell_shares = shares_to_trade.min(straddle.up_shares);
            Some(RebalanceAction::Rebalance {
                sell_token_id: straddle.up_token_id.clone(),
                sell_shares,
                buy_token_id: straddle.down_token_id.clone(),
                buy_shares: sell_shares,
            })
        } else {
            // Short delta → sell DOWN, buy UP
            let sell_shares = shares_to_trade.min(straddle.down_shares);
            Some(RebalanceAction::Rebalance {
                sell_token_id: straddle.down_token_id.clone(),
                sell_shares,
                buy_token_id: straddle.up_token_id.clone(),
                buy_shares: sell_shares,
            })
        }
    }

    /// Compute exit action — sell all shares in both legs.
    pub fn compute_exit(&self, straddle: &Straddle) -> RebalanceAction {
        RebalanceAction::Exit {
            sell_up_shares: straddle.up_shares,
            sell_down_shares: straddle.down_shares,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use rust_decimal_macros::dec;

    fn make_config() -> GammaScalpingConfig {
        GammaScalpingConfig {
            rebalance_delta_threshold: 0.15,
            rebalance_interval_secs: 15,
            exit_before_expiry_secs: 60,
            ..Default::default()
        }
    }

    fn make_straddle(now: DateTime<Utc>) -> Straddle {
        Straddle {
            event_id: "test-event".to_string(),
            symbol: "BTCUSDT".to_string(),
            up_token_id: "up-token".to_string(),
            down_token_id: "down-token".to_string(),
            up_shares: 100,
            down_shares: 100,
            up_entry_price: dec!(0.50),
            down_entry_price: dec!(0.50),
            entry_time: now,
            expiry_time: now + Duration::seconds(600),
            last_rebalance: now - Duration::seconds(30),
            rebalance_count: 0,
            realized_pnl: Decimal::ZERO,
            cost_basis: dec!(100),
        }
    }

    #[test]
    fn test_should_exit_before_expiry() {
        let now = Utc::now();
        let straddle = make_straddle(now);
        let rebalancer = Rebalancer::new(&make_config());

        // 600s remaining — should NOT exit
        assert!(!rebalancer.should_exit(&straddle, now));

        // 30s remaining — should exit
        let near_expiry = straddle.expiry_time - Duration::seconds(30);
        assert!(rebalancer.should_exit(&straddle, near_expiry));
    }

    #[test]
    fn test_rebalance_triggers_on_threshold() {
        let now = Utc::now();
        let straddle = make_straddle(now);
        let rebalancer = Rebalancer::new(&make_config());

        // Balanced straddle with ATM greeks → no rebalance needed
        let greeks = BinaryGreeks {
            delta: 0.5,
            gamma: 1.0,
            theta: -0.001,
            vega: 0.1,
            fair_value: 0.5,
            d2: 0.0,
        };
        // 100 up × 0.5 - 100 down × 0.5 = 0 → no rebalance
        assert!(!rebalancer.should_rebalance(&straddle, &greeks, now));
    }

    #[test]
    fn test_rebalance_respects_min_interval() {
        let now = Utc::now();
        let mut straddle = make_straddle(now);
        straddle.last_rebalance = now - Duration::seconds(5); // Only 5s ago
        straddle.up_shares = 150; // Imbalanced
        straddle.down_shares = 50;

        let rebalancer = Rebalancer::new(&make_config());
        let greeks = BinaryGreeks {
            delta: 0.5,
            gamma: 1.0,
            theta: -0.001,
            vega: 0.1,
            fair_value: 0.5,
            d2: 0.0,
        };

        // Should NOT rebalance — too soon
        assert!(!rebalancer.should_rebalance(&straddle, &greeks, now));
    }

    #[test]
    fn test_compute_exit() {
        let now = Utc::now();
        let straddle = make_straddle(now);
        let rebalancer = Rebalancer::new(&make_config());

        match rebalancer.compute_exit(&straddle) {
            RebalanceAction::Exit {
                sell_up_shares,
                sell_down_shares,
            } => {
                assert_eq!(sell_up_shares, 100);
                assert_eq!(sell_down_shares, 100);
            }
            _ => panic!("Expected Exit action"),
        }
    }
}
