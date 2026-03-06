use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::BacktestResults;

/// Configuration for a directional backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionalBacktestConfig {
    /// Symbols to backtest (e.g. ["BTCUSDT", "ETHUSDT"])
    pub symbols: Vec<String>,
    /// Starting equity in USD
    pub initial_capital: Decimal,
    /// Position size in shares per trade
    pub shares_per_trade: u64,
    /// Maximum concurrent positions per symbol
    pub max_concurrent_positions: usize,
    /// Minimum edge to enter (fair_value - pm_ask - fees), e.g. 0.05 = 5%
    pub entry_threshold: f64,
    /// Don't buy YES above this price (e.g. 0.85)
    pub max_entry_price: Decimal,
    /// Don't buy YES below this price (e.g. 0.15)
    pub min_entry_price: Decimal,
    /// Minimum absolute momentum to trigger signal (e.g. 0.003 = 0.3%)
    pub min_momentum: Decimal,
    /// Time stop: exit if <N secs remaining AND position is underwater (e.g. 30)
    pub time_stop_secs: u64,
    /// Maximum loss per position in USD
    pub hard_stop_usd: Decimal,
    /// Hold winners to settlement (default true — let them run)
    pub hold_to_settlement: bool,
    /// Cooldown between entries on same symbol (seconds)
    pub cooldown_secs: u64,
    /// Minimum time remaining to enter a position (seconds).
    pub min_time_remaining_secs: u64,
    /// Maximum time remaining to enter (seconds).
    /// Only enter when outcome is becoming clearer.
    pub max_time_remaining_secs: u64,
    /// Use price_to_beat in fair value calculation
    pub use_price_to_beat: bool,
}

impl Default for DirectionalBacktestConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            initial_capital: dec!(10000),
            shares_per_trade: 100,
            max_concurrent_positions: 3,
            entry_threshold: 0.05,
            max_entry_price: dec!(0.85),
            min_entry_price: dec!(0.15),
            min_momentum: dec!(0.003),
            time_stop_secs: 30,
            hard_stop_usd: dec!(5),
            hold_to_settlement: true,
            cooldown_secs: 60,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            use_price_to_beat: true,
        }
    }
}

impl DirectionalBacktestConfig {
    pub fn with_symbols(symbols: Vec<String>) -> Self {
        Self {
            symbols,
            ..Default::default()
        }
    }
}

/// A closed trade with directional-specific diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionalClosedTrade {
    pub symbol: String,
    pub direction: String,
    pub entry_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub shares: u64,
    pub pnl: Decimal,
    pub won: bool,
    pub holding_secs: i64,
    pub exit_reason: String,
    pub entry_p_hat: f64,
    pub entry_ev_net: f64,
    pub s0: Decimal,
    pub entry_sigma: f64,
}

/// Sigmoid-like mapping from momentum to fair value.
pub fn estimate_fair_value(momentum: Decimal) -> Decimal {
    let abs_momentum = momentum.abs();
    let momentum_factor = if abs_momentum < dec!(0.001) {
        abs_momentum * dec!(50)
    } else if abs_momentum < dec!(0.005) {
        dec!(0.05) + (abs_momentum - dec!(0.001)) * dec!(40)
    } else {
        dec!(0.21) + (abs_momentum - dec!(0.005)) * dec!(30)
    };

    (dec!(0.50) + momentum_factor).min(dec!(0.90))
}

