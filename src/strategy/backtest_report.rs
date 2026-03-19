//! Backtest Report Module
//!
//! Reads from DB tables (`backtest_runs`, `backtest_trades`, `backtest_signals`)
//! and generates comprehensive analysis reports including calibration, profitability
//! breakdowns, missed opportunity analysis, fee impact, and optimization suggestions.
//!
//! This module adds analysis ON TOP of the raw backtest data — it does not duplicate
//! `BacktestResults` from `backtest.rs`.

#[path = "backtest_report/analysis.rs"]
mod analysis;

use std::fmt;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use analysis::{
    build_calibration, build_fee_impact, build_gamma_verification, build_missed_opportunities,
    build_profitability, build_run_summary, generate_suggestions,
};

// ─────────────────────────────────────────────────────────────
// DB row types (sqlx::FromRow)
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
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

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
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

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
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
