use std::collections::{BTreeMap, HashMap, VecDeque};

use chrono::{DateTime, Utc};
use ploy_market_contracts::MarketUpdate;
#[cfg(feature = "polars-export")]
use polars::prelude::*;
use rust_decimal::prelude::ToPrimitive;
#[cfg(feature = "db")]
use serde_json::Value;
#[cfg(feature = "db")]
use sqlx::PgPool;

const EWMA_LAMBDA: f64 = 0.94;
const RETURN_BUFFER_WINDOW_SECS: f64 = 300.0;

#[derive(Debug, Clone)]
pub struct FactorObservation {
    pub event_id: String,
    pub symbol: String,
    pub tick_ts: DateTime<Utc>,
    pub time_remaining_secs: i64,
    pub signed_distance_to_beat: f64,
    pub abs_distance_to_beat: f64,
    pub drift_10s: f64,
    pub drift_30s: f64,
    pub flip_age_secs: f64,
    pub post_flip_drift: f64,
    pub sigma_horizon: f64,
    pub fair_prob_up: f64,
    pub fair_prob_up_clean: f64,
    pub prob_disagreement: f64,
    pub implied_sigma_horizon: f64,
    pub vol_gap: f64,
    pub distance_over_sigma: f64,
    pub model_prob_up: f64,
    pub model_edge_up: f64,
    pub reward_risk_up: f64,
    pub reward_risk_down: f64,
    pub obi: f64,
    pub spread_bps: f64,
    pub microprice_offset_bps: f64,
    pub bid_depth_near: f64,
    pub ask_depth_near: f64,
    pub depth_ratio: f64,
    pub depth_imbalance: f64,
    pub depth_far_ratio: f64,
    pub depth_acceleration: f64,
    pub obi_10: f64,
    pub pm_up_bid: f64,
    pub pm_up_ask: f64,
    pub pm_up_bid_size: f64,
    pub pm_up_ask_size: f64,
    pub pm_down_bid: f64,
    pub pm_down_ask: f64,
    pub pm_down_bid_size: f64,
    pub pm_down_ask_size: f64,
    pub pm_lag_secs: f64,
    pub settlement_up: f64,
    pub future_up_ask_change_30s: Option<f64>,
    pub future_up_ask_change_60s: Option<f64>,
    pub cum_obi_delta_5m: f64,
    pub cum_depth_delta_5m: f64,
    pub cum_mprice_drift_5m: f64,
    pub cum_trade_imbalance_5m: f64,
}

#[derive(Debug, Clone)]
pub struct EventFactorSummary {
    pub event_id: String,
    pub symbol: String,
    pub last_tick_ts: DateTime<Utc>,
    pub settlement_up: f64,
    pub signed_distance_to_beat: f64,
    pub abs_distance_to_beat: f64,
    pub drift_10s: f64,
    pub drift_30s: f64,
    pub flip_age_secs: f64,
    pub post_flip_drift: f64,
    pub sigma_horizon: f64,
    pub fair_prob_up: f64,
    pub fair_prob_up_clean: f64,
    pub prob_disagreement: f64,
    pub implied_sigma_horizon: f64,
    pub vol_gap: f64,
    pub distance_over_sigma: f64,
    pub model_prob_up: f64,
    pub model_edge_up: f64,
    pub reward_risk_up: f64,
    pub reward_risk_down: f64,
    pub obi: f64,
    pub spread_bps: f64,
    pub microprice_offset_bps: f64,
    pub bid_depth_near: f64,
    pub ask_depth_near: f64,
    pub depth_ratio: f64,
    pub depth_imbalance: f64,
    pub depth_far_ratio: f64,
    pub depth_acceleration: f64,
    pub obi_10: f64,
    pub pm_up_bid: f64,
    pub pm_up_ask: f64,
    pub pm_up_bid_size: f64,
    pub pm_up_ask_size: f64,
    pub pm_down_bid: f64,
    pub pm_down_ask: f64,
    pub pm_down_bid_size: f64,
    pub pm_down_ask_size: f64,
    pub pm_lag_secs: f64,
    pub cum_obi_delta_5m: f64,
    pub cum_depth_delta_5m: f64,
    pub cum_mprice_drift_5m: f64,
    pub cum_trade_imbalance_5m: f64,
}

#[derive(Debug, Clone)]
pub struct TaskGrainDerivedArtifacts {
    pub event_ids: Vec<String>,
    pub observation_rows: Vec<FactorObservation>,
    pub event_summaries: Vec<EventFactorSummary>,
}

impl TaskGrainDerivedArtifacts {
    pub fn observation_row_count(&self) -> usize {
        self.observation_rows.len()
    }

    pub fn event_summary_count(&self) -> usize {
        self.event_summaries.len()
    }

    pub fn repricing_label_row_count_30s(&self) -> usize {
        self.observation_rows
            .iter()
            .filter(|row| row.future_up_ask_change_30s.is_some_and(f64::is_finite))
            .count()
    }

    pub fn settlement_label_event_count(&self) -> usize {
        self.event_summaries
            .iter()
            .filter(|row| row.settlement_up.is_finite())
            .count()
    }
}