/// Adjust fair value based on distance to price_to_beat and time remaining.
pub fn adjust_fair_value_for_price_to_beat(
    base_fv: Decimal,
    momentum: Decimal,
    current_price: Decimal,
    price_to_beat: Decimal,
    time_remaining_secs: i64,
) -> Decimal {
    if price_to_beat <= Decimal::ZERO {
        return base_fv;
    }

    let distance_pct = (current_price - price_to_beat) / price_to_beat;
    let time_factor =
        (Decimal::ONE - Decimal::from(time_remaining_secs.max(0)) / dec!(900)).max(Decimal::ZERO);

    let direction_matches = (momentum > Decimal::ZERO && distance_pct > Decimal::ZERO)
        || (momentum < Decimal::ZERO && distance_pct < Decimal::ZERO);

    if direction_matches {
        let boost = distance_pct.abs() * time_factor * dec!(0.5);
        (base_fv + boost).min(dec!(0.95))
    } else {
        let reduction = distance_pct.abs() * dec!(0.3);
        (base_fv - reduction).max(dec!(0.35))
    }
}

/// Calculate the strategy-level Sharpe ratio from directional trades.
pub fn calculate_sharpe(trades: &[DirectionalClosedTrade]) -> f64 {
    if trades.len() < 2 {
        return 0.0;
    }

    let pnls: Vec<f64> = trades
        .iter()
        .map(|trade| trade.pnl.to_f64().unwrap_or(0.0))
        .collect();

    let n = pnls.len() as f64;
    let mean = pnls.iter().sum::<f64>() / n;
    let variance = pnls.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let std_dev = variance.sqrt();

    if std_dev < 1e-10 {
        return 0.0;
    }

    let trades_per_year: f64 = 24.0 * 365.0;
    (mean / std_dev) * trades_per_year.sqrt()
}

