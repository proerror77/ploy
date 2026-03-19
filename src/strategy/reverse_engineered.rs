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

use crate::error::{PloyError, Result};

mod dry_run_flow;
mod profile_payload;

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

fn to_i64(v: &Value) -> i64 {
    if let Some(x) = v.as_i64() {
        return x;
    }
    if let Some(s) = v.as_str() {
        if let Ok(x) = s.parse::<i64>() {
            return x;
        }
    }
    0
}

fn to_string(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
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

fn extract_json_object_from_html(body: &str) -> Result<String> {
    let start = body.find("{\"props\":{").ok_or_else(|| {
        PloyError::Validation("unable to locate profile json in html".to_string())
    })?;
    let bytes = body.as_bytes();
    let mut depth: i64 = 0;
    let mut in_str = false;
    let mut escaped = false;
    let mut end: Option<usize> = None;

    for (idx, b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
                continue;
            }
            if *b == b'\\' {
                escaped = true;
            } else if *b == b'"' {
                in_str = false;
            }
            continue;
        }

        match *b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(idx);
                    break;
                }
            }
            _ => {}
        }
    }

    let end_idx = end.ok_or_else(|| {
        PloyError::Validation("unterminated embedded json object in profile html".to_string())
    })?;

    Ok(body[start..=end_idx].to_string())
}

async fn fetch_payload(url_or_file: &str) -> Result<Value> {
    if url_or_file.starts_with("http://") || url_or_file.starts_with("https://") {
        let body = reqwest::get(url_or_file).await?.text().await?;
        let json_text = extract_json_object_from_html(&body)?;
        let value: Value = serde_json::from_str(&json_text)?;
        return Ok(value);
    }

    let raw = std::fs::read_to_string(url_or_file)?;
    if raw.trim_start().starts_with('<') {
        let json_text = extract_json_object_from_html(&raw)?;
        let value: Value = serde_json::from_str(&json_text)?;
        Ok(value)
    } else {
        let value: Value = serde_json::from_str(&raw)?;
        Ok(value)
    }
}

fn flatten_pages(data: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let pages = data
        .as_object()
        .and_then(|obj| obj.get("pages"))
        .and_then(Value::as_array);
    let Some(pages) = pages else {
        return out;
    };
    for page in pages {
        if let Some(rows) = page.as_array() {
            for row in rows {
                out.push(row.clone());
            }
        }
    }
    out
}

