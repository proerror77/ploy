use std::collections::HashMap;

use chrono::Timelike;
use rust_decimal::Decimal;
use rust_decimal::prelude::*;

use crate::strategy::fee_model::FeeModel;

use super::{
    BiasLevel, CalibrationAnalysis, CalibrationBucket, DirectionBreakdown, EvBucketBreakdown,
    FeeImpact, GammaVerification, MissedOpportunity, ProfitabilityBreakdown, RunRow, RunSummary,
    SessionBreakdown, SignalRow, Suggestion, SuggestionPriority, SymbolBreakdown, TradeRow,
};

pub(super) fn build_run_summary(row: &RunRow) -> RunSummary {
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
pub(super) fn build_calibration(trades: &[TradeRow]) -> CalibrationAnalysis {
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

pub(super) fn build_profitability(trades: &[TradeRow]) -> ProfitabilityBreakdown {
    ProfitabilityBreakdown {
        by_symbol: build_by_symbol(trades),
        by_direction: build_by_direction(trades),
        by_time_of_day: build_by_time_of_day(trades),
        by_ev_bucket: build_by_ev_bucket(trades),
    }
}

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

pub(super) fn build_missed_opportunities(signals: &[SignalRow]) -> Vec<MissedOpportunity> {
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
pub(super) fn build_fee_impact(trades: &[TradeRow]) -> FeeImpact {
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

pub(super) fn build_gamma_verification(trades: &[TradeRow]) -> Option<GammaVerification> {
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

pub(super) fn generate_suggestions(
    trades: &[TradeRow],
    signals: &[SignalRow],
    profitability: &ProfitabilityBreakdown,
    calibration: &CalibrationAnalysis,
    fee_impact: &FeeImpact,
) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

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

    let filtered: Vec<&SignalRow> = signals
        .iter()
        .filter(|s| s.signal_type == "filtered")
        .collect();
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
