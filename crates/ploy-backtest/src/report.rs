//! Backtest Report Module
//!
//! Reads from DB tables (`backtest_runs`, `backtest_trades`, `backtest_signals`)
//! and generates comprehensive analysis reports including calibration, profitability
//! breakdowns, missed opportunity analysis, fee impact, and optimization suggestions.
//!
//! This module adds analysis ON TOP of the raw backtest data — it does not duplicate
//! `BacktestResults` from `backtest.rs`.

use std::fmt;

use anyhow::Context;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
#[cfg(feature = "persistence")]
use sqlx::PgPool;
use uuid::Uuid;

#[cfg(feature = "persistence")]
use crate::fee_model::FeeModel;
#[cfg(feature = "persistence")]
use chrono::Timelike;
#[cfg(feature = "persistence")]
use rust_decimal::prelude::*;
#[cfg(feature = "persistence")]
use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────
// DB row types (sqlx::FromRow)
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "persistence", derive(sqlx::FromRow))]
pub struct RunRow {
    pub run_id: Uuid,
    pub strategy: String,
    pub mode: String,
    pub config_json: serde_json::Value,
    pub symbols: Vec<String>,
    pub data_start: Option<DateTime<Utc>>,
    pub data_end: Option<DateTime<Utc>>,
    pub total_trades: Option<i32>,
    pub win_rate: Option<f64>,
    pub total_pnl: Option<Decimal>,
    pub sharpe_ratio: Option<f64>,
    pub max_drawdown: Option<Decimal>,
    pub profit_factor: Option<f64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "persistence", derive(sqlx::FromRow))]
pub struct TradeRow {
    pub id: i64,
    pub run_id: Uuid,
    pub symbol: String,
    pub direction: String,
    pub entry_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub shares: i32,
    pub pnl: Decimal,
    pub won: bool,
    pub holding_secs: i64,
    pub exit_reason: String,
    pub entry_p_hat: Option<f64>,
    pub entry_ev_net: Option<f64>,
    pub entry_sigma: Option<f64>,
    pub s0: Option<Decimal>,
    pub gamma_settled_price: Option<Decimal>,
    pub gamma_resolved: Option<bool>,
    pub gamma_match: Option<bool>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "persistence", derive(sqlx::FromRow))]
pub struct SignalRow {
    pub id: i64,
    pub run_id: Uuid,
    pub signal_type: String,
    pub symbol: String,
    pub direction: String,
    pub timestamp: DateTime<Utc>,
    pub p_hat: Option<f64>,
    pub ev_net: Option<f64>,
    pub sigma: Option<f64>,
    pub market_price: Option<Decimal>,
    pub spot_price: Option<Decimal>,
    pub s0: Option<Decimal>,
    pub time_remaining_secs: Option<f64>,
    pub filter_reason: Option<String>,
    pub exit_reason: Option<String>,
    pub exit_price: Option<Decimal>,
    pub created_at: DateTime<Utc>,
}

// ─────────────────────────────────────────────────────────────
// Report types
// ─────────────────────────────────────────────────────────────

/// Complete backtest analysis report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestReport {
    pub run: RunSummary,
    pub calibration: CalibrationAnalysis,
    pub profitability: ProfitabilityBreakdown,
    pub missed_opportunities: Vec<MissedOpportunity>,
    pub fee_impact: FeeImpact,
    pub gamma_verification: Option<GammaVerification>,
    pub suggestions: Vec<Suggestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: Uuid,
    pub strategy: String,
    pub mode: String,
    pub symbols: Vec<String>,
    pub data_start: Option<DateTime<Utc>>,
    pub data_end: Option<DateTime<Utc>>,
    pub total_trades: i32,
    pub win_rate: f64,
    pub total_pnl: Decimal,
    pub sharpe_ratio: f64,
    pub max_drawdown: Decimal,
    pub profit_factor: f64,
}

