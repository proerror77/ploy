use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::BacktestEngine;

/// Individual backtest trade result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestTrade {
    pub entry_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub symbol: String,
    pub market_id: String,
    pub direction: String,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub shares: u64,
    pub pnl: Decimal,
    pub pnl_pct: Decimal,
    pub won: bool,
    pub fair_value: Decimal,
    pub price_edge: Decimal,
    pub vol_edge_pct: f64,
    pub confidence: f64,
    pub buffer_pct: Decimal,
    pub our_volatility: f64,
    pub implied_volatility: f64,
}

/// Backtest summary statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResults {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub total_trades: u64,
    pub winning_trades: u64,
    pub losing_trades: u64,
    pub win_rate: f64,
    pub total_pnl: Decimal,
    pub total_volume: Decimal,
    pub avg_pnl_per_trade: Decimal,
    pub max_drawdown: Decimal,
    pub sharpe_ratio: f64,
    pub profit_factor: f64,
    pub avg_win: Decimal,
    pub avg_loss: Decimal,
    pub largest_win: Decimal,
    pub largest_loss: Decimal,
    pub avg_holding_time_secs: f64,
    pub trades_by_symbol: HashMap<String, SymbolStats>,
    pub trades: Vec<BacktestTrade>,
    pub equity_curve: Vec<(DateTime<Utc>, Decimal)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolStats {
    pub total_trades: u64,
    pub winning_trades: u64,
    pub win_rate: f64,
    pub total_pnl: Decimal,
}

impl Default for BacktestResults {
    fn default() -> Self {
        Self {
            start_time: Utc::now(),
            end_time: Utc::now(),
            total_trades: 0,
            winning_trades: 0,
            losing_trades: 0,
            win_rate: 0.0,
            total_pnl: Decimal::ZERO,
            total_volume: Decimal::ZERO,
            avg_pnl_per_trade: Decimal::ZERO,
            max_drawdown: Decimal::ZERO,
            sharpe_ratio: 0.0,
            profit_factor: 0.0,
            avg_win: Decimal::ZERO,
            avg_loss: Decimal::ZERO,
            largest_win: Decimal::ZERO,
            largest_loss: Decimal::ZERO,
            avg_holding_time_secs: 0.0,
            trades_by_symbol: HashMap::new(),
            trades: Vec::new(),
            equity_curve: Vec::new(),
        }
    }
}

impl BacktestEngine {
    pub(super) fn calculate_statistics(&mut self) {
        let trades = &self.results.trades;

        if trades.is_empty() {
            return;
        }

        self.results.win_rate = self.results.winning_trades as f64 / self.results.total_trades as f64;
        self.results.avg_pnl_per_trade =
            self.results.total_pnl / Decimal::from(self.results.total_trades);

        let wins: Vec<_> = trades.iter().filter(|t| t.won).collect();
        let losses: Vec<_> = trades.iter().filter(|t| !t.won).collect();

        if !wins.is_empty() {
            self.results.avg_win =
                wins.iter().map(|t| t.pnl).sum::<Decimal>() / Decimal::from(wins.len() as u64);
            self.results.largest_win = wins.iter().map(|t| t.pnl).max().unwrap_or(Decimal::ZERO);
        }

        if !losses.is_empty() {
            self.results.avg_loss = losses.iter().map(|t| t.pnl).sum::<Decimal>()
                / Decimal::from(losses.len() as u64);
            self.results.largest_loss = losses.iter().map(|t| t.pnl).min().unwrap_or(Decimal::ZERO);
        }

        let mut peak = self.initial_capital;
        let mut max_dd = Decimal::ZERO;

        for (_, equity) in &self.results.equity_curve {
            if *equity > peak {
                peak = *equity;
            }
            let dd = (peak - equity) / peak;
            if dd > max_dd {
                max_dd = dd;
            }
        }
        self.results.max_drawdown = max_dd;

        let total_wins: Decimal = wins.iter().map(|t| t.pnl).sum();
        let total_losses: Decimal = losses.iter().map(|t| t.pnl.abs()).sum();
        if total_losses > Decimal::ZERO {
            self.results.profit_factor = (total_wins / total_losses).to_f64().unwrap_or(0.0);
        }

        let returns: Vec<f64> = trades.iter().filter_map(|t| t.pnl_pct.to_f64()).collect();
        if returns.len() > 1 {
            let mean = returns.iter().sum::<f64>() / returns.len() as f64;
            let variance =
                returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
            let std_dev = variance.sqrt();

            if std_dev > 0.0 {
                self.results.sharpe_ratio = mean / std_dev * (100.0_f64).sqrt();
            }
        }

        let total_hold_time: i64 = trades
            .iter()
            .map(|t| (t.exit_time - t.entry_time).num_seconds())
            .sum();
        self.results.avg_holding_time_secs = total_hold_time as f64 / trades.len() as f64;

        for stats in self.results.trades_by_symbol.values_mut() {
            if stats.total_trades > 0 {
                stats.win_rate = stats.winning_trades as f64 / stats.total_trades as f64;
            }
        }
    }
}