#[derive(Debug, Clone)]
pub struct FactorMetric {
    pub label: String,
    pub factor: String,
    pub n: usize,
    pub pearson_ic: f64,
    pub spearman_ic: f64,
    pub icir: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct AggregatedFactorMetric {
    pub label: String,
    pub factor: String,
    pub windows: usize,
    pub mean_n: f64,
    pub mean_pearson_ic: f64,
    pub mean_spearman_ic: f64,
    pub icir: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ResearchLobSnapshot {
    pub symbol: String,
    pub ts: DateTime<Utc>,
    pub obi: f64,
    pub obi_10: f64,
    pub spread_bps: f64,
    pub best_bid: f64,
    pub best_ask: f64,
    pub mid_price: f64,
    pub bid_depth_near: f64,
    pub ask_depth_near: f64,
    pub bid_depth_far: f64,
    pub ask_depth_far: f64,
    pub bid_depth_inner: f64,
    pub ask_depth_inner: f64,
}

#[derive(Clone, Default)]
struct EventState {
    event_id: String,
    start_time: Option<DateTime<Utc>>,
    end_time: Option<DateTime<Utc>>,
    window_secs: Option<i64>,
    price_to_beat: Option<f64>,
    resolved_up_won: Option<bool>,
    up_token: String,
    down_token: String,
}

#[derive(Clone, Default)]
struct LobState {
    obi: f64,
    spread_bps: f64,
    mid_price: f64,
    best_bid: f64,
    best_ask: f64,
    bid_depth_near: f64,
    ask_depth_near: f64,
    bid_depth_far: f64,
    ask_depth_far: f64,
    bid_depth_inner: f64,
    ask_depth_inner: f64,
    obi_10: f64,
}

#[derive(Clone, Default)]
struct VolatilityState {
    ewma_var_per_sec: f64,
}

#[derive(Clone, Default)]
struct DriftState {
    prev_drift_30s: f64,
    flip_ts: Option<DateTime<Utc>>,
    post_flip_drift: f64,
}

struct DriftBuffer {
    entries: VecDeque<(DateTime<Utc>, f64)>,
    window_secs: f64,
}

impl DriftBuffer {
    fn new(window_secs: f64) -> Self {
        Self {
            entries: VecDeque::new(),
            window_secs,
        }
    }

    fn push(&mut self, ts: DateTime<Utc>, price: f64) {
        self.entries.push_back((ts, price.ln()));
        while self.entries.len() > 1 {
            let oldest = self.entries.front().expect("front exists").0;
            let elapsed = (ts - oldest).num_milliseconds() as f64 / 1000.0;
            if elapsed > self.window_secs {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    fn drift_speed(&self) -> f64 {
        if self.entries.len() < 2 {
            return 0.0;
        }
        let (t0, p0) = self.entries.front().expect("front exists");
        let (t1, p1) = self.entries.back().expect("back exists");
        let dt = ((*t1 - *t0).num_milliseconds() as f64 / 1000.0).max(0.001);
        (p1 - p0) / dt
    }
}

struct ReturnBuffer {
    entries: VecDeque<(f64, f64, f64)>,
    total_secs: f64,
    high: f64,
    low: f64,
}

impl ReturnBuffer {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            total_secs: 0.0,
            high: f64::NEG_INFINITY,
            low: f64::INFINITY,
        }
    }

    fn push(&mut self, log_return: f64, dt_secs: f64, price: f64) {
        self.entries.push_back((log_return, dt_secs, price));
        self.total_secs += dt_secs;
        self.high = self.high.max(price);
        self.low = self.low.min(price);

        while self.total_secs > RETURN_BUFFER_WINDOW_SECS && self.entries.len() > 2 {
            if let Some((_, old_dt, _)) = self.entries.pop_front() {
                self.total_secs -= old_dt;
            }
        }
    }

    fn realized_var_per_sec(&self) -> f64 {
        if self.total_secs <= 0.0 {
            return 0.0;
        }
        self.entries
            .iter()
            .map(|(ret, _, _)| ret * ret)
            .sum::<f64>()
            / self.total_secs
    }

    fn parkinson_var_per_sec(&self) -> f64 {
        if self.high <= 0.0 || self.low <= 0.0 || self.high <= self.low || self.total_secs <= 0.0 {
            return 0.0;
        }
        let log_hl = (self.high / self.low).ln();
        log_hl * log_hl / (4.0 * std::f64::consts::LN_2 * self.total_secs)
    }
}

/// Load LOB snapshots for research, downsampled to one tick per `sample_every_secs` seconds.

/// Rolling accumulator for LOB flow signals over a time window.
struct LobFlowAccumulator {
    entries: VecDeque<(DateTime<Utc>, f64, f64, f64)>, // (ts, obi, depth_imbalance, microprice_offset_bps)
    window_secs: f64,
}

impl LobFlowAccumulator {
    fn new(window_secs: f64) -> Self {
        Self {
            entries: VecDeque::new(),
            window_secs,
        }
    }

    fn push(
        &mut self,
        ts: DateTime<Utc>,
        obi: f64,
        depth_imbalance: f64,
        microprice_offset_bps: f64,
    ) {
        self.entries
            .push_back((ts, obi, depth_imbalance, microprice_offset_bps));
        while self.entries.len() > 1 {
            let oldest = self.entries.front().unwrap().0;
            if (ts - oldest).num_milliseconds() as f64 / 1000.0 > self.window_secs {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    /// Sum of consecutive OBI differences within the window.
    fn cum_obi_delta(&self) -> f64 {
        if self.entries.len() < 2 {
            return 0.0;
        }
        self.entries
            .iter()
            .zip(self.entries.iter().skip(1))
            .map(|(a, b)| b.1 - a.1)
            .sum()
    }

    /// Sum of consecutive depth_imbalance differences within the window.
    fn cum_depth_delta(&self) -> f64 {
        if self.entries.len() < 2 {
            return 0.0;
        }
        self.entries
            .iter()
            .zip(self.entries.iter().skip(1))
            .map(|(a, b)| b.2 - a.2)
            .sum()
    }

    /// Sum of microprice_offset_bps within the window (level, not delta).
    fn cum_mprice_drift(&self) -> f64 {
        self.entries.iter().map(|e| e.3).sum()
    }
}

/// Rolling accumulator for signed trade flow over a time window.
struct TradeFlowAccumulator {
    entries: VecDeque<(DateTime<Utc>, f64)>, // (ts, signed_qty)
    window_secs: f64,
}

impl TradeFlowAccumulator {
    fn new(window_secs: f64) -> Self {
        Self {
            entries: VecDeque::new(),
            window_secs,
        }
    }

    fn push(&mut self, ts: DateTime<Utc>, signed_qty: f64) {
        self.entries.push_back((ts, signed_qty));
        while self.entries.len() > 1 {
            let oldest = self.entries.front().unwrap().0;
            if (ts - oldest).num_milliseconds() as f64 / 1000.0 > self.window_secs {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    fn cum_imbalance(&self) -> f64 {
        self.entries.iter().map(|e| e.1).sum()
    }
}

/// Load LOB snapshots for research, downsampled to one tick per `sample_every_secs` seconds.
///
/// `binance_lob_ticks` records at ~1 Hz; for factor research 1 tick per 5 s is sufficient
/// and reduces JSONB transfer by ~5x. Pass `sample_every_secs = 1` to disable downsampling.
#[cfg(feature = "db")]
pub async fn load_research_lob_snapshots(
    pool: &PgPool,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<ResearchLobSnapshot>, sqlx::Error> {
    load_research_lob_snapshots_sampled(pool, symbols, start, end, 5).await
}

/// Loads LOB snapshots from `binance_lob_ticks`, keeping one tick per symbol per
/// `sample_every_secs` bucket. This reduces JSONB transfer for multi-day
/// research runs at the cost of temporal resolution.
///
/// This is a bucket sampler, not a full resampler: each bucket keeps the latest
/// snapshot inside the bucket. It avoids transferring every high-frequency book
/// row when the collector records more than one update per second.
///
/// `sample_every_secs` is clamped to a minimum of 1 (no divide-by-zero).
#[cfg(feature = "db")]
pub async fn load_research_lob_snapshots_sampled(
    pool: &PgPool,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_every_secs: i32,
) -> Result<Vec<ResearchLobSnapshot>, sqlx::Error> {
    let sample_every_secs = sample_every_secs.max(1);
    let rows: Vec<(
        DateTime<Utc>,
        String,
        rust_decimal::Decimal,
        rust_decimal::Decimal,
        rust_decimal::Decimal,
        rust_decimal::Decimal,
        rust_decimal::Decimal,
        rust_decimal::Decimal,
        Value,
        Value,
    )> = sqlx::query_as(
        r#"
        WITH buckets AS (
            SELECT s.symbol, bucket_start
            FROM unnest($1::text[]) AS s(symbol)
            CROSS JOIN generate_series(
                $2::timestamptz,
                $3::timestamptz,
                ($4::text || ' seconds')::interval
            ) AS bucket_start
        )
        SELECT
            lob.event_time,
            lob.symbol,
            COALESCE(lob.obi_5, 0) AS obi_5,
            COALESCE(lob.obi_10, 0) AS obi_10,
            COALESCE(lob.spread_bps, 0) AS spread_bps,
            COALESCE(lob.best_bid, 0) AS best_bid,
            COALESCE(lob.best_ask, 0) AS best_ask,
            COALESCE(lob.mid_price, 0) AS mid_price,
            lob.bids,
            lob.asks
        FROM buckets
        JOIN LATERAL (
            SELECT
                event_time,
                symbol,
                obi_5,
                obi_10,
                spread_bps,
                best_bid,
                best_ask,
                mid_price,
                bids,
                asks
            FROM binance_lob_ticks
            WHERE symbol = buckets.symbol
              AND event_time >= buckets.bucket_start
              AND event_time < buckets.bucket_start + ($4::text || ' seconds')::interval
              AND event_time <= $3
            ORDER BY event_time DESC
            LIMIT 1
        ) AS lob ON true
        ORDER BY lob.event_time
        "#,
    )
    .bind(symbols)
    .bind(start)
    .bind(end)
    .bind(sample_every_secs)
    .fetch_all(pool)
    .await?;

    eprintln!(
        "lob snapshot rows: {} (sample_every_secs={})",
        rows.len(),
        sample_every_secs
    );

    Ok(rows
        .into_iter()
        .map(
            |(ts, symbol, obi_5, obi_10, spread_bps, best_bid, best_ask, mid_price, bids, asks)| {
                let mid_price = mid_price.to_f64().unwrap_or(f64::NAN);
                let (bid_depth_near, ask_depth_near) = depth_band(&bids, &asks, mid_price, 0.001);
                let (bid_depth_far, ask_depth_far) = depth_band(&bids, &asks, mid_price, 0.005);
                // Inner band: much tighter than near, so depth_acceleration
                // (inner_ratio - near_ratio) has variance even when the book
                // is very tight (e.g. BTC where all 20 levels sit within 0.007% of mid).
                let (bid_depth_inner, ask_depth_inner) =
                    depth_band(&bids, &asks, mid_price, 0.00003);

                ResearchLobSnapshot {
                    symbol,
                    ts,
                    obi: obi_5.to_f64().unwrap_or(0.0),
                    obi_10: obi_10.to_f64().unwrap_or(0.0),
                    spread_bps: spread_bps.to_f64().unwrap_or(0.0),
                    best_bid: best_bid.to_f64().unwrap_or(f64::NAN),
                    best_ask: best_ask.to_f64().unwrap_or(f64::NAN),
                    mid_price,
                    bid_depth_near,
                    ask_depth_near,
                    bid_depth_far,
                    ask_depth_far,
                    bid_depth_inner,
                    ask_depth_inner,
                }
            },
        )
        .collect())
}

#[cfg(feature = "db")]
fn depth_band(bids: &Value, asks: &Value, mid_price: f64, pct_range: f64) -> (f64, f64) {
    if !mid_price.is_finite() || mid_price <= 0.0 {
        return (f64::NAN, f64::NAN);
    }
    let bid_min = mid_price * (1.0 - pct_range);
    let ask_max = mid_price * (1.0 + pct_range);
    (
        sum_depth_in_range(bids, bid_min, mid_price),
        sum_depth_in_range(asks, mid_price, ask_max),
    )
}

#[cfg(feature = "db")]
fn sum_depth_in_range(levels: &Value, min_price: f64, max_price: f64) -> f64 {
    levels
        .as_array()
        .map(|levels| {
            levels
                .iter()
                .filter_map(parse_depth_level)
                .filter(|(price, _)| *price >= min_price && *price <= max_price)
                .map(|(_, size)| size)
                .sum()
        })
        .unwrap_or(0.0)
}

#[cfg(feature = "db")]
fn parse_depth_level(level: &Value) -> Option<(f64, f64)> {
    match level {
        Value::Array(items) if items.len() >= 2 => {
            Some((json_f64(&items[0])?, json_f64(&items[1])?))
        }
        Value::Object(map) => Some((json_f64(map.get("price")?)?, json_f64(map.get("size")?)?)),
        _ => None,
    }
}

#[cfg(feature = "db")]
fn json_f64(value: &Value) -> Option<f64> {
    match value {
        Value::String(raw) => raw.parse::<f64>().ok(),
        Value::Number(number) => number.as_f64(),
        _ => None,
    }
}

pub fn build_factor_observations(
    updates: &[MarketUpdate],
    max_quote_age_secs: i64,
) -> Vec<FactorObservation> {
    build_factor_observations_with_lob(updates, &[], max_quote_age_secs)
}

pub fn build_factor_observations_with_lob(
    updates: &[MarketUpdate],
    lob_snapshots: &[ResearchLobSnapshot],
    max_quote_age_secs: i64,
) -> Vec<FactorObservation> {
    let mut final_outcomes: HashMap<String, bool> = HashMap::new();
    for update in updates {
        match update {
            MarketUpdate::EventDiscovered {
                event_id,
                resolved_up_won: Some(outcome),
                ..
            }
            | MarketUpdate::EventExpired {
                event_id,
                resolved_up_won: Some(outcome),
                ..
            } => {
                final_outcomes.insert(event_id.to_string(), *outcome);
            }
            _ => {}
        }
    }

    let mut buf_30s: HashMap<String, DriftBuffer> = HashMap::new();
    let mut buf_10s: HashMap<String, DriftBuffer> = HashMap::new();
    let mut drift_state: HashMap<String, DriftState> = HashMap::new();
    let mut spot: HashMap<String, (DateTime<Utc>, f64)> = HashMap::new();
    let mut vol: HashMap<String, VolatilityState> = HashMap::new();
    let mut retbuf: HashMap<String, ReturnBuffer> = HashMap::new();
    let mut events: HashMap<String, EventState> = HashMap::new();
    let mut events_by_symbol: HashMap<String, Vec<String>> = HashMap::new();
    let mut quotes: HashMap<String, (DateTime<Utc>, f64, f64, f64, f64)> = HashMap::new();
    let mut lob: HashMap<String, LobState> = HashMap::new();
    let mut lob_by_symbol: HashMap<String, Vec<&ResearchLobSnapshot>> = HashMap::new();
    let mut lob_flow: HashMap<String, LobFlowAccumulator> = HashMap::new();
    let mut trade_flow: HashMap<String, TradeFlowAccumulator> = HashMap::new();
    let mut rows = Vec::new();

    for snapshot in lob_snapshots {
        lob_by_symbol
            .entry(snapshot.symbol.clone())
            .or_default()
            .push(snapshot);
    }

    let mut ordered_updates: Vec<&MarketUpdate> = updates.iter().collect();
    ordered_updates.sort_by_key(|update| update_sort_ts(update));

    for update in ordered_updates {
        match update {
            MarketUpdate::EventDiscovered {
                event_id,
                symbol,
                up_token,
                down_token,
                end_time,
                window_secs,
                price_to_beat,
                resolved_up_won,
                ..
            } => {
                let event_id = event_id.to_string();
                let symbol = symbol.to_string();
                let resolved_up_won = final_outcomes.get(&*event_id).copied().or(*resolved_up_won);
                events_by_symbol
                    .entry(symbol.clone())
                    .or_default()
                    .push(event_id.clone());
                events.insert(
                    event_id.clone(),
                    EventState {
                        event_id,
                        start_time: Some(
                            *end_time - chrono::Duration::seconds(*window_secs as i64),
                        ),
                        end_time: Some(*end_time),
                        window_secs: Some(*window_secs as i64),
                        price_to_beat: price_to_beat.and_then(|value| value.to_f64()),
                        resolved_up_won,
                        up_token: up_token.to_string(),
                        down_token: down_token.to_string(),
                    },
                );
            }
            MarketUpdate::EventExpired {
                event_id,
                resolved_up_won,
                ..
            } => {
                if let Some(event) = events.get_mut(&**event_id) {
                    event.resolved_up_won = resolved_up_won.or(event.resolved_up_won);
                }
            }
            MarketUpdate::Quote {
                token_id,
                bid,
                ask,
                bid_size,
                ask_size,
                ts,
                ..
            } => {
                let bid = bid.and_then(|value| value.to_f64()).unwrap_or(f64::NAN);
                let ask = ask.and_then(|value| value.to_f64()).unwrap_or(f64::NAN);
                let bid_sz = bid_size.and_then(|v| v.to_f64()).unwrap_or(f64::NAN);
                let ask_sz = ask_size.and_then(|v| v.to_f64()).unwrap_or(f64::NAN);
                if bid.is_finite() || ask.is_finite() {
                    quotes.insert(token_id.to_string(), (*ts, bid, ask, bid_sz, ask_sz));
                }
            }
            MarketUpdate::L2 {
                symbol,
                obi,
                spread_bps,
                ..
            } => {
                let state = lob.entry(symbol.to_string()).or_default();
                state.obi = *obi;
                state.spread_bps = *spread_bps as f64;
                state.obi_10 = *obi;
            }
            MarketUpdate::L2Depth {
                symbol,
                obi,
                spread_bps,
                bid_depth_near,
                ask_depth_near,
                ..
            } => {
                lob.insert(
                    symbol.to_string(),
                    LobState {
                        obi: *obi,
                        spread_bps: *spread_bps as f64,
                        mid_price: f64::NAN,
                        best_bid: f64::NAN,
                        best_ask: f64::NAN,
                        bid_depth_near: *bid_depth_near,
                        ask_depth_near: *ask_depth_near,
                        bid_depth_far: *bid_depth_near,
                        ask_depth_far: *ask_depth_near,
                        bid_depth_inner: *bid_depth_near,
                        ask_depth_inner: *ask_depth_near,
                        obi_10: *obi,
                    },
                );
            }
            MarketUpdate::SpotPrice { symbol, price, ts } => {
                let Some(spot_price) = price.to_f64() else {
                    continue;
                };
                if spot_price <= 0.0 {
                    continue;
                }

                let sym = symbol.to_string();
                buf_30s
                    .entry(sym.clone())
                    .or_insert_with(|| DriftBuffer::new(30.0))
                    .push(*ts, spot_price);
                buf_10s
                    .entry(sym.clone())
                    .or_insert_with(|| DriftBuffer::new(10.0))
                    .push(*ts, spot_price);

                let drift_30s = buf_30s
                    .get(&sym)
                    .map(DriftBuffer::drift_speed)
                    .unwrap_or(0.0);
                let drift_10s = buf_10s
                    .get(&sym)
                    .map(DriftBuffer::drift_speed)
                    .unwrap_or(0.0);

                let dstate = drift_state.entry(sym.clone()).or_default();
                let old_sign = signum(dstate.prev_drift_30s);
                let new_sign = signum(drift_30s);
                let flipped = old_sign != 0.0 && new_sign != 0.0 && old_sign != new_sign;
                if flipped {
                    dstate.flip_ts = Some(*ts);
                }
                dstate.prev_drift_30s = drift_30s;
                dstate.post_flip_drift = drift_30s.abs();

                if let Some((prev_ts, prev_price)) = spot.get(&sym).copied() {
                    let dt_secs = (*ts - prev_ts).num_milliseconds() as f64 / 1000.0;
                    if dt_secs > 0.0 && prev_price > 0.0 {
                        let log_return = (spot_price / prev_price).ln();
                        let inst_var_per_sec = log_return * log_return / dt_secs.max(1e-6);
                        let floor = 0.001_f64.powi(2) / 900.0;
                        let vstate = vol.entry(sym.clone()).or_default();
                        vstate.ewma_var_per_sec = if vstate.ewma_var_per_sec <= 0.0 {
                            inst_var_per_sec.max(floor)
                        } else {
                            EWMA_LAMBDA * vstate.ewma_var_per_sec
                                + (1.0 - EWMA_LAMBDA) * inst_var_per_sec
                        };

                        retbuf
                            .entry(sym.clone())
                            .or_insert_with(ReturnBuffer::new)
                            .push(log_return, dt_secs, spot_price);
                    }
                }
                spot.insert(sym.clone(), (*ts, spot_price));

                if let Some(snapshots) = lob_by_symbol.get(&sym) {
                    if let Some(snapshot) =
                        snapshots.iter().rev().find(|snapshot| snapshot.ts <= *ts)
                    {
                        // Compute microprice offset for flow accumulator
                        let snap_mprice_bps = if snapshot.mid_price > 0.0
                            && snapshot.best_bid > 0.0
                            && snapshot.best_ask > 0.0
                            && snapshot.bid_depth_near > 0.0
                            && snapshot.ask_depth_near > 0.0
                        {
                            let mp = (snapshot.best_ask * snapshot.bid_depth_near
                                + snapshot.best_bid * snapshot.ask_depth_near)
                                / (snapshot.bid_depth_near + snapshot.ask_depth_near);
                            ((mp - snapshot.mid_price) / snapshot.mid_price) * 10_000.0
                        } else {
                            0.0
                        };
                        let snap_di = if snapshot.bid_depth_near + snapshot.ask_depth_near > 0.0 {
                            (snapshot.bid_depth_near - snapshot.ask_depth_near)
                                / (snapshot.bid_depth_near + snapshot.ask_depth_near)
                        } else {
                            0.0
                        };

                        lob_flow
                            .entry(sym.clone())
                            .or_insert_with(|| LobFlowAccumulator::new(300.0))
                            .push(snapshot.ts, snapshot.obi, snap_di, snap_mprice_bps);

                        lob.insert(
                            sym.clone(),
                            LobState {
                                obi: snapshot.obi,
                                spread_bps: snapshot.spread_bps,
                                mid_price: snapshot.mid_price,
                                best_bid: snapshot.best_bid,
                                best_ask: snapshot.best_ask,
                                bid_depth_near: snapshot.bid_depth_near,
                                ask_depth_near: snapshot.ask_depth_near,
                                bid_depth_far: snapshot.bid_depth_far,
                                ask_depth_far: snapshot.ask_depth_far,
                                bid_depth_inner: snapshot.bid_depth_inner,
                                ask_depth_inner: snapshot.ask_depth_inner,
                                obi_10: snapshot.obi_10,
                            },
                        );
                    }
                }

                let Some(event_ids) = events_by_symbol.get(&sym) else {
                    continue;
                };
                for event_id in event_ids {
                    let Some(event) = events.get(event_id) else {
                        continue;
                    };
                    let Some(price_to_beat) = event.price_to_beat else {
                        continue;
                    };
                    let Some(end_time) = event.end_time else {
                        continue;
                    };
                    let Some(resolved_up_won) = event.resolved_up_won else {
                        continue;
                    };
                    let time_remaining = (end_time - *ts).num_seconds();
                    if time_remaining < 0 {
                        continue;
                    }
                    if let Some(window_secs) = event.window_secs {
                        if time_remaining > window_secs {
                            continue;
                        }
                    } else if let Some(start_time) = event.start_time {
                        if *ts < start_time {
                            continue;
                        }
                    }

                    let (up_bid, up_ask, up_lag, up_bid_sz, up_ask_sz) = quotes
                        .get(&event.up_token)
                        .map(|(quote_ts, bid, ask, bid_sz, ask_sz)| {
                            (
                                *bid,
                                *ask,
                                (*ts - *quote_ts).num_seconds() as f64,
                                *bid_sz,
                                *ask_sz,
                            )
                        })
                        .unwrap_or((f64::NAN, f64::NAN, f64::NAN, f64::NAN, f64::NAN));
                    let (down_bid, down_ask, down_bid_sz, down_ask_sz) = quotes
                        .get(&event.down_token)
                        .map(|(_, bid, ask, bid_sz, ask_sz)| (*bid, *ask, *bid_sz, *ask_sz))
                        .unwrap_or((f64::NAN, f64::NAN, f64::NAN, f64::NAN));

                    if !up_lag.is_finite() || up_lag < 0.0 || up_lag > max_quote_age_secs as f64 {
                        continue;
                    }
                    if !up_ask.is_finite() {
                        continue;
                    }

                    let lob_state = lob.get(&sym).cloned().unwrap_or_default();
                    let depth_ratio = if lob_state.ask_depth_near > 0.0 {
                        lob_state.bid_depth_near / lob_state.ask_depth_near
                    } else {
                        f64::NAN
                    };
                    let depth_imbalance =
                        if lob_state.bid_depth_near + lob_state.ask_depth_near > 0.0 {
                            (lob_state.bid_depth_near - lob_state.ask_depth_near)
                                / (lob_state.bid_depth_near + lob_state.ask_depth_near)
                        } else {
                            f64::NAN
                        };
                    let depth_far_ratio = if lob_state.ask_depth_far > 0.0 {
                        lob_state.bid_depth_far / lob_state.ask_depth_far
                    } else {
                        f64::NAN
                    };
                    let depth_inner_ratio = if lob_state.ask_depth_inner > 0.0 {
                        lob_state.bid_depth_inner / lob_state.ask_depth_inner
                    } else {
                        f64::NAN
                    };
                    // depth_acceleration: difference between inner-book and full-book
                    // depth imbalance. Uses the 0.003% inner band vs 0.1% near band
                    // so that even very tight books (BTC) produce non-zero variance.
                    let depth_acceleration =
                        if depth_inner_ratio.is_finite() && depth_ratio.is_finite() {
                            depth_inner_ratio - depth_ratio
                        } else {
                            f64::NAN
                        };
                    let microprice_offset_bps = if lob_state.mid_price.is_finite()
                        && lob_state.best_bid.is_finite()
                        && lob_state.best_ask.is_finite()
                        && lob_state.bid_depth_near > 0.0
                        && lob_state.ask_depth_near > 0.0
                    {
                        let microprice = ((lob_state.best_ask * lob_state.bid_depth_near)
                            + (lob_state.best_bid * lob_state.ask_depth_near))
                            / (lob_state.bid_depth_near + lob_state.ask_depth_near);
                        ((microprice - lob_state.mid_price) / lob_state.mid_price) * 10_000.0
                    } else {
                        f64::NAN
                    };

                    let floor = 0.001_f64.powi(2) / 900.0;
                    let ewma = vol
                        .get(&sym)
                        .map(|state| state.ewma_var_per_sec)
                        .unwrap_or(floor);
                    let (rv, parkinson) = retbuf
                        .get(&sym)
                        .map(|buf| (buf.realized_var_per_sec(), buf.parkinson_var_per_sec()))
                        .unwrap_or((0.0, 0.0));
                    let best_var = ewma.max(rv).max(parkinson).max(floor);
                    let sigma_horizon = (best_var * (time_remaining.max(1) as f64)).sqrt();
                    let signed_distance = (spot_price - price_to_beat) / price_to_beat;
                    let distance_over_sigma = if sigma_horizon > 0.0 {
                        signed_distance / sigma_horizon
                    } else {
                        f64::NAN
                    };

                    let up_break_even_prob = if up_ask.is_finite() {
                        (up_ask + crypto_fee_cost(up_ask)).clamp(1e-4, 1.0 - 1e-4)
                    } else {
                        f64::NAN
                    };
                    let down_break_even_prob = if down_ask.is_finite() {
                        (down_ask + crypto_fee_cost(down_ask)).clamp(1e-4, 1.0 - 1e-4)
                    } else {
                        f64::NAN
                    };
                    let fair_prob_up = fair_market_prob_up(up_bid, up_ask, down_bid, down_ask);
                    let fair_prob_up_clean = clean_market_prob_up(
                        up_bid,
                        up_ask,
                        down_bid,
                        down_ask,
                        up_break_even_prob,
                        down_break_even_prob,
                    );
                    let prob_disagreement =
                        implied_prob_disagreement(up_break_even_prob, down_break_even_prob);
                    let implied_sigma_horizon =
                        implied_sigma_horizon(price_to_beat, spot_price, fair_prob_up_clean);
                    let vol_gap = if implied_sigma_horizon.is_finite() {
                        implied_sigma_horizon - sigma_horizon
                    } else {
                        f64::NAN
                    };

                    let model_prob_up =
                        estimate_probability(price_to_beat, spot_price, sigma_horizon);
                    let model_edge_up = if up_ask.is_finite() {
                        model_prob_up - up_ask - crypto_fee_cost(up_ask)
                    } else {
                        f64::NAN
                    };
                    let reward_risk_up = reward_risk_ratio(up_ask);
                    let reward_risk_down = reward_risk_ratio(down_ask);
                    let flip_age_secs = dstate
                        .flip_ts
                        .map(|flip_ts| (*ts - flip_ts).num_milliseconds() as f64 / 1000.0)
                        .unwrap_or(f64::NAN);

                    rows.push(FactorObservation {
                        event_id: event.event_id.clone(),
                        symbol: sym.clone(),
                        tick_ts: *ts,
                        time_remaining_secs: time_remaining,
                        signed_distance_to_beat: signed_distance,
                        abs_distance_to_beat: signed_distance.abs(),
                        drift_10s,
                        drift_30s,
                        flip_age_secs,
                        post_flip_drift: dstate.post_flip_drift,
                        sigma_horizon,
                        fair_prob_up,
                        fair_prob_up_clean,
                        prob_disagreement,
                        implied_sigma_horizon,
                        vol_gap,
                        distance_over_sigma,
                        model_prob_up,
                        model_edge_up,
                        reward_risk_up,
                        reward_risk_down,
                        obi: lob_state.obi,
                        spread_bps: lob_state.spread_bps,
                        microprice_offset_bps,
                        bid_depth_near: lob_state.bid_depth_near,
                        ask_depth_near: lob_state.ask_depth_near,
                        depth_ratio,
                        depth_imbalance,
                        depth_far_ratio,
                        depth_acceleration,
                        obi_10: lob_state.obi_10,
                        pm_up_bid: up_bid,
                        pm_up_ask: up_ask,
                        pm_up_bid_size: up_bid_sz,
                        pm_up_ask_size: up_ask_sz,
                        pm_down_bid: down_bid,
                        pm_down_ask: down_ask,
                        pm_down_bid_size: down_bid_sz,
                        pm_down_ask_size: down_ask_sz,
                        pm_lag_secs: up_lag,
                        settlement_up: if resolved_up_won { 1.0 } else { 0.0 },
                        future_up_ask_change_30s: None,
                        future_up_ask_change_60s: None,
                        cum_obi_delta_5m: lob_flow
                            .get(&sym)
                            .map(|f| f.cum_obi_delta())
                            .unwrap_or(0.0),
                        cum_depth_delta_5m: lob_flow
                            .get(&sym)
                            .map(|f| f.cum_depth_delta())
                            .unwrap_or(0.0),
                        cum_mprice_drift_5m: lob_flow
                            .get(&sym)
                            .map(|f| f.cum_mprice_drift())
                            .unwrap_or(0.0),
                        cum_trade_imbalance_5m: trade_flow
                            .get(&sym)
                            .map(|f| f.cum_imbalance())
                            .unwrap_or(0.0),
                    });
                }
            }
            MarketUpdate::AggTrade {
                symbol,
                quantity,
                is_buyer_maker,
                ts,
                ..
            } => {
                if let Some(qty) = quantity.to_f64() {
                    // buyer_maker=false → buyer aggressor (bullish); true → seller aggressor
                    let signed_qty = if *is_buyer_maker { -qty } else { qty };
                    trade_flow
                        .entry(symbol.to_string())
                        .or_insert_with(|| TradeFlowAccumulator::new(300.0))
                        .push(*ts, signed_qty);
                }
            }
            _ => {}
        }
    }

    rows.sort_by_key(|row| (row.event_id.clone(), row.tick_ts));
    attach_future_pm_labels(&mut rows, 30, LabelField::Change30s);
    attach_future_pm_labels(&mut rows, 60, LabelField::Change60s);
    rows
}

#[derive(Clone, Copy)]
enum LabelField {
    Change30s,
    Change60s,
}

fn attach_future_pm_labels(rows: &mut [FactorObservation], horizon_secs: i64, field: LabelField) {
    let mut grouped: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, row) in rows.iter().enumerate() {
        grouped.entry(row.event_id.clone()).or_default().push(idx);
    }
    for indexes in grouped.values_mut() {
        indexes.sort_by_key(|idx| rows[*idx].tick_ts);
        for (pos, row_idx) in indexes.iter().enumerate() {
            let target_ts = rows[*row_idx].tick_ts + chrono::Duration::seconds(horizon_secs);
            let mut future_change = None;
            for next_idx in indexes.iter().skip(pos + 1) {
                if rows[*next_idx].tick_ts >= target_ts {
                    if rows[*row_idx].pm_up_ask.is_finite() && rows[*next_idx].pm_up_ask.is_finite()
                    {
                        future_change = Some(rows[*next_idx].pm_up_ask - rows[*row_idx].pm_up_ask);
                    }
                    break;
                }
            }
            match field {
                LabelField::Change30s => rows[*row_idx].future_up_ask_change_30s = future_change,
                LabelField::Change60s => rows[*row_idx].future_up_ask_change_60s = future_change,
            }
        }
    }
}

pub fn build_event_summaries(rows: &[FactorObservation]) -> Vec<EventFactorSummary> {
    let mut grouped: BTreeMap<&str, Vec<&FactorObservation>> = BTreeMap::new();
    for row in rows {
        grouped.entry(&row.event_id).or_default().push(row);
    }

    grouped
        .into_values()
        .filter_map(|rows| {
            let first = rows.first()?;
            Some(EventFactorSummary {
                event_id: first.event_id.clone(),
                symbol: first.symbol.clone(),
                last_tick_ts: rows
                    .iter()
                    .map(|row| row.tick_ts)
                    .max()
                    .unwrap_or(first.tick_ts),
                settlement_up: first.settlement_up,
                signed_distance_to_beat: mean(rows.iter().map(|row| row.signed_distance_to_beat)),
                abs_distance_to_beat: mean(rows.iter().map(|row| row.abs_distance_to_beat)),
                drift_10s: mean(rows.iter().map(|row| row.drift_10s)),
                drift_30s: mean(rows.iter().map(|row| row.drift_30s)),
                flip_age_secs: mean(rows.iter().map(|row| row.flip_age_secs)),
                post_flip_drift: mean(rows.iter().map(|row| row.post_flip_drift)),
                sigma_horizon: mean(rows.iter().map(|row| row.sigma_horizon)),
                fair_prob_up: mean(rows.iter().map(|row| row.fair_prob_up)),
                fair_prob_up_clean: mean(rows.iter().map(|row| row.fair_prob_up_clean)),
                prob_disagreement: mean(rows.iter().map(|row| row.prob_disagreement)),
                implied_sigma_horizon: mean(rows.iter().map(|row| row.implied_sigma_horizon)),
                vol_gap: mean(rows.iter().map(|row| row.vol_gap)),
                distance_over_sigma: mean(rows.iter().map(|row| row.distance_over_sigma)),
                model_prob_up: mean(rows.iter().map(|row| row.model_prob_up)),
                model_edge_up: mean(rows.iter().map(|row| row.model_edge_up)),
                reward_risk_up: mean(rows.iter().map(|row| row.reward_risk_up)),
                reward_risk_down: mean(rows.iter().map(|row| row.reward_risk_down)),
                obi: mean(rows.iter().map(|row| row.obi)),
                spread_bps: mean(rows.iter().map(|row| row.spread_bps)),
                microprice_offset_bps: mean(rows.iter().map(|row| row.microprice_offset_bps)),
                bid_depth_near: mean(rows.iter().map(|row| row.bid_depth_near)),
                ask_depth_near: mean(rows.iter().map(|row| row.ask_depth_near)),
                depth_ratio: mean(rows.iter().map(|row| row.depth_ratio)),
                depth_imbalance: mean(rows.iter().map(|row| row.depth_imbalance)),
                depth_far_ratio: mean(rows.iter().map(|row| row.depth_far_ratio)),
                depth_acceleration: mean(rows.iter().map(|row| row.depth_acceleration)),
                obi_10: mean(rows.iter().map(|row| row.obi_10)),
                pm_up_bid: mean(rows.iter().map(|row| row.pm_up_bid)),
                pm_up_ask: mean(rows.iter().map(|row| row.pm_up_ask)),
                pm_up_bid_size: mean(rows.iter().map(|row| row.pm_up_bid_size)),
                pm_up_ask_size: mean(rows.iter().map(|row| row.pm_up_ask_size)),
                pm_down_bid: mean(rows.iter().map(|row| row.pm_down_bid)),
                pm_down_ask: mean(rows.iter().map(|row| row.pm_down_ask)),
                pm_down_bid_size: mean(rows.iter().map(|row| row.pm_down_bid_size)),
                pm_down_ask_size: mean(rows.iter().map(|row| row.pm_down_ask_size)),
                pm_lag_secs: mean(rows.iter().map(|row| row.pm_lag_secs)),
                cum_obi_delta_5m: mean(rows.iter().map(|row| row.cum_obi_delta_5m)),
                cum_depth_delta_5m: mean(rows.iter().map(|row| row.cum_depth_delta_5m)),
                cum_mprice_drift_5m: mean(rows.iter().map(|row| row.cum_mprice_drift_5m)),
                cum_trade_imbalance_5m: mean(rows.iter().map(|row| row.cum_trade_imbalance_5m)),
            })
        })
        .collect()
}

pub fn build_task_grain_derived_artifacts_for_event_ids<I, S>(
    rows: &[FactorObservation],
    event_ids: I,
) -> TaskGrainDerivedArtifacts
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let selected_event_ids: std::collections::BTreeSet<String> = event_ids
        .into_iter()
        .map(|event_id| event_id.as_ref().to_string())
        .collect();

    let mut observation_rows: Vec<FactorObservation> = rows
        .iter()
        .filter(|row| selected_event_ids.contains(&row.event_id))
        .cloned()
        .collect();
    observation_rows.sort_by(|lhs, rhs| {
        lhs.event_id
            .cmp(&rhs.event_id)
            .then(lhs.tick_ts.cmp(&rhs.tick_ts))
            .then(lhs.symbol.cmp(&rhs.symbol))
    });

    let mut event_summaries = build_event_summaries(&observation_rows);
    event_summaries.sort_by(|lhs, rhs| {
        lhs.event_id
            .cmp(&rhs.event_id)
            .then(lhs.last_tick_ts.cmp(&rhs.last_tick_ts))
            .then(lhs.symbol.cmp(&rhs.symbol))
    });

    TaskGrainDerivedArtifacts {
        event_ids: selected_event_ids.into_iter().collect(),
        observation_rows,
        event_summaries,
    }
}
pub fn factor_metrics(
    rows: &[FactorObservation],
    event_rows: &[EventFactorSummary],
) -> Vec<FactorMetric> {
    let mut metrics = Vec::new();

    for (factor, accessor) in row_factor_accessors() {
        let (xs, ys): (Vec<f64>, Vec<f64>) = rows
            .iter()
            .filter_map(|row| {
                let x = accessor(row);
                let y = row.future_up_ask_change_30s?;
                if x.is_finite() && y.is_finite() {
                    Some((x, y))
                } else {
                    None
                }
            })
            .unzip();
        let bucketed: Vec<(i64, f64, f64)> = rows
            .iter()
            .filter_map(|row| {
                let x = accessor(row);
                let y = row.future_up_ask_change_30s?;
                if x.is_finite() && y.is_finite() {
                    Some((row.tick_ts.timestamp() / 300, x, y))
                } else {
                    None
                }
            })
            .collect();
        if xs.len() >= 5 {
            metrics.push(FactorMetric {
                label: "future_up_ask_change_30s".to_string(),
                factor: factor.to_string(),
                n: xs.len(),
                pearson_ic: pearson_ic(&xs, &ys),
                spearman_ic: spearman_ic(&xs, &ys),
                icir: bucket_icir(&bucketed, 20),
            });
        }
    }

    for (factor, accessor) in event_factor_accessors() {
        let (xs, ys): (Vec<f64>, Vec<f64>) = event_rows
            .iter()
            .filter_map(|row| {
                let x = accessor(row);
                let y = row.settlement_up;
                if x.is_finite() && y.is_finite() {
                    Some((x, y))
                } else {
                    None
                }
            })
            .unzip();
        let bucketed: Vec<(i64, f64, f64)> = event_rows
            .iter()
            .filter_map(|row| {
                let x = accessor(row);
                let y = row.settlement_up;
                if x.is_finite() && y.is_finite() {
                    Some((row.last_tick_ts.timestamp() / 3600, x, y))
                } else {
                    None
                }
            })
            .collect();
        if xs.len() >= 5 {
            metrics.push(FactorMetric {
                label: "settlement_up".to_string(),
                factor: factor.to_string(),
                n: xs.len(),
                pearson_ic: pearson_ic(&xs, &ys),
                spearman_ic: spearman_ic(&xs, &ys),
                icir: bucket_icir(&bucketed, 5),
            });
        }
    }

    metrics
}

pub fn aggregate_factor_metrics(windows: &[Vec<FactorMetric>]) -> Vec<AggregatedFactorMetric> {
    let mut grouped: BTreeMap<(String, String), Vec<&FactorMetric>> = BTreeMap::new();
    for window in windows {
        for metric in window {
            grouped
                .entry((metric.label.clone(), metric.factor.clone()))
                .or_default()
                .push(metric);
        }
    }

    grouped
        .into_iter()
        .map(|((label, factor), metrics)| {
            let valid_metrics: Vec<&FactorMetric> = metrics
                .iter()
                .copied()
                .filter(|metric| metric.pearson_ic.is_finite() || metric.spearman_ic.is_finite())
                .collect();
            let windows = valid_metrics.len();
            let mean_n = if windows == 0 {
                f64::NAN
            } else {
                valid_metrics.iter().map(|m| m.n as f64).sum::<f64>() / windows as f64
            };
            let mean_pearson_ic = mean_finite(valid_metrics.iter().map(|m| m.pearson_ic));
            let mean_spearman_ic = mean_finite(valid_metrics.iter().map(|m| m.spearman_ic));
            let icir = {
                let vals: Vec<f64> = valid_metrics
                    .iter()
                    .map(|m| m.spearman_ic)
                    .filter(|v| v.is_finite())
                    .collect();
                if vals.len() < 2 {
                    None
                } else {
                    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
                    let std = (vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>()
                        / vals.len() as f64)
                        .sqrt();
                    if std <= 1e-9 { None } else { Some(mean / std) }
                }
            };
            AggregatedFactorMetric {
                label,
                factor,
                windows,
                mean_n,
                mean_pearson_ic,
                mean_spearman_ic,
                icir,
            }
        })
        .collect()
}

#[cfg(feature = "polars-export")]
pub fn observations_to_frame(rows: &[FactorObservation]) -> PolarsResult<DataFrame> {
    df![
        "event_id" => rows.iter().map(|row| row.event_id.as_str()).collect::<Vec<_>>(),
        "symbol" => rows.iter().map(|row| row.symbol.as_str()).collect::<Vec<_>>(),
        "tick_ts" => rows.iter().map(|row| row.tick_ts.timestamp_millis()).collect::<Vec<_>>(),
        "time_remaining_secs" => rows.iter().map(|row| row.time_remaining_secs).collect::<Vec<_>>(),
        "signed_distance_to_beat" => rows.iter().map(|row| row.signed_distance_to_beat).collect::<Vec<_>>(),
        "abs_distance_to_beat" => rows.iter().map(|row| row.abs_distance_to_beat).collect::<Vec<_>>(),
        "drift_10s" => rows.iter().map(|row| row.drift_10s).collect::<Vec<_>>(),
        "drift_30s" => rows.iter().map(|row| row.drift_30s).collect::<Vec<_>>(),
        "flip_age_secs" => rows.iter().map(|row| row.flip_age_secs).collect::<Vec<_>>(),
        "post_flip_drift" => rows.iter().map(|row| row.post_flip_drift).collect::<Vec<_>>(),
        "sigma_horizon" => rows.iter().map(|row| row.sigma_horizon).collect::<Vec<_>>(),
        "fair_prob_up" => rows.iter().map(|row| row.fair_prob_up).collect::<Vec<_>>(),
        "fair_prob_up_clean" => rows.iter().map(|row| row.fair_prob_up_clean).collect::<Vec<_>>(),
        "prob_disagreement" => rows.iter().map(|row| row.prob_disagreement).collect::<Vec<_>>(),
        "implied_sigma_horizon" => rows.iter().map(|row| row.implied_sigma_horizon).collect::<Vec<_>>(),
        "vol_gap" => rows.iter().map(|row| row.vol_gap).collect::<Vec<_>>(),
        "distance_over_sigma" => rows.iter().map(|row| row.distance_over_sigma).collect::<Vec<_>>(),
        "model_prob_up" => rows.iter().map(|row| row.model_prob_up).collect::<Vec<_>>(),
        "model_edge_up" => rows.iter().map(|row| row.model_edge_up).collect::<Vec<_>>(),
        "reward_risk_up" => rows.iter().map(|row| row.reward_risk_up).collect::<Vec<_>>(),
        "reward_risk_down" => rows.iter().map(|row| row.reward_risk_down).collect::<Vec<_>>(),
        "obi" => rows.iter().map(|row| row.obi).collect::<Vec<_>>(),
        "spread_bps" => rows.iter().map(|row| row.spread_bps).collect::<Vec<_>>(),
        "microprice_offset_bps" => rows.iter().map(|row| row.microprice_offset_bps).collect::<Vec<_>>(),
        "bid_depth_near" => rows.iter().map(|row| row.bid_depth_near).collect::<Vec<_>>(),
        "ask_depth_near" => rows.iter().map(|row| row.ask_depth_near).collect::<Vec<_>>(),
        "depth_ratio" => rows.iter().map(|row| row.depth_ratio).collect::<Vec<_>>(),
        "depth_imbalance" => rows.iter().map(|row| row.depth_imbalance).collect::<Vec<_>>(),
        "depth_far_ratio" => rows.iter().map(|row| row.depth_far_ratio).collect::<Vec<_>>(),
        "depth_acceleration" => rows.iter().map(|row| row.depth_acceleration).collect::<Vec<_>>(),
        "obi_10" => rows.iter().map(|row| row.obi_10).collect::<Vec<_>>(),
        "pm_up_bid" => rows.iter().map(|row| row.pm_up_bid).collect::<Vec<_>>(),
        "pm_up_ask" => rows.iter().map(|row| row.pm_up_ask).collect::<Vec<_>>(),
        "pm_up_bid_size" => rows.iter().map(|row| row.pm_up_bid_size).collect::<Vec<_>>(),
        "pm_up_ask_size" => rows.iter().map(|row| row.pm_up_ask_size).collect::<Vec<_>>(),
        "pm_down_bid" => rows.iter().map(|row| row.pm_down_bid).collect::<Vec<_>>(),
        "pm_down_ask" => rows.iter().map(|row| row.pm_down_ask).collect::<Vec<_>>(),
        "pm_down_bid_size" => rows.iter().map(|row| row.pm_down_bid_size).collect::<Vec<_>>(),
        "pm_down_ask_size" => rows.iter().map(|row| row.pm_down_ask_size).collect::<Vec<_>>(),
        "pm_lag_secs" => rows.iter().map(|row| row.pm_lag_secs).collect::<Vec<_>>(),
        "settlement_up" => rows.iter().map(|row| row.settlement_up).collect::<Vec<_>>(),
        "future_up_ask_change_30s" => rows.iter().map(|row| row.future_up_ask_change_30s.unwrap_or(f64::NAN)).collect::<Vec<_>>(),
        "future_up_ask_change_60s" => rows.iter().map(|row| row.future_up_ask_change_60s.unwrap_or(f64::NAN)).collect::<Vec<_>>(),
    ]
}

fn row_factor_accessors() -> Vec<(&'static str, fn(&FactorObservation) -> f64)> {
    vec![
        ("signed_distance_to_beat", |row| row.signed_distance_to_beat),
        ("abs_distance_to_beat", |row| row.abs_distance_to_beat),
        ("drift_10s", |row| row.drift_10s),
        ("drift_30s", |row| row.drift_30s),
        ("flip_age_secs", |row| row.flip_age_secs),
        ("post_flip_drift", |row| row.post_flip_drift),
        ("sigma_horizon", |row| row.sigma_horizon),
        ("fair_prob_up", |row| row.fair_prob_up),
        ("fair_prob_up_clean", |row| row.fair_prob_up_clean),
        ("prob_disagreement", |row| row.prob_disagreement),
        ("implied_sigma_horizon", |row| row.implied_sigma_horizon),
        ("vol_gap", |row| row.vol_gap),
        ("distance_over_sigma", |row| row.distance_over_sigma),
        ("model_prob_up", |row| row.model_prob_up),
        ("model_edge_up", |row| row.model_edge_up),
        ("reward_risk_up", |row| row.reward_risk_up),
        ("reward_risk_down", |row| row.reward_risk_down),
        ("obi", |row| row.obi),
        ("spread_bps", |row| row.spread_bps),
        ("microprice_offset_bps", |row| row.microprice_offset_bps),
        ("depth_ratio", |row| row.depth_ratio),
        ("depth_imbalance", |row| row.depth_imbalance),
        ("depth_far_ratio", |row| row.depth_far_ratio),
        ("depth_acceleration", |row| row.depth_acceleration),
        ("obi_10", |row| row.obi_10),
        ("pm_up_bid", |row| row.pm_up_bid),
        ("pm_up_ask", |row| row.pm_up_ask),
        ("pm_up_bid_size", |row| row.pm_up_bid_size),
        ("pm_up_ask_size", |row| row.pm_up_ask_size),
        ("pm_down_bid", |row| row.pm_down_bid),
        ("pm_down_ask", |row| row.pm_down_ask),
        ("pm_down_bid_size", |row| row.pm_down_bid_size),
        ("pm_down_ask_size", |row| row.pm_down_ask_size),
        ("pm_lag_secs", |row| row.pm_lag_secs),
        ("cum_obi_delta_5m", |row| row.cum_obi_delta_5m),
        ("cum_depth_delta_5m", |row| row.cum_depth_delta_5m),
        ("cum_mprice_drift_5m", |row| row.cum_mprice_drift_5m),
        ("cum_trade_imbalance_5m", |row| row.cum_trade_imbalance_5m),
    ]
}

fn event_factor_accessors() -> Vec<(&'static str, fn(&EventFactorSummary) -> f64)> {
    vec![
        ("signed_distance_to_beat", |row| row.signed_distance_to_beat),
        ("abs_distance_to_beat", |row| row.abs_distance_to_beat),
        ("drift_10s", |row| row.drift_10s),
        ("drift_30s", |row| row.drift_30s),
        ("flip_age_secs", |row| row.flip_age_secs),
        ("post_flip_drift", |row| row.post_flip_drift),
        ("sigma_horizon", |row| row.sigma_horizon),
        ("fair_prob_up", |row| row.fair_prob_up),
        ("fair_prob_up_clean", |row| row.fair_prob_up_clean),
        ("prob_disagreement", |row| row.prob_disagreement),
        ("implied_sigma_horizon", |row| row.implied_sigma_horizon),
        ("vol_gap", |row| row.vol_gap),
        ("distance_over_sigma", |row| row.distance_over_sigma),
        ("model_prob_up", |row| row.model_prob_up),
        ("model_edge_up", |row| row.model_edge_up),
        ("reward_risk_up", |row| row.reward_risk_up),
        ("reward_risk_down", |row| row.reward_risk_down),
        ("obi", |row| row.obi),
        ("spread_bps", |row| row.spread_bps),
        ("microprice_offset_bps", |row| row.microprice_offset_bps),
        ("depth_ratio", |row| row.depth_ratio),
        ("depth_imbalance", |row| row.depth_imbalance),
        ("depth_far_ratio", |row| row.depth_far_ratio),
        ("depth_acceleration", |row| row.depth_acceleration),
        ("obi_10", |row| row.obi_10),
        ("pm_up_bid", |row| row.pm_up_bid),
        ("pm_up_ask", |row| row.pm_up_ask),
        ("pm_up_bid_size", |row| row.pm_up_bid_size),
        ("pm_up_ask_size", |row| row.pm_up_ask_size),
        ("pm_down_bid", |row| row.pm_down_bid),
        ("pm_down_ask", |row| row.pm_down_ask),
        ("pm_down_bid_size", |row| row.pm_down_bid_size),
        ("pm_down_ask_size", |row| row.pm_down_ask_size),
        ("pm_lag_secs", |row| row.pm_lag_secs),
        ("cum_obi_delta_5m", |row| row.cum_obi_delta_5m),
        ("cum_depth_delta_5m", |row| row.cum_depth_delta_5m),
        ("cum_mprice_drift_5m", |row| row.cum_mprice_drift_5m),
        ("cum_trade_imbalance_5m", |row| row.cum_trade_imbalance_5m),
    ]
}
fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let vals: Vec<f64> = values.filter(|value| value.is_finite()).collect();
    if vals.is_empty() {
        f64::NAN
    } else {
        vals.iter().sum::<f64>() / vals.len() as f64
    }
}

fn mean_finite(values: impl Iterator<Item = f64>) -> f64 {
    let vals: Vec<f64> = values.filter(|value| value.is_finite()).collect();
    if vals.is_empty() {
        f64::NAN
    } else {
        vals.iter().sum::<f64>() / vals.len() as f64
    }
}

fn estimate_probability(s0: f64, st: f64, sigma_horizon: f64) -> f64 {
    if sigma_horizon <= 0.0 {
        return if st >= s0 { 1.0 } else { 0.0 };
    }
    if s0 <= 0.0 || st <= 0.0 {
        return 0.5;
    }
    let z = (st / s0).ln() / sigma_horizon;
    normal_cdf(z)
}

fn quote_mid(bid: f64, ask: f64) -> f64 {
    if bid.is_finite() && ask.is_finite() && bid > 0.0 && ask > 0.0 && bid <= ask {
        0.5 * (bid + ask)
    } else if ask.is_finite() && ask > 0.0 {
        ask
    } else if bid.is_finite() && bid > 0.0 {
        bid
    } else {
        f64::NAN
    }
}

fn fair_market_prob_up(up_bid: f64, up_ask: f64, down_bid: f64, down_ask: f64) -> f64 {
    let up_mid = quote_mid(up_bid, up_ask);
    let down_mid = quote_mid(down_bid, down_ask);
    if !up_mid.is_finite() || !down_mid.is_finite() || up_mid <= 0.0 || down_mid <= 0.0 {
        return f64::NAN;
    }
    let total = up_mid + down_mid;
    if total <= 0.0 {
        return f64::NAN;
    }
    (up_mid / total).clamp(1e-4, 1.0 - 1e-4)
}

fn clean_market_prob_up(
    up_bid: f64,
    up_ask: f64,
    down_bid: f64,
    down_ask: f64,
    up_break_even_prob: f64,
    down_break_even_prob: f64,
) -> f64 {
    if !up_break_even_prob.is_finite() || !down_break_even_prob.is_finite() {
        return f64::NAN;
    }
    let down_implied_up = 1.0 - down_break_even_prob;
    let ask_clean = 0.5 * (up_break_even_prob + down_implied_up);
    let mid_fair = fair_market_prob_up(up_bid, up_ask, down_bid, down_ask);
    if mid_fair.is_finite() {
        (0.5 * ask_clean + 0.5 * mid_fair).clamp(1e-4, 1.0 - 1e-4)
    } else {
        ask_clean.clamp(1e-4, 1.0 - 1e-4)
    }
}

fn implied_prob_disagreement(up_break_even_prob: f64, down_break_even_prob: f64) -> f64 {
    if !up_break_even_prob.is_finite() || !down_break_even_prob.is_finite() {
        return f64::NAN;
    }
    up_break_even_prob - (1.0 - down_break_even_prob)
}

fn implied_sigma_horizon(s0: f64, st: f64, fair_prob_up: f64) -> f64 {
    if !s0.is_finite() || !st.is_finite() || s0 <= 0.0 || st <= 0.0 || !fair_prob_up.is_finite() {
        return f64::NAN;
    }
    let log_ratio = (st / s0).ln().abs();
    if log_ratio <= 1e-12 {
        return 0.0;
    }
    let z = inv_normal_cdf(fair_prob_up);
    if !z.is_finite() || z.abs() <= 1e-9 {
        return f64::NAN;
    }
    log_ratio / z.abs()
}

fn normal_cdf(x: f64) -> f64 {
    let a1 = 0.254829592_f64;
    let a2 = -0.284496736_f64;
    let a3 = 1.421413741_f64;
    let a4 = -1.453152027_f64;
    let a5 = 1.061405429_f64;
    let p = 0.3275911_f64;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + p * z);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-z * z).exp();

    0.5 * (1.0 + sign * y)
}

fn inv_normal_cdf(p: f64) -> f64 {
    if !p.is_finite() {
        return f64::NAN;
    }
    let p = p.clamp(1e-12, 1.0 - 1e-12);

    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];

    const P_LOW: f64 = 0.02425;
    const P_HIGH: f64 = 1.0 - P_LOW;

    if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        return (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0);
    }
    if p > P_HIGH {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        return -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0);
    }

    let q = p - 0.5;
    let r = q * q;
    (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
        / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
}

fn crypto_fee_cost(entry_price: f64) -> f64 {
    0.02 * entry_price * (1.0 - entry_price)
}

fn reward_risk_ratio(entry_price: f64) -> f64 {
    if !entry_price.is_finite() || entry_price <= 0.0 || entry_price >= 1.0 {
        return f64::NAN;
    }
    let fee = crypto_fee_cost(entry_price);
    let reward = 1.0 - entry_price - fee;
    let risk = entry_price + fee;
    if risk <= 0.0 {
        return f64::NAN;
    }
    reward / risk
}

fn signum(value: f64) -> f64 {
    if value > 1e-7 {
        1.0
    } else if value < -1e-7 {
        -1.0
    } else {
        0.0
    }
}

pub fn pearson_ic(xs: &[f64], ys: &[f64]) -> f64 {
    if xs.len() != ys.len() || xs.len() < 2 {
        return f64::NAN;
    }
    let mean_x = xs.iter().sum::<f64>() / xs.len() as f64;
    let mean_y = ys.iter().sum::<f64>() / ys.len() as f64;
    let mut cov = 0.0;
    let mut var_x = 0.0;
    let mut var_y = 0.0;
    for (x, y) in xs.iter().zip(ys.iter()) {
        cov += (x - mean_x) * (y - mean_y);
        var_x += (x - mean_x).powi(2);
        var_y += (y - mean_y).powi(2);
    }
    if var_x <= 0.0 || var_y <= 0.0 {
        return f64::NAN;
    }
    cov / (var_x.sqrt() * var_y.sqrt())
}

pub fn spearman_ic(xs: &[f64], ys: &[f64]) -> f64 {
    pearson_ic(&rank(xs), &rank(ys))
}

fn rank(values: &[f64]) -> Vec<f64> {
    let mut indexed: Vec<(usize, f64)> = values.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut ranks = vec![0.0; values.len()];
    let mut pos = 0;
    while pos < indexed.len() {
        let start = pos;
        let value = indexed[pos].1;
        while pos < indexed.len() && indexed[pos].1 == value {
            pos += 1;
        }
        let avg_rank = (start + pos - 1) as f64 / 2.0;
        for (idx, _) in &indexed[start..pos] {
            ranks[*idx] = avg_rank;
        }
    }
    ranks
}

fn update_sort_ts(update: &MarketUpdate) -> DateTime<Utc> {
    match update {
        MarketUpdate::SpotPrice { ts, .. }
        | MarketUpdate::AggTrade { ts, .. }
        | MarketUpdate::Quote { ts, .. }
        | MarketUpdate::L2 { ts, .. }
        | MarketUpdate::L2Depth { ts, .. }
        | MarketUpdate::SportsState { ts, .. }
        | MarketUpdate::SportsPregame { ts, .. }
        | MarketUpdate::SportsLive { ts, .. }
        | MarketUpdate::ReferencePrice { ts, .. }
        | MarketUpdate::Kline { ts, .. } => *ts,
        MarketUpdate::EventDiscovered {
            end_time,
            window_secs,
            ..
        } => {
            *end_time - chrono::Duration::seconds(*window_secs as i64) - chrono::Duration::hours(1)
        }
        MarketUpdate::EventExpired { end_time, .. } => *end_time,
    }
}

pub(crate) fn bucket_icir(bucketed: &[(i64, f64, f64)], min_points: usize) -> Option<f64> {
    let mut grouped: BTreeMap<i64, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    for (bucket, x, y) in bucketed {
        let entry = grouped.entry(*bucket).or_default();
        entry.0.push(*x);
        entry.1.push(*y);
    }

    let ics: Vec<f64> = grouped
        .into_values()
        .filter_map(|(xs, ys)| {
            if xs.len() < min_points {
                return None;
            }
            let ic = spearman_ic(&xs, &ys);
            if ic.is_finite() { Some(ic) } else { None }
        })
        .collect();

    if ics.len() < 2 {
        return None;
    }
    let mean_ic = ics.iter().sum::<f64>() / ics.len() as f64;
    let std_ic =
        (ics.iter().map(|ic| (ic - mean_ic).powi(2)).sum::<f64>() / ics.len() as f64).sqrt();
    if std_ic <= 1e-9 {
        None
    } else {
        Some(mean_ic / std_ic)
    }
}

/// Export a slice of `FactorObservation` to a Parquet file at `path`.
///
/// Creates or overwrites the file. Returns an error if the DataFrame cannot
/// be built or the file cannot be written.
#[cfg(feature = "polars-export")]
pub fn export_observations_parquet(
    rows: &[FactorObservation],
    path: &std::path::Path,
) -> polars::prelude::PolarsResult<()> {
    use polars::io::parquet::write::ParquetWriter;
    use std::fs::File;

    let mut df = observations_to_frame(rows)?;
    let file = File::create(path).map_err(|e| polars::prelude::PolarsError::IO {
        error: std::sync::Arc::new(e),
        msg: None,
    })?;
    ParquetWriter::new(file).finish(&mut df)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        FactorObservation, LabelField, attach_future_pm_labels, build_factor_observations_with_lob,
        build_task_grain_derived_artifacts_for_event_ids, pearson_ic, spearman_ic,
    };
    use chrono::{TimeZone, Utc};
    use ploy_market_contracts::MarketUpdate;
    use rust_decimal::Decimal;
    #[cfg(feature = "db")]
    use serde_json::json;
    use std::sync::Arc;

    fn test_factor_observation(
        event_id: &str,
        symbol: &str,
        tick_ts_secs: i64,
        settlement_up: f64,
        future_up_ask_change_30s: Option<f64>,
    ) -> FactorObservation {
        FactorObservation {
            event_id: event_id.into(),
            symbol: symbol.into(),
            tick_ts: chrono::DateTime::from_timestamp(tick_ts_secs, 0)
                .unwrap()
                .with_timezone(&Utc),
            time_remaining_secs: 60,
            signed_distance_to_beat: 0.0,
            abs_distance_to_beat: 0.0,
            drift_10s: 0.0,
            drift_30s: 0.0,
            flip_age_secs: 0.0,
            post_flip_drift: 0.0,
            sigma_horizon: 1.0,
            fair_prob_up: 0.5,
            fair_prob_up_clean: 0.5,
            prob_disagreement: 0.0,
            implied_sigma_horizon: 0.2,
            vol_gap: 0.0,
            distance_over_sigma: 0.0,
            model_prob_up: 0.5,
            model_edge_up: 0.0,
            reward_risk_up: 1.0,
            reward_risk_down: 1.0,
            obi: 0.0,
            spread_bps: 0.0,
            microprice_offset_bps: 0.0,
            bid_depth_near: 1.0,
            ask_depth_near: 1.0,
            depth_ratio: 1.0,
            depth_imbalance: 0.0,
            depth_far_ratio: 1.0,
            depth_acceleration: 0.0,
            obi_10: 0.0,
            pm_up_bid: 0.49,
            pm_up_ask: 0.50,
            pm_up_bid_size: 1.0,
            pm_up_ask_size: 1.0,
            pm_down_bid: 0.49,
            pm_down_ask: 0.50,
            pm_down_bid_size: 1.0,
            pm_down_ask_size: 1.0,
            pm_lag_secs: 0.0,
            settlement_up,
            future_up_ask_change_30s,
            future_up_ask_change_60s: None,
            cum_obi_delta_5m: 0.0,
            cum_depth_delta_5m: 0.0,
            cum_mprice_drift_5m: 0.0,
            cum_trade_imbalance_5m: 0.0,
        }
    }

    #[test]
    fn factor_observations_only_include_active_event_window() {
        let start = Utc.timestamp_opt(700, 0).unwrap();
        let end = Utc.timestamp_opt(1000, 0).unwrap();
        let updates = vec![
            MarketUpdate::EventDiscovered {
                event_id: Arc::from("evt"),
                symbol: Arc::from("BTCUSDT"),
                up_token: Arc::from("up"),
                down_token: Arc::from("down"),
                end_time: end,
                window_secs: 300,
                price_to_beat: Some(Decimal::new(100, 0)),
                resolved_up_won: None,
            },
            MarketUpdate::Quote {
                token_id: Arc::from("up"),
                bid: Some(Decimal::new(45, 2)),
                ask: Some(Decimal::new(46, 2)),
                bid_size: Some(Decimal::new(100, 0)),
                ask_size: Some(Decimal::new(100, 0)),
                ts: Utc.timestamp_opt(649, 0).unwrap(),
            },
            MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: Decimal::new(100, 0),
                ts: Utc.timestamp_opt(650, 0).unwrap(),
            },
            MarketUpdate::Quote {
                token_id: Arc::from("up"),
                bid: Some(Decimal::new(45, 2)),
                ask: Some(Decimal::new(46, 2)),
                bid_size: Some(Decimal::new(100, 0)),
                ask_size: Some(Decimal::new(100, 0)),
                ts: Utc.timestamp_opt(709, 0).unwrap(),
            },
            MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: Decimal::new(100, 0),
                ts: Utc.timestamp_opt(710, 0).unwrap(),
            },
            MarketUpdate::EventExpired {
                event_id: Arc::from("evt"),
                end_time: end,
                resolved_up_won: Some(true),
            },
        ];

        let rows = build_factor_observations_with_lob(&updates, &[], 30);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tick_ts, Utc.timestamp_opt(710, 0).unwrap());
        assert!(rows[0].tick_ts >= start);
    }

    #[test]
    fn pearson_ic_detects_positive_relationship() {
        let xs = vec![1.0, 2.0, 3.0, 4.0];
        let ys = vec![2.0, 4.0, 6.0, 8.0];
        assert!(pearson_ic(&xs, &ys) > 0.99);
    }

    #[test]
    fn spearman_ic_detects_negative_monotonicity() {
        let xs = vec![1.0, 2.0, 3.0, 4.0];
        let ys = vec![4.0, 3.0, 2.0, 1.0];
        assert!(spearman_ic(&xs, &ys) < -0.99);
    }

    #[test]
    fn spearman_ic_uses_average_ranks_for_ties() {
        let xs = vec![1.0, 1.0, 2.0, 2.0];
        let ys = vec![10.0, 20.0, 10.0, 20.0];
        let ic = spearman_ic(&xs, &ys);
        assert!(
            ic.abs() < 1e-9,
            "expected tie-correct spearman near 0, got {ic}"
        );
    }

    #[test]
    fn future_label_attachment_is_order_invariant_after_sort() {
        let ts0 = chrono::DateTime::from_timestamp(0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let ts1 = chrono::DateTime::from_timestamp(40, 0)
            .unwrap()
            .with_timezone(&Utc);
        let ts2 = chrono::DateTime::from_timestamp(80, 0)
            .unwrap()
            .with_timezone(&Utc);

        let make_rows = || {
            vec![
                FactorObservation {
                    event_id: "evt".into(),
                    symbol: "BTCUSDT".into(),
                    tick_ts: ts2,
                    time_remaining_secs: 10,
                    signed_distance_to_beat: 0.0,
                    abs_distance_to_beat: 0.0,
                    drift_10s: 0.0,
                    drift_30s: 0.0,
                    flip_age_secs: 0.0,
                    post_flip_drift: 0.0,
                    sigma_horizon: 1.0,
                    fair_prob_up: 0.4,
                    fair_prob_up_clean: 0.39,
                    prob_disagreement: 0.02,
                    implied_sigma_horizon: 0.2,
                    vol_gap: -0.8,
                    distance_over_sigma: 0.0,
                    model_prob_up: 0.5,
                    model_edge_up: 0.0,
                    reward_risk_up: 1.0,
                    reward_risk_down: 0.5,
                    obi: 0.0,
                    spread_bps: 0.0,
                    microprice_offset_bps: f64::NAN,
                    bid_depth_near: 0.0,
                    ask_depth_near: 0.0,
                    depth_ratio: 0.0,
                    depth_imbalance: 0.0,
                    depth_far_ratio: 0.0,
                    depth_acceleration: 0.0,
                    obi_10: 0.0,
                    pm_up_bid: 0.39,
                    pm_up_ask: 0.40,
                    pm_up_bid_size: f64::NAN,
                    pm_up_ask_size: f64::NAN,
                    pm_down_bid: 0.59,
                    pm_down_ask: 0.60,
                    pm_down_bid_size: f64::NAN,
                    pm_down_ask_size: f64::NAN,
                    pm_lag_secs: 0.0,
                    settlement_up: 1.0,
                    future_up_ask_change_30s: None,
                    future_up_ask_change_60s: None,
                    cum_obi_delta_5m: 0.0,
                    cum_depth_delta_5m: 0.0,
                    cum_mprice_drift_5m: 0.0,
                    cum_trade_imbalance_5m: 0.0,
                },
                FactorObservation {
                    event_id: "evt".into(),
                    symbol: "BTCUSDT".into(),
                    tick_ts: ts0,
                    time_remaining_secs: 90,
                    signed_distance_to_beat: 0.0,
                    abs_distance_to_beat: 0.0,
                    drift_10s: 0.0,
                    drift_30s: 0.0,
                    flip_age_secs: 0.0,
                    post_flip_drift: 0.0,
                    sigma_horizon: 1.0,
                    fair_prob_up: 0.1,
                    fair_prob_up_clean: 0.12,
                    prob_disagreement: -0.01,
                    implied_sigma_horizon: 0.3,
                    vol_gap: -0.7,
                    distance_over_sigma: 0.0,
                    model_prob_up: 0.5,
                    model_edge_up: 0.0,
                    reward_risk_up: 4.0,
                    reward_risk_down: 0.1,
                    obi: 0.0,
                    spread_bps: 0.0,
                    microprice_offset_bps: f64::NAN,
                    bid_depth_near: 0.0,
                    ask_depth_near: 0.0,
                    depth_ratio: 0.0,
                    depth_imbalance: 0.0,
                    depth_far_ratio: 0.0,
                    depth_acceleration: 0.0,
                    obi_10: 0.0,
                    pm_up_bid: 0.09,
                    pm_up_ask: 0.10,
                    pm_up_bid_size: f64::NAN,
                    pm_up_ask_size: f64::NAN,
                    pm_down_bid: 0.89,
                    pm_down_ask: 0.90,
                    pm_down_bid_size: f64::NAN,
                    pm_down_ask_size: f64::NAN,
                    pm_lag_secs: 0.0,
                    settlement_up: 1.0,
                    future_up_ask_change_30s: None,
                    future_up_ask_change_60s: None,
                    cum_obi_delta_5m: 0.0,
                    cum_depth_delta_5m: 0.0,
                    cum_mprice_drift_5m: 0.0,
                    cum_trade_imbalance_5m: 0.0,
                },
                FactorObservation {
                    event_id: "evt".into(),
                    symbol: "BTCUSDT".into(),
                    tick_ts: ts1,
                    time_remaining_secs: 50,
                    signed_distance_to_beat: 0.0,
                    abs_distance_to_beat: 0.0,
                    drift_10s: 0.0,
                    drift_30s: 0.0,
                    flip_age_secs: 0.0,
                    post_flip_drift: 0.0,
                    sigma_horizon: 1.0,
                    fair_prob_up: 0.6,
                    fair_prob_up_clean: 0.58,
                    prob_disagreement: 0.03,
                    implied_sigma_horizon: 0.4,
                    vol_gap: -0.6,
                    distance_over_sigma: 0.0,
                    model_prob_up: 0.5,
                    model_edge_up: 0.0,
                    reward_risk_up: 0.6,
                    reward_risk_down: 1.5,
                    obi: 0.0,
                    spread_bps: 0.0,
                    microprice_offset_bps: f64::NAN,
                    bid_depth_near: 0.0,
                    ask_depth_near: 0.0,
                    depth_ratio: 0.0,
                    depth_imbalance: 0.0,
                    depth_far_ratio: 0.0,
                    depth_acceleration: 0.0,
                    obi_10: 0.0,
                    pm_up_bid: 0.24,
                    pm_up_ask: 0.25,
                    pm_up_bid_size: f64::NAN,
                    pm_up_ask_size: f64::NAN,
                    pm_down_bid: 0.74,
                    pm_down_ask: 0.75,
                    pm_down_bid_size: f64::NAN,
                    pm_down_ask_size: f64::NAN,
                    pm_lag_secs: 0.0,
                    settlement_up: 1.0,
                    future_up_ask_change_30s: None,
                    future_up_ask_change_60s: None,
                    cum_obi_delta_5m: 0.0,
                    cum_depth_delta_5m: 0.0,
                    cum_mprice_drift_5m: 0.0,
                    cum_trade_imbalance_5m: 0.0,
                },
            ]
        };

        let mut rows = make_rows();
        rows.sort_by_key(|row| (row.event_id.clone(), row.tick_ts));
        attach_future_pm_labels(&mut rows, 30, LabelField::Change30s);

        let first = rows
            .iter()
            .find(|row| row.tick_ts == ts0)
            .expect("first row");
        assert_eq!(first.future_up_ask_change_30s, Some(0.15));
    }

    #[test]
    fn derived_artifacts_filter_to_selected_events_and_preserve_task_grains() {
        let rows = vec![
            test_factor_observation("evt-b", "ETHUSDT", 20, 0.0, Some(-0.20)),
            test_factor_observation("evt-a", "BTCUSDT", 30, 1.0, None),
            test_factor_observation("evt-c", "SOLUSDT", 15, 1.0, Some(0.30)),
            test_factor_observation("evt-a", "BTCUSDT", 10, 1.0, Some(0.10)),
            test_factor_observation("evt-b", "BTCUSDT", 20, 0.0, None),
        ];

        let artifacts = build_task_grain_derived_artifacts_for_event_ids(&rows, ["evt-b", "evt-a"]);

        assert_eq!(artifacts.event_ids, vec!["evt-a", "evt-b"]);
        assert_eq!(artifacts.observation_row_count(), 4);
        assert_eq!(artifacts.event_summary_count(), 2);
        assert_eq!(artifacts.repricing_label_row_count_30s(), 2);
        assert_eq!(artifacts.settlement_label_event_count(), 2);

        let observation_keys: Vec<(&str, i64, &str)> = artifacts
            .observation_rows
            .iter()
            .map(|row| {
                (
                    row.event_id.as_str(),
                    row.tick_ts.timestamp(),
                    row.symbol.as_str(),
                )
            })
            .collect();
        assert_eq!(
            observation_keys,
            vec![
                ("evt-a", 10, "BTCUSDT"),
                ("evt-a", 30, "BTCUSDT"),
                ("evt-b", 20, "BTCUSDT"),
                ("evt-b", 20, "ETHUSDT"),
            ]
        );

        let summary_keys: Vec<(&str, i64, &str)> = artifacts
            .event_summaries
            .iter()
            .map(|row| {
                (
                    row.event_id.as_str(),
                    row.last_tick_ts.timestamp(),
                    row.symbol.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary_keys,
            vec![("evt-a", 30, "BTCUSDT"), ("evt-b", 20, "BTCUSDT")]
        );
        assert_eq!(
            artifacts
                .event_summaries
                .iter()
                .map(|row| row.settlement_up)
                .collect::<Vec<_>>(),
            vec![1.0, 0.0]
        );
    }

    #[cfg(feature = "db")]
    #[test]
    fn depth_band_supports_near_and_far_ranges() {
        let bids = json!([
            {"price": "100.0", "size": "2.0"},
            {"price": "99.9", "size": "3.0"},
            {"price": "99.5", "size": "4.0"}
        ]);
        let asks = json!([
            {"price": "100.02", "size": "1.0"},
            {"price": "100.08", "size": "2.5"},
            {"price": "100.40", "size": "9.0"}
        ]);

        let near = super::depth_band(&bids, &asks, 100.0, 0.001);
        let far = super::depth_band(&bids, &asks, 100.0, 0.005);

        assert!((near.0 - 5.0).abs() < 1e-9);
        assert!((near.1 - 3.5).abs() < 1e-9);
        assert!((far.0 - 9.0).abs() < 1e-9);
        assert!((far.1 - 12.5).abs() < 1e-9);
    }
}