pub fn extract_profile_snapshot(payload: &Value) -> Result<ProfileSnapshot> {
    let page_props = payload
        .get("props")
        .and_then(Value::as_object)
        .and_then(|x| x.get("pageProps"))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            PloyError::Validation("invalid payload: missing props.pageProps".to_string())
        })?;

    let address = page_props
        .get("proxyAddress")
        .or_else(|| page_props.get("primaryAddress"))
        .or_else(|| page_props.get("baseAddress"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let queries = page_props
        .get("dehydratedState")
        .and_then(Value::as_object)
        .and_then(|x| x.get("queries"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            PloyError::Validation("invalid payload: missing dehydratedState.queries".to_string())
        })?;

    let mut activity_rows: Vec<Value> = Vec::new();
    let mut position_rows: Vec<Value> = Vec::new();

    for q in queries {
        let Some(query_obj) = q.as_object() else {
            continue;
        };
        let Some(query_key) = query_obj.get("queryKey").and_then(Value::as_array) else {
            continue;
        };
        if query_key.len() < 2 {
            continue;
        }
        let key0 = query_key[0].as_str().unwrap_or_default();
        let key1 = query_key[1].as_str().unwrap_or_default();
        let data = query_obj
            .get("state")
            .and_then(Value::as_object)
            .and_then(|x| x.get("data"))
            .cloned()
            .unwrap_or(Value::Null);

        if key0 == "profile" && key1 == "activity" {
            activity_rows.extend(flatten_pages(&data));
        }
        if key0 == "profile" && key1 == "positions" {
            position_rows.extend(flatten_pages(&data));
        }
    }

    let mut activity: Vec<ReverseTradeEvent> = activity_rows
        .into_iter()
        .map(|row| {
            let obj = row.as_object().cloned().unwrap_or_default();
            ReverseTradeEvent {
                event_slug: obj.get("eventSlug").map(to_string).unwrap_or_default(),
                outcome: obj.get("outcome").map(to_string).unwrap_or_default(),
                side: obj.get("side").map(to_string).unwrap_or_default(),
                price: obj.get("price").map(to_f64).unwrap_or_default(),
                size: obj.get("size").map(to_f64).unwrap_or_default(),
                usdc_size: obj.get("usdcSize").map(to_f64).unwrap_or_default(),
                timestamp: obj.get("timestamp").map(to_i64).unwrap_or_default(),
                title: obj.get("title").map(to_string).unwrap_or_default(),
                raw_type: obj.get("type").map(to_string).unwrap_or_default(),
                transaction_hash: obj
                    .get("transactionHash")
                    .map(to_string)
                    .unwrap_or_default(),
            }
        })
        .collect();

    activity.sort_by_key(|x| x.timestamp);

    Ok(ProfileSnapshot {
        address,
        activity,
        positions: position_rows,
    })
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

fn mark_prices_from_positions(positions: &[Value]) -> HashMap<(String, String), f64> {
    let mut out = HashMap::new();
    for row in positions {
        let Some(obj) = row.as_object() else {
            continue;
        };
        let event_slug = obj
            .get("eventSlug")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let outcome = obj
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let px = obj.get("curPrice").map(to_f64).unwrap_or_default();
        if !event_slug.is_empty() && !outcome.is_empty() && px > 0.0 {
            out.insert((event_slug, outcome), px);
        }
    }
    out
}

fn title_allowed(title: &str, target_assets: &[String]) -> bool {
    if target_assets.is_empty() {
        return true;
    }
    let up = title.to_ascii_uppercase();
    target_assets
        .iter()
        .any(|x| up.contains(&x.to_ascii_uppercase()))
}

fn inventory_down_ratio(
    inventory: &HashMap<(String, String), Vec<(f64, f64)>>,
    prices: &HashMap<(String, String), f64>,
) -> f64 {
    let mut down = 0.0;
    let mut up = 0.0;
    for ((event_slug, outcome), lots) in inventory {
        let Some(px) = prices.get(&(event_slug.clone(), outcome.clone())) else {
            continue;
        };
        let qty: f64 = lots.iter().map(|(q, _)| *q).sum();
        let val = qty * px;
        if outcome.eq_ignore_ascii_case("Down") {
            down += val;
        } else if outcome.eq_ignore_ascii_case("Up") {
            up += val;
        }
    }
    let total = down + up;
    if total <= 0.0 {
        0.5
    } else {
        down / total
    }
}

fn execute_buy(st: &mut ReverseState, params: &StrategyParams, ev: &ReverseTradeEvent) -> bool {
    let usdc = params.min_trade_usdc;
    if params.max_event_usdc > 0.0 {
        let spent = st.event_spend.get(&ev.event_slug).copied().unwrap_or(0.0);
        if spent + usdc > params.max_event_usdc {
            st.result.skipped_orders += 1;
            return false;
        }
    }
    if params.max_total_usdc > 0.0 && st.result.buy_notional + usdc > params.max_total_usdc {
        st.result.skipped_orders += 1;
        return false;
    }
    let qty = usdc / ev.price;
    st.inventory
        .entry((ev.event_slug.clone(), ev.outcome.clone()))
        .or_default()
        .push((qty, ev.price));
    *st.event_spend.entry(ev.event_slug.clone()).or_insert(0.0) += usdc;
    st.result.buy_notional += usdc;
    st.result.executed_buys += 1;
    true
}

fn execute_sell(st: &mut ReverseState, ev: &ReverseTradeEvent, fraction: f64) -> bool {
    let key = (ev.event_slug.clone(), ev.outcome.clone());
    let Some(lots) = st.inventory.get_mut(&key) else {
        st.result.skipped_orders += 1;
        return false;
    };
    let total_qty: f64 = lots.iter().map(|(q, _)| *q).sum();
    if total_qty <= 0.0 {
        st.result.skipped_orders += 1;
        return false;
    }
    let mut remain = total_qty * clamp(fraction, 0.0, 1.0);
    let mut closed = 0.0;
    while remain > 1e-9 && !lots.is_empty() {
        let (lot_qty, lot_px) = lots[0];
        let take = remain.min(lot_qty);
        st.result.realized_pnl += (ev.price - lot_px) * take;
        let left = lot_qty - take;
        remain -= take;
        closed += take;
        if left <= 1e-9 {
            lots.remove(0);
        } else {
            lots[0] = (left, lot_px);
        }
    }
    if closed <= 0.0 {
        st.result.skipped_orders += 1;
        return false;
    }
    st.result.sell_notional += closed * ev.price;
    st.result.executed_sells += 1;
    true
}

fn apply_new_ticks(
    st: &mut ReverseState,
    params: &StrategyParams,
    snapshot: &ProfileSnapshot,
    target_assets: &[String],
) {
    let mut ticks: Vec<&ReverseTradeEvent> = snapshot
        .activity
        .iter()
        .filter(|x| x.raw_type == "TRADE" && x.price > 0.0 && x.size > 0.0)
        .collect();
    ticks.sort_by_key(|x| x.timestamp);

    for ev in ticks {
        let key_id = if ev.transaction_hash.is_empty() {
            format!(
                "{}|{}|{}|{:.8}|{:.8}|{}",
                ev.timestamp, ev.event_slug, ev.outcome, ev.price, ev.size, ev.side
            )
        } else {
            format!("{}|{}|{}", ev.transaction_hash, ev.outcome, ev.side)
        };
        if st.seen_trade_ids.contains(&key_id) {
            continue;
        }
        st.seen_trade_ids.insert(key_id);

        if !title_allowed(&ev.title, target_assets) {
            continue;
        }

        st.latest_px
            .insert((ev.event_slug.clone(), ev.outcome.clone()), ev.price);

        let Some(end_ts) = parse_event_end_ts(&ev.event_slug) else {
            continue;
        };
        let tte = end_ts - ev.timestamp;
        if tte < 0 {
            continue;
        }

        let down_ratio = inventory_down_ratio(&st.inventory, &st.latest_px);
        if tte <= params.entry_window_secs {
            if ev.outcome.eq_ignore_ascii_case("Down") {
                if ev.price <= params.down_buy_threshold && down_ratio <= params.hedge_band_high {
                    let _ = execute_buy(st, params, ev);
                }
            } else if ev.outcome.eq_ignore_ascii_case("Up") {
                if ev.price <= params.up_buy_threshold && down_ratio >= params.hedge_band_low {
                    let _ = execute_buy(st, params, ev);
                }
            }
        }

        let inv_key = (ev.event_slug.clone(), ev.outcome.clone());
        if let Some(lots) = st.inventory.get(&inv_key) {
            if !lots.is_empty() {
                let total_qty: f64 = lots.iter().map(|(q, _)| *q).sum();
                if total_qty > 0.0 {
                    let avg_entry =
                        lots.iter().map(|(q, p)| q * p).sum::<f64>() / total_qty.max(1e-9);
                    if ev.price >= avg_entry + params.take_profit {
                        let _ = execute_sell(st, ev, params.scalp_fraction);
                    } else if tte <= 20
                        && ev.price >= avg_entry + (params.take_profit * 0.6).max(0.01)
                    {
                        let _ = execute_sell(st, ev, 0.20);
                    }
                }
            }
        }
    }
}

fn refresh_mark_to_market(st: &mut ReverseState, snapshot: &ProfileSnapshot) {
    let mut marks = mark_prices_from_positions(&snapshot.positions);
    for (k, v) in &st.latest_px {
        marks.entry(k.clone()).or_insert(*v);
    }

    st.result.unrealized_pnl = 0.0;
    st.result.open_mark_value = 0.0;
    st.result.down_mark_value = 0.0;
    st.result.up_mark_value = 0.0;

    for ((event_slug, outcome), lots) in &st.inventory {
        let Some(px) = marks.get(&(event_slug.clone(), outcome.clone())) else {
            continue;
        };
        let qty: f64 = lots.iter().map(|(q, _)| *q).sum();
        let cost: f64 = lots.iter().map(|(q, p)| q * p).sum();
        let value = qty * px;

        st.result.open_mark_value += value;
        st.result.unrealized_pnl += value - cost;
        if outcome.eq_ignore_ascii_case("Down") {
            st.result.down_mark_value += value;
        } else if outcome.eq_ignore_ascii_case("Up") {
            st.result.up_mark_value += value;
        }
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