/// Build the generic `BacktestResults` view from directional-specific closed trades.
pub fn build_results(
    trades: &[DirectionalClosedTrade],
    equity_curve: &[(DateTime<Utc>, Decimal)],
    max_drawdown: Decimal,
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
) -> BacktestResults {
    let total = trades.len() as u64;
    let winning = trades.iter().filter(|trade| trade.won).count() as u64;
    let losing = total - winning;
    let total_pnl: Decimal = trades.iter().map(|trade| trade.pnl).sum();

    let win_rate = if total > 0 {
        winning as f64 / total as f64
    } else {
        0.0
    };

    let avg_pnl = if total > 0 {
        total_pnl / Decimal::from(total)
    } else {
        Decimal::ZERO
    };

    let wins: Vec<Decimal> = trades
        .iter()
        .filter(|trade| trade.won)
        .map(|trade| trade.pnl)
        .collect();
    let losses: Vec<Decimal> = trades
        .iter()
        .filter(|trade| !trade.won)
        .map(|trade| trade.pnl)
        .collect();

    let avg_win = if wins.is_empty() {
        Decimal::ZERO
    } else {
        wins.iter().sum::<Decimal>() / Decimal::from(wins.len() as u64)
    };
    let avg_loss = if losses.is_empty() {
        Decimal::ZERO
    } else {
        losses.iter().sum::<Decimal>() / Decimal::from(losses.len() as u64)
    };

    let largest_win = wins.iter().max().copied().unwrap_or(Decimal::ZERO);
    let largest_loss = losses.iter().min().copied().unwrap_or(Decimal::ZERO);

    let total_wins: Decimal = wins.iter().sum();
    let total_losses_abs: Decimal = losses.iter().map(|loss| loss.abs()).sum();
    let profit_factor = if total_losses_abs > Decimal::ZERO {
        (total_wins / total_losses_abs).to_f64().unwrap_or(0.0)
    } else if total_wins > Decimal::ZERO {
        f64::INFINITY
    } else {
        0.0
    };

    let avg_holding = if total > 0 {
        trades
            .iter()
            .map(|trade| trade.holding_secs as f64)
            .sum::<f64>()
            / total as f64
    } else {
        0.0
    };

    let total_volume: Decimal = trades
        .iter()
        .map(|trade| Decimal::from(trade.shares) * trade.entry_price)
        .sum();

    BacktestResults {
        start_time: data_range_start.unwrap_or_else(Utc::now),
        end_time: data_range_end.unwrap_or_else(Utc::now),
        total_trades: total,
        winning_trades: winning,
        losing_trades: losing,
        win_rate,
        total_pnl,
        total_volume,
        avg_pnl_per_trade: avg_pnl,
        max_drawdown,
        sharpe_ratio: calculate_sharpe(trades),
        profit_factor,
        avg_win,
        avg_loss,
        largest_win,
        largest_loss,
        avg_holding_time_secs: avg_holding,
        trades_by_symbol: HashMap::new(),
        trades: Vec::new(),
        equity_curve: equity_curve.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn sample_trade(
        entry_time: i64,
        exit_time: i64,
        entry_price: Decimal,
        shares: u64,
        pnl: Decimal,
        won: bool,
    ) -> DirectionalClosedTrade {
        DirectionalClosedTrade {
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            entry_time: Utc.timestamp_opt(entry_time, 0).unwrap(),
            exit_time: Utc.timestamp_opt(exit_time, 0).unwrap(),
            entry_price,
            exit_price: if won { Decimal::ONE } else { Decimal::ZERO },
            shares,
            pnl,
            won,
            holding_secs: exit_time - entry_time,
            exit_reason: "settlement".into(),
            entry_p_hat: 0.62,
            entry_ev_net: 0.08,
            s0: dec!(100),
            entry_sigma: 0.004,
        }
    }

    #[test]
    fn with_symbols_preserves_defaults() {
        let config =
            DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into(), "ETHUSDT".into()]);

        assert_eq!(config.symbols, vec!["BTCUSDT", "ETHUSDT"]);
        assert_eq!(config.initial_capital, dec!(10000));
        assert_eq!(config.shares_per_trade, 100);
        assert!(config.use_price_to_beat);
    }

    #[test]
    fn price_to_beat_adjustment_boosts_when_direction_matches() {
        let adjusted =
            adjust_fair_value_for_price_to_beat(dec!(0.60), dec!(0.01), dec!(101), dec!(100), 60);

        assert!(adjusted > dec!(0.60));
    }

    #[test]
    fn build_results_preserves_directional_trade_aggregates() {
        let trades = vec![
            sample_trade(
                1_700_000_000,
                1_700_000_060,
                dec!(0.40),
                10,
                dec!(6.0),
                true,
            ),
            sample_trade(
                1_700_000_120,
                1_700_000_180,
                dec!(0.30),
                20,
                dec!(-2.0),
                false,
            ),
        ];
        let equity_curve = vec![
            (Utc.timestamp_opt(1_700_000_000, 0).unwrap(), dec!(100)),
            (Utc.timestamp_opt(1_700_000_180, 0).unwrap(), dec!(104)),
        ];

        let results = build_results(
            &trades,
            &equity_curve,
            dec!(0.08),
            Some(Utc.timestamp_opt(1_700_000_000, 0).unwrap()),
            Some(Utc.timestamp_opt(1_700_000_180, 0).unwrap()),
        );

        assert_eq!(results.total_trades, 2);
        assert_eq!(results.winning_trades, 1);
        assert_eq!(results.losing_trades, 1);
        assert!((results.win_rate - 0.5).abs() < f64::EPSILON);
        assert_eq!(results.total_pnl, dec!(4.0));
        assert_eq!(results.total_volume, dec!(10.0));
        assert_eq!(results.avg_pnl_per_trade, dec!(2.0));
        assert_eq!(results.max_drawdown, dec!(0.08));
        assert_eq!(results.avg_win, dec!(6.0));
        assert_eq!(results.avg_loss, dec!(-2.0));
        assert_eq!(results.largest_win, dec!(6.0));
        assert_eq!(results.largest_loss, dec!(-2.0));
        assert_eq!(results.avg_holding_time_secs, 60.0);
        assert_eq!(results.equity_curve, equity_curve);
        assert!((results.sharpe_ratio - calculate_sharpe(&trades)).abs() < f64::EPSILON);
    }
}
