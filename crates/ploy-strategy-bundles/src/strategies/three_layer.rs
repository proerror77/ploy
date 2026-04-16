//! Three-layer strategy config and regime classification.

use rust_decimal::Decimal;
use crate::strategies::directional::DirectionalConfig;

/// Time-remaining regime for a binary option market.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// 181..=300 seconds remaining.
    Early,
    /// 61..=180 seconds remaining.
    Middle,
    /// 6..=60 seconds remaining.
    Late,
    /// 0..=5 seconds remaining.
    Expiry,
}

impl Regime {
    pub fn from_secs(secs: i64) -> Self {
        match secs {
            181..=300 => Regime::Early,
            61..=180  => Regime::Middle,
            6..=60    => Regime::Late,
            _         => Regime::Expiry,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Regime::Early  => "early",
            Regime::Middle => "middle",
            Regime::Late   => "late",
            Regime::Expiry => "expiry",
        }
    }
}

/// Configuration for the three-layer directional strategy.
#[derive(Debug, Clone)]
pub struct ThreeLayerConfig {
    pub symbols: Vec<String>,
    pub min_direction_prob: f64,
    pub min_distance_over_sigma: f64,
    pub min_confirmation_score: f64,
    pub min_drift_confirmation: f64,
    pub min_edge: f64,
    pub min_reward_risk: f64,
    pub take_profit_ask: f64,
    pub stop_distance_pct: f64,
    pub max_pm_lag_secs: u64,
    pub min_time_remaining_secs: u64,
    pub max_time_remaining_secs: u64,
    pub cooldown_secs: u64,
    pub stake_usd: Decimal,
    pub max_positions: usize,
    pub max_daily_trades: u32,
    pub allowed_window_secs: Vec<u64>,
    pub min_entry_price: f64,
    pub max_entry_price: f64,
}

impl From<DirectionalConfig> for ThreeLayerConfig {
    fn from(c: DirectionalConfig) -> Self {
        Self {
            symbols: c.symbols,
            min_direction_prob: c.three_layer_min_direction_prob,
            min_distance_over_sigma: c.three_layer_min_distance_over_sigma,
            min_confirmation_score: c.three_layer_min_confirmation_score,
            min_drift_confirmation: c.three_layer_min_drift_confirmation,
            min_edge: c.three_layer_min_edge,
            min_reward_risk: c.three_layer_min_reward_risk,
            take_profit_ask: c.three_layer_take_profit_ask,
            stop_distance_pct: c.three_layer_stop_distance_pct,
            max_pm_lag_secs: c.three_layer_max_pm_lag_secs,
            min_time_remaining_secs: c.min_time_remaining_secs,
            max_time_remaining_secs: c.max_time_remaining_secs,
            cooldown_secs: c.cooldown_secs,
            stake_usd: c.stake_usd,
            max_positions: c.max_positions,
            max_daily_trades: c.max_daily_trades,
            allowed_window_secs: c.allowed_window_secs,
            min_entry_price: c.min_entry_price,
            max_entry_price: c.max_entry_price,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regime_from_secs_boundaries() {
        assert_eq!(Regime::from_secs(300), Regime::Early);
        assert_eq!(Regime::from_secs(181), Regime::Early);
        assert_eq!(Regime::from_secs(180), Regime::Middle);
        assert_eq!(Regime::from_secs(61),  Regime::Middle);
        assert_eq!(Regime::from_secs(60),  Regime::Late);
        assert_eq!(Regime::from_secs(6),   Regime::Late);
        assert_eq!(Regime::from_secs(5),   Regime::Expiry);
        assert_eq!(Regime::from_secs(0),   Regime::Expiry);
    }

    #[test]
    fn config_from_directional_preserves_fields() {
        let dc: DirectionalConfig = serde_json::from_str("{}").unwrap();
        let tlc: ThreeLayerConfig = dc.into();
        assert_eq!(tlc.min_edge, 0.03);
        assert_eq!(tlc.min_reward_risk, 1.2);
        assert_eq!(tlc.take_profit_ask, 0.70);
        assert!(!tlc.symbols.is_empty());
    }
}
