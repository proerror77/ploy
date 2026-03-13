//! Reverse-engineered profile strategy (paper-only).
//!
//! This module does NOT copy target BUY/SELL orders directly.
//! Instead, it infers a parameterized strategy from a public profile snapshot
//! and runs an independent decision engine in dry-run mode.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;
use tracing::{info, warn};

use crate::error::Result;

mod dry_run_flow;
mod profile_payload;

use dry_run_flow::{apply_new_ticks, refresh_mark_to_market};
pub use profile_payload::extract_profile_snapshot;
use profile_payload::fetch_payload;

/// Canonical strategy slug for reverse-engineered profile dry-run mode.
pub const REVERSE_PROFILE_STRATEGY_SLUG: &str = "shadow_pulse";
/// Human-readable strategy name for reverse-engineered profile dry-run mode.
pub const REVERSE_PROFILE_STRATEGY_NAME: &str = "Shadow Pulse";

#[derive(Debug, Clone)]
pub struct ReverseEngineeredConfig {
    pub profile_url: String,
    pub poll_interval_secs: u64,
    pub min_trade_usdc: f64,
    pub max_event_usdc: f64,
    pub max_total_usdc: f64,
    pub target_assets: Vec<String>,
}

impl Default for ReverseEngineeredConfig {
    fn default() -> Self {
        Self {
            profile_url: "https://polymarket.com/zh/@k9Q2mX4L8A7ZP3R".to_string(),
            poll_interval_secs: 30,
            min_trade_usdc: 5.0,
            max_event_usdc: 250.0,
            max_total_usdc: 2000.0,
            target_assets: vec![
                "Bitcoin".to_string(),
                "Ethereum".to_string(),
                "Solana".to_string(),
                "XRP".to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone)]
pub struct StrategyParams {
    pub bias_down_target: f64,
    pub entry_window_secs: i64,
    pub down_buy_threshold: f64,
    pub up_buy_threshold: f64,
    pub take_profit: f64,
    pub min_trade_usdc: f64,
    pub scalp_fraction: f64,
    pub hedge_band_low: f64,
    pub hedge_band_high: f64,
    pub max_event_usdc: f64,
    pub max_total_usdc: f64,
}

#[derive(Debug, Clone)]
pub struct ReverseTradeEvent {
    pub event_slug: String,
    pub outcome: String,
    pub side: String,
    pub price: f64,
    pub size: f64,
    pub usdc_size: f64,
    pub timestamp: i64,
    pub title: String,
    pub raw_type: String,
    pub transaction_hash: String,
}

#[derive(Debug, Clone)]
pub struct ProfileSnapshot {
    pub address: String,
    pub activity: Vec<ReverseTradeEvent>,
    pub positions: Vec<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct ReverseDryRunResult {
    pub executed_buys: usize,
    pub executed_sells: usize,
    pub skipped_orders: usize,
    pub buy_notional: f64,
    pub sell_notional: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub open_mark_value: f64,
    pub down_mark_value: f64,
    pub up_mark_value: f64,
}

impl ReverseDryRunResult {
    pub fn total_pnl(&self) -> f64 {
        self.realized_pnl + self.unrealized_pnl
    }

    pub fn down_ratio(&self) -> f64 {
        let total = self.down_mark_value + self.up_mark_value;
        if total <= 0.0 {
            0.5
        } else {
            self.down_mark_value / total
        }
    }
}

#[derive(Debug, Default)]
struct ReverseState {
    seen_trade_ids: HashSet<String>,
    inventory: HashMap<(String, String), Vec<(f64, f64)>>,
    event_spend: HashMap<String, f64>,
    latest_px: HashMap<(String, String), f64>,
    result: ReverseDryRunResult,
}

fn clamp(x: f64, lo: f64, hi: f64) -> f64 {
    x.max(lo).min(hi)
}

fn to_f64(v: &Value) -> f64 {
    if let Some(x) = v.as_f64() {
        if x.is_finite() {
            return x;
        }
    }
    if let Some(s) = v.as_str() {
        if let Ok(x) = s.parse::<f64>() {
            if x.is_finite() {
                return x;
            }
        }
    }
    0.0
}

fn percentile(mut values: Vec<f64>, p: f64, default: f64) -> f64 {
    values.retain(|x| x.is_finite() && *x > 0.0);
    if values.is_empty() {
        return default;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let idx = ((values.len() - 1) as f64 * clamp(p, 0.0, 1.0)).round() as usize;
    values[idx]
}

fn parse_event_end_ts(event_slug: &str) -> Option<i64> {
    let ts = event_slug
        .rsplit('-')
        .next()
        .and_then(|x| x.parse::<i64>().ok())?;
    if ts < 100_000_000 {
        return None;
    }
    let dur = if event_slug.contains("-15m-") {
        900
    } else if event_slug.contains("-5m-") {
        300
    } else {
        3600
    };
    Some(ts + dur)
}

pub fn infer_strategy_params(
    snapshot: &ProfileSnapshot,
    cfg: &ReverseEngineeredConfig,
) -> StrategyParams {
    let trades: Vec<&ReverseTradeEvent> = snapshot
        .activity
        .iter()
        .filter(|x| x.raw_type == "TRADE" && x.price > 0.0 && x.size > 0.0)
        .collect();

    let mut down_value = 0.0;
    let mut up_value = 0.0;
    for row in &snapshot.positions {
        let Some(obj) = row.as_object() else {
            continue;
        };
        let v = obj.get("currentValue").map(to_f64).unwrap_or_default();
        match obj
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "down" => down_value += v,
            "up" => up_value += v,
            _ => {}
        }
    }
    let total = down_value + up_value;
    let inferred_bias = if total > 0.0 {
        down_value / total
    } else {
        0.65
    };
    let bias_down_target = clamp(inferred_bias, 0.55, 0.85);

    let mut tte_values = Vec::new();
    for t in &trades {
        if let Some(end_ts) = parse_event_end_ts(&t.event_slug) {
            let tte = end_ts - t.timestamp;
            if (0..=600).contains(&tte) {
                tte_values.push(tte as f64);
            }
        }
    }
    let entry_window_secs = percentile(tte_values, 0.80, 120.0).round() as i64;
    let entry_window_secs = clamp(entry_window_secs as f64, 30.0, 300.0) as i64;

    let down_buy_prices: Vec<f64> = trades
        .iter()
        .filter(|t| t.side == "BUY" && t.outcome.eq_ignore_ascii_case("Down"))
        .map(|t| t.price)
        .collect();
    let up_buy_prices: Vec<f64> = trades
        .iter()
        .filter(|t| t.side == "BUY" && t.outcome.eq_ignore_ascii_case("Up"))
        .map(|t| t.price)
        .collect();

    let down_buy_threshold = clamp(percentile(down_buy_prices, 0.70, 0.58), 0.10, 0.90);
    let up_buy_threshold = clamp(percentile(up_buy_prices, 0.60, 0.35), 0.05, 0.80);

    let mut leg_prices: HashMap<(String, String), (Vec<f64>, Vec<f64>)> = HashMap::new();
    for t in &trades {
        let entry = leg_prices
            .entry((t.event_slug.clone(), t.outcome.clone()))
            .or_insert_with(|| (Vec::new(), Vec::new()));
        if t.side == "BUY" {
            entry.0.push(t.price);
        } else if t.side == "SELL" {
            entry.1.push(t.price);
        }
    }
    let mut positive_spreads = Vec::new();
    for (buys, sells) in leg_prices.values() {
        if buys.is_empty() || sells.is_empty() {
            continue;
        }
        let b = buys.iter().sum::<f64>() / buys.len() as f64;
        let s = sells.iter().sum::<f64>() / sells.len() as f64;
        if s > b {
            positive_spreads.push(s - b);
        }
    }
    let take_profit = clamp(percentile(positive_spreads, 0.50, 0.02), 0.005, 0.08);

    let hedge_band_low = clamp(bias_down_target - 0.18, 0.45, 0.80);
    let hedge_band_high = clamp(bias_down_target + 0.18, 0.55, 0.92);

    StrategyParams {
        bias_down_target,
        entry_window_secs,
        down_buy_threshold,
        up_buy_threshold,
        take_profit,
        min_trade_usdc: cfg.min_trade_usdc.max(1.0),
        scalp_fraction: 0.35,
        hedge_band_low,
        hedge_band_high,
        max_event_usdc: cfg.max_event_usdc.max(0.0),
        max_total_usdc: cfg.max_total_usdc.max(0.0),
    }
}

pub async fn run_reverse_engineered_profile_paper(cfg: ReverseEngineeredConfig) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_secs(cfg.poll_interval_secs.max(5)));
    let mut state = ReverseState::default();
    let strategy_tag = REVERSE_PROFILE_STRATEGY_SLUG.replace('_', "-");

    info!(
        "Starting {} ({}) paper mode: profile={} poll={}s min_trade=${:.2} max_event=${:.2} max_total=${:.2}",
        REVERSE_PROFILE_STRATEGY_NAME,
        REVERSE_PROFILE_STRATEGY_SLUG,
        cfg.profile_url,
        cfg.poll_interval_secs,
        cfg.min_trade_usdc,
        cfg.max_event_usdc,
        cfg.max_total_usdc
    );

    println!(
        "\n[{}] {} running. Ctrl+C to stop.\n",
        strategy_tag, REVERSE_PROFILE_STRATEGY_NAME
    );

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let payload = match fetch_payload(&cfg.profile_url).await {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("{}: failed to fetch payload: {}", strategy_tag, e);
                        continue;
                    }
                };
                let snapshot = extract_profile_snapshot(&payload)
                    .with_context(|| format!("{}: failed to extract profile snapshot", strategy_tag))?;