// ─── Calibration ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationAnalysis {
    pub buckets: Vec<CalibrationBucket>,
    pub overall_bias: f64,
    pub bias_level: BiasLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationBucket {
    pub range_start: f64,
    pub range_end: f64,
    pub count: usize,
    pub predicted_avg: f64,
    pub actual_win_rate: f64,
    pub bias: f64,
    pub bias_level: BiasLevel,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BiasLevel {
    Ok,
    Warning,
    Error,
}

// ─── Profitability ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfitabilityBreakdown {
    pub by_symbol: Vec<SymbolBreakdown>,
    pub by_direction: Vec<DirectionBreakdown>,
    pub by_time_of_day: Vec<SessionBreakdown>,
    pub by_ev_bucket: Vec<EvBucketBreakdown>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolBreakdown {
    pub symbol: String,
    pub trades: usize,
    pub wins: usize,
    pub win_rate: f64,
    pub pnl: Decimal,
    pub profit_factor: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionBreakdown {
    pub direction: String,
    pub trades: usize,
    pub wins: usize,
    pub win_rate: f64,
    pub pnl: Decimal,
    pub profit_factor: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBreakdown {
    pub session: String,
    pub hour_start: u32,
    pub hour_end: u32,
    pub trades: usize,
    pub wins: usize,
    pub win_rate: f64,
    pub pnl: Decimal,
    pub profit_factor: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvBucketBreakdown {
    pub label: String,
    pub ev_min: f64,
    pub ev_max: f64,
    pub trades: usize,
    pub wins: usize,
    pub win_rate: f64,
    pub pnl: Decimal,
    pub profit_factor: f64,
}

// ─── Missed opportunities ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissedOpportunity {
    pub filter_reason: String,
    pub count: usize,
    pub avg_ev: f64,
    pub max_ev: f64,
}

// ─── Fee impact ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeImpact {
    pub gross_pnl: Decimal,
    pub total_entry_fees: Decimal,
    pub total_exit_fees: Decimal,
    pub total_fees: Decimal,
    pub net_pnl: Decimal,
    pub fee_drag_pct: f64,
}

// ─── Gamma verification ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GammaVerification {
    pub total_trades: usize,
    pub matched: usize,
    pub mismatched: usize,
    pub unverified: usize,
    pub match_rate: f64,
}

// ─── Suggestions ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub priority: SuggestionPriority,
    pub description: String,
    pub evidence: String,
    pub estimated_impact: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum SuggestionPriority {
    High,
    Med,
    Low,
}

impl fmt::Display for SuggestionPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuggestionPriority::High => write!(f, "HIGH"),
            SuggestionPriority::Med => write!(f, "MED"),
            SuggestionPriority::Low => write!(f, "LOW"),
        }
    }
}

impl fmt::Display for BiasLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BiasLevel::Ok => write!(f, "OK"),
            BiasLevel::Warning => write!(f, "WARN"),
            BiasLevel::Error => write!(f, "ERR"),
        }
    }
}

// ─────────────────────────────────────────────────────────────
// Load from DB
// ─────────────────────────────────────────────────────────────

/// Load a complete backtest report from the database.
#[cfg(feature = "persistence")]
pub async fn load_report(pool: &PgPool, run_id: Uuid) -> Result<BacktestReport> {
    let run_row: RunRow = sqlx::query_as(
        "SELECT run_id, strategy, mode, config_json, symbols, data_start, data_end,
                total_trades, win_rate, total_pnl, sharpe_ratio, max_drawdown,
                profit_factor, created_at
         FROM backtest_runs WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .context("backtest_runs: run not found")?;

    let trades: Vec<TradeRow> = sqlx::query_as(
        "SELECT id, run_id, symbol, direction, entry_time, exit_time, entry_price,
                exit_price, shares, pnl, won, holding_secs, exit_reason,
                entry_p_hat, entry_ev_net, entry_sigma, s0,
                gamma_settled_price, gamma_resolved, gamma_match, created_at
         FROM backtest_trades WHERE run_id = $1 ORDER BY entry_time",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .context("backtest_trades: query failed")?;

    let signals: Vec<SignalRow> = sqlx::query_as(
        "SELECT id, run_id, signal_type, symbol, direction, timestamp,
                p_hat, ev_net, sigma, market_price, spot_price, s0,
                time_remaining_secs, filter_reason, exit_reason, exit_price, created_at
         FROM backtest_signals WHERE run_id = $1 ORDER BY timestamp",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .context("backtest_signals: query failed")?;

    let run = build_run_summary(&run_row);
    let calibration = build_calibration(&trades);
    let profitability = build_profitability(&trades);
    let missed_opportunities = build_missed_opportunities(&signals);
    let fee_impact = build_fee_impact(&trades);
    let gamma_verification = build_gamma_verification(&trades);
    let suggestions =
        generate_suggestions(&trades, &signals, &profitability, &calibration, &fee_impact);

    Ok(BacktestReport {
        run,
        calibration,
        profitability,
        missed_opportunities,
        fee_impact,
        gamma_verification,
        suggestions,
    })
}

// ─────────────────────────────────────────────────────────────
// Builder functions (used by load_report, requires persistence)
// ─────────────────────────────────────────────────────────────

#[cfg(feature = "persistence")]
fn build_run_summary(row: &RunRow) -> RunSummary {
    RunSummary {
        run_id: row.run_id,
        strategy: row.strategy.clone(),
        mode: row.mode.clone(),
        symbols: row.symbols.clone(),
        data_start: row.data_start,
        data_end: row.data_end,
        total_trades: row.total_trades.unwrap_or(0),
        win_rate: row.win_rate.unwrap_or(0.0),
        total_pnl: row.total_pnl.unwrap_or(Decimal::ZERO),
        sharpe_ratio: row.sharpe_ratio.unwrap_or(0.0),
        max_drawdown: row.max_drawdown.unwrap_or(Decimal::ZERO),
        profit_factor: row.profit_factor.unwrap_or(0.0),
    }
}

/// Bucket entry_p_hat by 0.05 steps from 0.50 to 1.00, compare predicted vs actual.
#[cfg(feature = "persistence")]
fn build_calibration(trades: &[TradeRow]) -> CalibrationAnalysis {
    let bucket_edges: Vec<f64> = vec![0.50, 0.55, 0.60, 0.65, 0.70, 0.75, 0.80, 0.85, 0.90, 1.00];
    let mut buckets = Vec::new();

    for window in bucket_edges.windows(2) {
        let (lo, hi) = (window[0], window[1]);
        let in_bucket: Vec<&TradeRow> = trades
            .iter()
            .filter(|t| {
                let p = t.entry_p_hat.unwrap_or(0.0);
                p >= lo && p < hi
            })
            .collect();

        let count = in_bucket.len();
        if count == 0 {
            buckets.push(CalibrationBucket {
                range_start: lo,
                range_end: hi,
                count: 0,
                predicted_avg: (lo + hi) / 2.0,
                actual_win_rate: 0.0,
                bias: 0.0,
                bias_level: BiasLevel::Ok,
            });
            continue;
        }

        let predicted_avg =
            in_bucket.iter().filter_map(|t| t.entry_p_hat).sum::<f64>() / count as f64;
        let wins = in_bucket.iter().filter(|t| t.won).count();
        let actual_win_rate = wins as f64 / count as f64;
        let bias = predicted_avg - actual_win_rate;
        let bias_level = classify_bias(bias);

        buckets.push(CalibrationBucket {
            range_start: lo,
            range_end: hi,
            count,
            predicted_avg,
            actual_win_rate,
            bias,
            bias_level,
        });
    }

    let total_predicted: f64 = trades.iter().filter_map(|t| t.entry_p_hat).sum();
    let total_wins = trades.iter().filter(|t| t.won).count();
    let n = trades.len().max(1) as f64;
    let overall_bias = total_predicted / n - total_wins as f64 / n;
    let bias_level = classify_bias(overall_bias);

    CalibrationAnalysis {
        buckets,
        overall_bias,
        bias_level,
    }
}

#[cfg(feature = "persistence")]
fn classify_bias(bias: f64) -> BiasLevel {
    let abs = bias.abs();
    if abs > 0.10 {
        BiasLevel::Error
    } else if abs > 0.05 {
        BiasLevel::Warning
    } else {
        BiasLevel::Ok
    }
}

#[cfg(feature = "persistence")]
fn build_profitability(trades: &[TradeRow]) -> ProfitabilityBreakdown {
    ProfitabilityBreakdown {
        by_symbol: build_by_symbol(trades),
        by_direction: build_by_direction(trades),
        by_time_of_day: build_by_time_of_day(trades),
        by_ev_bucket: build_by_ev_bucket(trades),
    }
}

#[cfg(feature = "persistence")]
fn build_by_symbol(trades: &[TradeRow]) -> Vec<SymbolBreakdown> {
    let mut groups: HashMap<&str, Vec<&TradeRow>> = HashMap::new();
    for t in trades {
        groups.entry(&t.symbol).or_default().push(t);
    }
    let mut result: Vec<SymbolBreakdown> = groups
        .into_iter()
        .map(|(symbol, group)| {
            let wins = group.iter().filter(|t| t.won).count();
            let n = group.len();
            SymbolBreakdown {
                symbol: symbol.to_string(),
                trades: n,
                wins,
                win_rate: if n > 0 { wins as f64 / n as f64 } else { 0.0 },
                pnl: group.iter().map(|t| t.pnl).sum(),
                profit_factor: compute_profit_factor(&group),
            }
        })
        .collect();
    result.sort_by(|a, b| b.pnl.cmp(&a.pnl));
    result
}

#[cfg(feature = "persistence")]
fn build_by_direction(trades: &[TradeRow]) -> Vec<DirectionBreakdown> {
    let mut groups: HashMap<&str, Vec<&TradeRow>> = HashMap::new();
    for t in trades {
        groups.entry(&t.direction).or_default().push(t);
    }
    groups
        .into_iter()
        .map(|(dir, group)| {
            let wins = group.iter().filter(|t| t.won).count();
            let n = group.len();
            DirectionBreakdown {
                direction: dir.to_string(),
                trades: n,
                wins,
                win_rate: if n > 0 { wins as f64 / n as f64 } else { 0.0 },
                pnl: group.iter().map(|t| t.pnl).sum(),
                profit_factor: compute_profit_factor(&group),
            }
        })
        .collect()
}

#[cfg(feature = "persistence")]
fn build_by_time_of_day(trades: &[TradeRow]) -> Vec<SessionBreakdown> {
    let sessions = [
        ("Asia (00-08 UTC)", 0u32, 8u32),
        ("EU (08-16 UTC)", 8, 16),
        ("US (16-24 UTC)", 16, 24),
    ];

    sessions
        .iter()
        .map(|(name, start, end)| {
            let group: Vec<&TradeRow> = trades
                .iter()
                .filter(|t| {
                    let h = t.entry_time.hour();
                    h >= *start && h < *end
                })
                .collect();
            let wins = group.iter().filter(|t| t.won).count();
            let n = group.len();
            SessionBreakdown {
                session: name.to_string(),
                hour_start: *start,
                hour_end: *end,
                trades: n,
                wins,
                win_rate: if n > 0 { wins as f64 / n as f64 } else { 0.0 },
                pnl: group.iter().map(|t| t.pnl).sum(),
                profit_factor: compute_profit_factor(&group),
            }
        })
        .collect()
}

#[cfg(feature = "persistence")]
fn build_by_ev_bucket(trades: &[TradeRow]) -> Vec<EvBucketBreakdown> {
    let buckets: Vec<(&str, f64, f64)> = vec![
        ("[0.10-0.15)", 0.10, 0.15),
        ("[0.15-0.20)", 0.15, 0.20),
        ("[0.20-0.30)", 0.20, 0.30),
        ("[0.30+)", 0.30, f64::MAX),
    ];

    buckets
        .into_iter()
        .map(|(label, lo, hi)| {
            let group: Vec<&TradeRow> = trades
                .iter()
                .filter(|t| {
                    let ev = t.entry_ev_net.unwrap_or(0.0);
                    ev >= lo && ev < hi
                })
                .collect();
            let wins = group.iter().filter(|t| t.won).count();
            let n = group.len();
            EvBucketBreakdown {
                label: label.to_string(),
                ev_min: lo,
                ev_max: if hi == f64::MAX { 999.0 } else { hi },
                trades: n,
                wins,
                win_rate: if n > 0 { wins as f64 / n as f64 } else { 0.0 },
                pnl: group.iter().map(|t| t.pnl).sum(),
                profit_factor: compute_profit_factor(&group),
            }
        })
        .collect()
}

#[cfg(feature = "persistence")]
fn compute_profit_factor(trades: &[&TradeRow]) -> f64 {
    let gross_wins: Decimal = trades.iter().filter(|t| t.won).map(|t| t.pnl).sum();
    let gross_losses: Decimal = trades.iter().filter(|t| !t.won).map(|t| t.pnl.abs()).sum();
    if gross_losses > Decimal::ZERO {
        gross_wins
            .checked_div(gross_losses)
            .and_then(|d| d.to_f64())
            .unwrap_or(0.0)
    } else if gross_wins > Decimal::ZERO {
        f64::INFINITY
    } else {
        0.0
    }
}

#[cfg(feature = "persistence")]
fn build_missed_opportunities(signals: &[SignalRow]) -> Vec<MissedOpportunity> {
    let filtered: Vec<&SignalRow> = signals
        .iter()
        .filter(|s| s.signal_type == "filtered")
        .collect();

    let mut groups: HashMap<String, Vec<&SignalRow>> = HashMap::new();
    for s in &filtered {
        let reason = s
            .filter_reason
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        groups.entry(reason).or_default().push(s);
    }

    let mut result: Vec<MissedOpportunity> = groups
        .into_iter()
        .map(|(reason, group)| {
            let evs: Vec<f64> = group.iter().filter_map(|s| s.ev_net).collect();
            let count = group.len();
            let avg_ev = if evs.is_empty() {
                0.0
            } else {
                evs.iter().sum::<f64>() / evs.len() as f64
            };
            let max_ev = evs.iter().cloned().fold(0.0_f64, f64::max);
            MissedOpportunity {
                filter_reason: reason,
                count,
                avg_ev,
                max_ev,
            }
        })
        .collect();
    result.sort_by(|a, b| b.count.cmp(&a.count));
    result
}

/// Compute fee impact using the parabolic FeeModel::crypto() curve.
#[cfg(feature = "persistence")]
fn build_fee_impact(trades: &[TradeRow]) -> FeeImpact {
    let fee_model = FeeModel::crypto();
    let mut total_entry_fees = Decimal::ZERO;
    let mut total_exit_fees = Decimal::ZERO;
    let mut gross_pnl = Decimal::ZERO;

    for t in trades {
        let shares = Decimal::from(t.shares);
        let entry_fee = fee_model.fee_shares(shares, t.entry_price);
        let exit_fee = fee_model.fee_shares(shares, t.exit_price);
        total_entry_fees += entry_fee;
        total_exit_fees += exit_fee;
        // Gross PnL = net PnL (from DB) + fees we're computing
        gross_pnl += t.pnl + entry_fee + exit_fee;
    }

    let total_fees = total_entry_fees + total_exit_fees;
    let net_pnl = gross_pnl - total_fees;
    let fee_drag_pct = if gross_pnl != Decimal::ZERO {
        (total_fees / gross_pnl.abs()).to_f64().unwrap_or(0.0) * 100.0
    } else {
        0.0
    };

    FeeImpact {
        gross_pnl,
        total_entry_fees,
        total_exit_fees,
        total_fees,
        net_pnl,
        fee_drag_pct,
    }
}

#[cfg(feature = "persistence")]
fn build_gamma_verification(trades: &[TradeRow]) -> Option<GammaVerification> {
    let has_any_gamma = trades.iter().any(|t| t.gamma_match.is_some());
    if !has_any_gamma {
        return None;
    }

    let total = trades.len();
    let matched = trades
        .iter()
        .filter(|t| t.gamma_match == Some(true))
        .count();
    let mismatched = trades
        .iter()
        .filter(|t| t.gamma_match == Some(false))
        .count();
    let unverified = total - matched - mismatched;
    let verified = matched + mismatched;
    let match_rate = if verified > 0 {
        matched as f64 / verified as f64
    } else {
        0.0
    };

    Some(GammaVerification {
        total_trades: total,
        matched,
        mismatched,
        unverified,
        match_rate,
    })
}

// ─────────────────────────────────────────────────────────────
// Suggestion engine
// ─────────────────────────────────────────────────────────────

#[cfg(feature = "persistence")]
fn generate_suggestions(
    trades: &[TradeRow],
    signals: &[SignalRow],
    profitability: &ProfitabilityBreakdown,
    calibration: &CalibrationAnalysis,
    fee_impact: &FeeImpact,
) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // 1. Low-EV bucket PF < 1.2 → raise entry_threshold
    if let Some(lowest) = profitability.by_ev_bucket.first() {
        if lowest.trades >= 5 && lowest.profit_factor < 1.2 {
            suggestions.push(Suggestion {
                priority: SuggestionPriority::High,
                description: format!(
                    "Raise entry_threshold: lowest EV bucket {} has PF {:.2}",
                    lowest.label, lowest.profit_factor
                ),
                evidence: format!(
                    "{} trades, PnL ${:.2}, win rate {:.1}%",
                    lowest.trades,
                    lowest.pnl,
                    lowest.win_rate * 100.0
                ),
                estimated_impact: "Eliminates low-edge trades dragging overall PF".to_string(),
            });
        }
    }

    // 2. Filtered signals with high avg EV → relax that filter
    let filtered: Vec<&SignalRow> = signals
        .iter()
        .filter(|s| s.signal_type == "filtered")
        .collect();
    // Compute current avg EV from entry trades as threshold reference
    let entry_evs: Vec<f64> = trades.iter().filter_map(|t| t.entry_ev_net).collect();
    let current_avg_ev = if entry_evs.is_empty() {
        0.15
    } else {
        entry_evs.iter().sum::<f64>() / entry_evs.len() as f64
    };

    let mut filter_groups: HashMap<String, Vec<f64>> = HashMap::new();
    for s in &filtered {
        if let (Some(reason), Some(ev)) = (&s.filter_reason, s.ev_net) {
            filter_groups.entry(reason.clone()).or_default().push(ev);
        }
    }
    for (reason, evs) in &filter_groups {
        let avg = evs.iter().sum::<f64>() / evs.len() as f64;
        if avg > current_avg_ev * 1.5 && evs.len() > 20 {
            suggestions.push(Suggestion {
                priority: SuggestionPriority::Med,
                description: format!(
                    "Relax '{}' filter: {} filtered signals with avg EV {:.3} (1.5x threshold)",
                    reason,
                    evs.len(),
                    avg
                ),
                evidence: format!(
                    "Avg EV {:.3} vs current avg {:.3}, count {}",
                    avg,
                    current_avg_ev,
                    evs.len()
                ),
                estimated_impact: format!(
                    "Could capture ~{} additional trades with strong edge",
                    evs.len()
                ),
            });
        }
    }

    // 3. Per-symbol PF < 1.15 → drop symbol
    for sym in &profitability.by_symbol {
        if sym.trades >= 10 && sym.profit_factor < 1.15 && sym.profit_factor > 0.0 {
            suggestions.push(Suggestion {
                priority: SuggestionPriority::Med,
                description: format!(
                    "Consider dropping {}: PF {:.2} across {} trades",
                    sym.symbol, sym.profit_factor, sym.trades
                ),
                evidence: format!("PnL ${:.2}, win rate {:.1}%", sym.pnl, sym.win_rate * 100.0),
                estimated_impact: "Removes drag from underperforming symbol".to_string(),
            });
        }
    }

    // 4. Calibration bias > 8% in p_hat > 0.70 bucket → raise vol_floor
    for bucket in &calibration.buckets {
        if bucket.range_start >= 0.70 && bucket.count >= 5 && bucket.bias > 0.08 {
            suggestions.push(Suggestion {
                priority: SuggestionPriority::High,
                description: format!(
                    "Raise vol_floor: p_hat [{:.2}-{:.2}) bucket overconfident by {:.1}%",
                    bucket.range_start,
                    bucket.range_end,
                    bucket.bias * 100.0
                ),
                evidence: format!(
                    "Predicted {:.1}% vs actual {:.1}% ({} trades)",
                    bucket.predicted_avg * 100.0,
                    bucket.actual_win_rate * 100.0,
                    bucket.count
                ),
                estimated_impact: "Reduces overconfidence in high-probability estimates"
                    .to_string(),
            });
        }
    }

    // 5. Fee drag > 25% → shift to higher-EV trades
    if fee_impact.fee_drag_pct > 25.0 {
        suggestions.push(Suggestion {
            priority: SuggestionPriority::High,
            description: format!(
                "Fee drag {:.1}% is excessive — shift to higher-EV trades",
                fee_impact.fee_drag_pct
            ),
            evidence: format!(
                "Gross PnL ${:.2}, fees ${:.2}, net ${:.2}",
                fee_impact.gross_pnl, fee_impact.total_fees, fee_impact.net_pnl
            ),
            estimated_impact: "Limit 0.45-0.55 price range or raise EV threshold".to_string(),
        });
    }

    suggestions.sort_by(|a, b| a.priority.cmp(&b.priority));
    suggestions
}

// ─────────────────────────────────────────────────────────────
// Report output
// ─────────────────────────────────────────────────────────────

impl BacktestReport {
    /// Formatted text report for terminal output.
    pub fn print_report(&self) -> String {
        let mut out = String::new();
        let w = 64;
        let bar = "=".repeat(w);
        let thin = "-".repeat(w);

        // Header
        out.push_str(&format!("{}\n", bar));
        out.push_str(&format!(
            "  BACKTEST REPORT: {} ({})\n",
            self.run.strategy, self.run.mode
        ));
        out.push_str(&format!(
            "  Run: {}  Symbols: {}\n",
            self.run.run_id,
            self.run.symbols.join(", ")
        ));
        if let (Some(start), Some(end)) = (self.run.data_start, self.run.data_end) {
            out.push_str(&format!(
                "  Period: {} to {}\n",
                start.format("%Y-%m-%d %H:%M"),
                end.format("%Y-%m-%d %H:%M")
            ));
        }
        out.push_str(&format!("{}\n\n", bar));

        // Performance summary
        out.push_str(&format!("  PERFORMANCE SUMMARY\n  {}\n", thin));
        out.push_str(&format!(
            "  Trades: {}  Win Rate: {:.1}%  PnL: ${:.2}\n",
            self.run.total_trades,
            self.run.win_rate * 100.0,
            self.run.total_pnl
        ));
        out.push_str(&format!(
            "  Sharpe: {:.2}  Max DD: {:.2}%  PF: {:.2}\n\n",
            self.run.sharpe_ratio,
            self.run.max_drawdown * dec!(100),
            self.run.profit_factor
        ));

        // Calibration
        out.push_str(&format!("  CALIBRATION ANALYSIS\n  {}\n", thin));
        out.push_str("  p_hat Range   | Count | Predicted | Actual  | Bias    | Status\n");
        for b in &self.calibration.buckets {
            if b.count == 0 {
                continue;
            }
            out.push_str(&format!(
                "  [{:.2}-{:.2}) | {:>5} | {:>8.1}% | {:>6.1}% | {:>+5.1}% | {}\n",
                b.range_start,
                b.range_end,
                b.count,
                b.predicted_avg * 100.0,
                b.actual_win_rate * 100.0,
                b.bias * 100.0,
                b.bias_level
            ));
        }
        out.push_str(&format!(
            "  Overall bias: {:+.1}% ({})\n\n",
            self.calibration.overall_bias * 100.0,
            self.calibration.bias_level
        ));

        // Profitability by symbol
        out.push_str(&format!("  PROFITABILITY BY SYMBOL\n  {}\n", thin));
        out.push_str("  Symbol   | Trades | Win%  | PnL       | PF\n");
        for s in &self.profitability.by_symbol {
            out.push_str(&format!(
                "  {:8} | {:>6} | {:>4.1}% | ${:>8.2} | {:.2}\n",
                s.symbol,
                s.trades,
                s.win_rate * 100.0,
                s.pnl,
                s.profit_factor
            ));
        }
        out.push('\n');

        // Profitability by direction
        out.push_str(&format!("  PROFITABILITY BY DIRECTION\n  {}\n", thin));
        for d in &self.profitability.by_direction {
            out.push_str(&format!(
                "  {:6} | {} trades | {:.1}% win | ${:.2} | PF {:.2}\n",
                d.direction,
                d.trades,
                d.win_rate * 100.0,
                d.pnl,
                d.profit_factor
            ));
        }
        out.push('\n');

        // Profitability by session
        out.push_str(&format!("  PROFITABILITY BY SESSION\n  {}\n", thin));
        for s in &self.profitability.by_time_of_day {
            out.push_str(&format!(
                "  {:20} | {} trades | {:.1}% win | ${:.2} | PF {:.2}\n",
                s.session,
                s.trades,
                s.win_rate * 100.0,
                s.pnl,
                s.profit_factor
            ));
        }
        out.push('\n');

        // Profitability by EV bucket
        out.push_str(&format!("  PROFITABILITY BY EV BUCKET\n  {}\n", thin));
        for e in &self.profitability.by_ev_bucket {
            out.push_str(&format!(
                "  {:12} | {} trades | {:.1}% win | ${:.2} | PF {:.2}\n",
                e.label,
                e.trades,
                e.win_rate * 100.0,
                e.pnl,
                e.profit_factor
            ));
        }
        out.push('\n');

        // Fee impact
        out.push_str(&format!("  FEE IMPACT\n  {}\n", thin));
        out.push_str(&format!(
            "  Gross PnL:    ${:.2}\n",
            self.fee_impact.gross_pnl
        ));
        out.push_str(&format!(
            "  Entry fees:   ${:.2}\n",
            self.fee_impact.total_entry_fees
        ));
        out.push_str(&format!(
            "  Exit fees:    ${:.2}\n",
            self.fee_impact.total_exit_fees
        ));
        out.push_str(&format!(
            "  Total fees:   ${:.2}\n",
            self.fee_impact.total_fees
        ));
        out.push_str(&format!(
            "  Net PnL:      ${:.2}\n",
            self.fee_impact.net_pnl
        ));
        out.push_str(&format!(
            "  Fee drag:     {:.1}%\n\n",
            self.fee_impact.fee_drag_pct
        ));

        // Missed opportunities
        if !self.missed_opportunities.is_empty() {
            out.push_str(&format!("  MISSED OPPORTUNITIES\n  {}\n", thin));
            out.push_str("  Filter Reason          | Count | Avg EV | Max EV\n");
            for m in &self.missed_opportunities {
                out.push_str(&format!(
                    "  {:24} | {:>5} | {:.3}  | {:.3}\n",
                    m.filter_reason, m.count, m.avg_ev, m.max_ev
                ));
            }
            out.push('\n');
        }

        // Gamma verification
        if let Some(ref gv) = self.gamma_verification {
            out.push_str(&format!("  GAMMA VERIFICATION\n  {}\n", thin));
            out.push_str(&format!(
                "  Matched: {}  Mismatched: {}  Unverified: {}  Rate: {:.1}%\n\n",
                gv.matched,
                gv.mismatched,
                gv.unverified,
                gv.match_rate * 100.0
            ));
        }

        // Suggestions
        if !self.suggestions.is_empty() {
            out.push_str(&format!("  OPTIMIZATION SUGGESTIONS\n  {}\n", thin));
            for (i, s) in self.suggestions.iter().enumerate() {
                out.push_str(&format!(
                    "  {}. [{}] {}\n     Evidence: {}\n     Impact: {}\n\n",
                    i + 1,
                    s.priority,
                    s.description,
                    s.evidence,
                    s.estimated_impact
                ));
            }
        }

        out.push_str(&format!("{}\n", bar));

        out
    }

    /// Serialize the full report to JSON.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("failed to serialize report")
    }
}