                let params = infer_strategy_params(&snapshot, &cfg);
                apply_new_ticks(&mut state, &params, &snapshot, &cfg.target_assets);
                refresh_mark_to_market(&mut state, &snapshot);

                println!(
                    "[{}] buys={} sells={} skipped={} buy=${:.2} sell=${:.2} realized=${:.2} unrealized=${:.2} total=${:.2} down_ratio={:.2}% params(entry={}s,down<={:.3},up<={:.3},tp={:.3})",
                    strategy_tag,
                    state.result.executed_buys,
                    state.result.executed_sells,
                    state.result.skipped_orders,
                    state.result.buy_notional,
                    state.result.sell_notional,
                    state.result.realized_pnl,
                    state.result.unrealized_pnl,
                    state.result.total_pnl(),
                    state.result.down_ratio() * 100.0,
                    params.entry_window_secs,
                    params.down_buy_threshold,
                    params.up_buy_threshold,
                    params.take_profit
                );
            }
            _ = tokio::signal::ctrl_c() => {
                println!("[{}] stopping...", strategy_tag);
                break;
            }
        }
    }

    println!(
        "[{}] final: buys={} sells={} buy=${:.2} sell=${:.2} realized=${:.2} unrealized=${:.2} total=${:.2}",
        strategy_tag,
        state.result.executed_buys,
        state.result.executed_sells,
        state.result.buy_notional,
        state.result.sell_notional,
        state.result.realized_pnl,
        state.result.unrealized_pnl,
        state.result.total_pnl()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_trade(
        slug: &str,
        outcome: &str,
        side: &str,
        price: f64,
        ts: i64,
        title: &str,
    ) -> ReverseTradeEvent {
        ReverseTradeEvent {
            event_slug: slug.to_string(),
            outcome: outcome.to_string(),
            side: side.to_string(),
            price,
            size: 10.0,
            usdc_size: 10.0 * price,
            timestamp: ts,
            title: title.to_string(),
            raw_type: "TRADE".to_string(),
            transaction_hash: format!("0x{}", ts),
        }
    }

    #[test]
    fn test_infer_bias_from_positions() {
        let snapshot = ProfileSnapshot {
            address: "0xabc".to_string(),
            activity: vec![],
            positions: vec![
                serde_json::json!({"eventSlug":"e1","outcome":"Down","currentValue":80.0,"curPrice":0.8}),
                serde_json::json!({"eventSlug":"e2","outcome":"Up","currentValue":20.0,"curPrice":0.2}),
            ],
        };
        let cfg = ReverseEngineeredConfig::default();
        let params = infer_strategy_params(&snapshot, &cfg);
        assert!(params.bias_down_target >= 0.75);
    }

    #[test]
    fn test_reverse_engine_executes_buys_and_sells() {
        let slug = "btc-updown-5m-1000000000";
        let snapshot = ProfileSnapshot {
            address: "0xabc".to_string(),
            activity: vec![
                mk_trade(slug, "Down", "SELL", 0.22, 1000000260, "Bitcoin Up or Down"),
                mk_trade(slug, "Down", "SELL", 0.24, 1000000270, "Bitcoin Up or Down"),
                mk_trade(slug, "Down", "SELL", 0.30, 1000000290, "Bitcoin Up or Down"),
            ],
            positions: vec![serde_json::json!({"eventSlug":slug,"outcome":"Down","curPrice":0.31})],
        };
        let cfg = ReverseEngineeredConfig::default();
        let params = infer_strategy_params(&snapshot, &cfg);
        let mut state = ReverseState::default();
        apply_new_ticks(&mut state, &params, &snapshot, &cfg.target_assets);
        refresh_mark_to_market(&mut state, &snapshot);
        assert!(state.result.executed_buys >= 1);
        assert!(state.result.executed_sells >= 1);
    }
}
