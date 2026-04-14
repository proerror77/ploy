use std::collections::{BTreeMap, HashMap, VecDeque};

use chrono::{DateTime, Utc};
use ploy_strategy_bundles::traits::MarketUpdate;
use polars::prelude::*;
use rust_decimal::prelude::ToPrimitive;
use serde_json::Value;
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
    pub distance_over_sigma: f64,
    pub model_prob_up: f64,
    pub model_edge_up: f64,
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
    pub pm_up_ask: f64,
    pub pm_down_ask: f64,
    pub pm_lag_secs: f64,
    pub settlement_up: f64,
    pub future_up_ask_change_30s: Option<f64>,
    pub future_up_ask_change_60s: Option<f64>,
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
    pub distance_over_sigma: f64,
    pub model_prob_up: f64,
    pub model_edge_up: f64,
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
    pub pm_up_ask: f64,
    pub pm_down_ask: f64,
    pub pm_lag_secs: f64,
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
    symbol: String,
    end_time: Option<DateTime<Utc>>,
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
        self.entries.iter().map(|(ret, _, _)| ret * ret).sum::<f64>() / self.total_secs
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
///
/// `binance_lob_ticks` records at ~1 Hz; for factor research 1 tick per 5 s is sufficient
/// and reduces JSONB transfer by ~5x. Pass `sample_every_secs = 1` to disable downsampling.
pub async fn load_research_lob_snapshots(
    pool: &PgPool,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<ResearchLobSnapshot>, sqlx::Error> {
    load_research_lob_snapshots_sampled(pool, symbols, start, end, 5).await
}

/// Loads LOB snapshots from `binance_lob_ticks`, keeping only ticks whose Unix epoch
/// timestamp is divisible by `sample_every_secs`. This reduces data transfer for
/// research runs at the cost of temporal resolution.
///
/// Note: sampling is epoch-modulo based (`epoch % N = 0`), not uniform wall-clock
/// spacing. For odd values of N (e.g. 7), sampled timestamps will not be evenly
/// spaced. For most research use cases (N=5 or N=10), this is not a concern.
///
/// `sample_every_secs` is clamped to a minimum of 1 (no divide-by-zero).
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
        SELECT
            event_time,
            symbol,
            COALESCE(obi_5, 0) AS obi_5,
            COALESCE(obi_10, 0) AS obi_10,
            COALESCE(spread_bps, 0) AS spread_bps,
            COALESCE(best_bid, 0) AS best_bid,
            COALESCE(best_ask, 0) AS best_ask,
            COALESCE(mid_price, 0) AS mid_price,
            bids,
            asks
        FROM binance_lob_ticks
        WHERE symbol = ANY($1)
          AND event_time >= $2
          AND event_time <= $3
          AND EXTRACT(EPOCH FROM event_time)::bigint % $4 = 0
        ORDER BY event_time
        "#,
    )
    .bind(symbols)
    .bind(start)
    .bind(end)
    .bind(sample_every_secs)
    .fetch_all(pool)
    .await?;

    eprintln!("lob snapshot rows: {} (sample_every_secs={})", rows.len(), sample_every_secs);

    Ok(rows
        .into_iter()
        .map(
            |(
                ts,
                symbol,
                obi_5,
                obi_10,
                spread_bps,
                best_bid,
                best_ask,
                mid_price,
                bids,
                asks,
            )| {
                let mid_price = mid_price.to_f64().unwrap_or(f64::NAN);
                let (bid_depth_near, ask_depth_near) =
                    depth_band(&bids, &asks, mid_price, 0.001);
                let (bid_depth_far, ask_depth_far) =
                    depth_band(&bids, &asks, mid_price, 0.005);
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

fn parse_depth_level(level: &Value) -> Option<(f64, f64)> {
    match level {
        Value::Array(items) if items.len() >= 2 => {
            Some((json_f64(&items[0])?, json_f64(&items[1])?))
        }
        Value::Object(map) => Some((json_f64(map.get("price")?)?, json_f64(map.get("size")?)?)),
        _ => None,
    }
}

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
                final_outcomes.insert(event_id.clone(), *outcome);
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
    let mut quotes: HashMap<String, (DateTime<Utc>, f64)> = HashMap::new();
    let mut lob: HashMap<String, LobState> = HashMap::new();
    let mut lob_by_symbol: HashMap<String, Vec<&ResearchLobSnapshot>> = HashMap::new();
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
                price_to_beat,
                resolved_up_won,
                ..
            } => {
                events.insert(
                    event_id.clone(),
                    EventState {
                        event_id: event_id.clone(),
                        symbol: symbol.clone(),
                        end_time: Some(*end_time),
                        price_to_beat: price_to_beat.and_then(|value| value.to_f64()),
                        resolved_up_won: final_outcomes
                            .get(event_id)
                            .copied()
                            .or(*resolved_up_won),
                        up_token: up_token.clone(),
                        down_token: down_token.clone(),
                    },
                );
            }
            MarketUpdate::EventExpired {
                event_id,
                resolved_up_won,
                ..
            } => {
                if let Some(event) = events.get_mut(event_id) {
                    event.resolved_up_won = resolved_up_won.or(event.resolved_up_won);
                }
            }
            MarketUpdate::Quote {
                token_id, ask, ts, ..
            } => {
                if let Some(ask) = ask.and_then(|value| value.to_f64()) {
                    quotes.insert(token_id.clone(), (*ts, ask));
                }
            }
            MarketUpdate::L2 {
                symbol,
                obi,
                spread_bps,
                ..
            } => {
                let state = lob.entry(symbol.clone()).or_default();
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
                    symbol.clone(),
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

                buf_30s
                    .entry(symbol.clone())
                    .or_insert_with(|| DriftBuffer::new(30.0))
                    .push(*ts, spot_price);
                buf_10s
                    .entry(symbol.clone())
                    .or_insert_with(|| DriftBuffer::new(10.0))
                    .push(*ts, spot_price);

                let drift_30s = buf_30s
                    .get(symbol)
                    .map(DriftBuffer::drift_speed)
                    .unwrap_or(0.0);
                let drift_10s = buf_10s
                    .get(symbol)
                    .map(DriftBuffer::drift_speed)
                    .unwrap_or(0.0);

                let dstate = drift_state.entry(symbol.clone()).or_default();
                let old_sign = signum(dstate.prev_drift_30s);
                let new_sign = signum(drift_30s);
                let flipped = old_sign != 0.0 && new_sign != 0.0 && old_sign != new_sign;
                if flipped {
                    dstate.flip_ts = Some(*ts);
                }
                dstate.prev_drift_30s = drift_30s;
                dstate.post_flip_drift = drift_30s.abs();

                if let Some((prev_ts, prev_price)) = spot.get(symbol).copied() {
                    let dt_secs = (*ts - prev_ts).num_milliseconds() as f64 / 1000.0;
                    if dt_secs > 0.0 && prev_price > 0.0 {
                        let log_return = (spot_price / prev_price).ln();
                        let inst_var_per_sec = log_return * log_return / dt_secs.max(1e-6);
                        let floor = 0.001_f64.powi(2) / 900.0;
                        let vstate = vol.entry(symbol.clone()).or_default();
                        vstate.ewma_var_per_sec = if vstate.ewma_var_per_sec <= 0.0 {
                            inst_var_per_sec.max(floor)
                        } else {
                            EWMA_LAMBDA * vstate.ewma_var_per_sec
                                + (1.0 - EWMA_LAMBDA) * inst_var_per_sec
                        };

                        retbuf
                            .entry(symbol.clone())
                            .or_insert_with(ReturnBuffer::new)
                            .push(log_return, dt_secs, spot_price);
                    }
                }
                spot.insert(symbol.clone(), (*ts, spot_price));

                if let Some(snapshots) = lob_by_symbol.get(symbol) {
                    if let Some(snapshot) = snapshots
                        .iter()
                        .rev()
                        .find(|snapshot| snapshot.ts <= *ts)
                    {
                        lob.insert(
                            symbol.clone(),
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

                for event in events.values() {
                    if event.symbol != *symbol {
                        continue;
                    }
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

                    let (up_ask, up_lag) = quotes
                        .get(&event.up_token)
                        .map(|(quote_ts, ask)| (*ask, (*ts - *quote_ts).num_seconds() as f64))
                        .unwrap_or((f64::NAN, f64::NAN));
                    let (down_ask, _) = quotes
                        .get(&event.down_token)
                        .map(|(quote_ts, ask)| (*ask, (*ts - *quote_ts).num_seconds() as f64))
                        .unwrap_or((f64::NAN, f64::NAN));

                    if !up_lag.is_finite() || up_lag < 0.0 || up_lag > max_quote_age_secs as f64 {
                        continue;
                    }
                    if !up_ask.is_finite() {
                        continue;
                    }

                    let lob_state = lob.get(symbol).cloned().unwrap_or_default();
                    let depth_ratio = if lob_state.ask_depth_near > 0.0 {
                        lob_state.bid_depth_near / lob_state.ask_depth_near
                    } else {
                        f64::NAN
                    };
                    let depth_imbalance = if lob_state.bid_depth_near + lob_state.ask_depth_near > 0.0 {
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
                    let depth_acceleration = if depth_inner_ratio.is_finite() && depth_ratio.is_finite() {
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
                    let ewma = vol.get(symbol).map(|state| state.ewma_var_per_sec).unwrap_or(floor);
                    let (rv, parkinson) = retbuf
                        .get(symbol)
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

                    let model_prob_up = estimate_probability(price_to_beat, spot_price, sigma_horizon);
                    let model_edge_up = if up_ask.is_finite() {
                        model_prob_up - up_ask - crypto_fee_cost(up_ask)
                    } else {
                        f64::NAN
                    };
                    let flip_age_secs = dstate
                        .flip_ts
                        .map(|flip_ts| (*ts - flip_ts).num_milliseconds() as f64 / 1000.0)
                        .unwrap_or(f64::NAN);

                    rows.push(FactorObservation {
                        event_id: event.event_id.clone(),
                        symbol: symbol.clone(),
                        tick_ts: *ts,
                        time_remaining_secs: time_remaining,
                        signed_distance_to_beat: signed_distance,
                        abs_distance_to_beat: signed_distance.abs(),
                        drift_10s,
                        drift_30s,
                        flip_age_secs,
                        post_flip_drift: dstate.post_flip_drift,
                        sigma_horizon,
                        distance_over_sigma,
                        model_prob_up,
                        model_edge_up,
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
                        pm_up_ask: up_ask,
                        pm_down_ask: down_ask,
                        pm_lag_secs: up_lag,
                        settlement_up: if resolved_up_won { 1.0 } else { 0.0 },
                        future_up_ask_change_30s: None,
                        future_up_ask_change_60s: None,
                    });
                }
            }
            _ => {}
        }
    }

    rows.sort_by_key(|row| (row.event_id.clone(), row.tick_ts));
    attach_future_pm_labels(&mut rows, 30);
    attach_future_pm_labels_60(&mut rows, 60);
    rows
}

fn attach_future_pm_labels(rows: &mut [FactorObservation], horizon_secs: i64) {
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
                    if rows[*row_idx].pm_up_ask.is_finite() && rows[*next_idx].pm_up_ask.is_finite() {
                        future_change = Some(rows[*next_idx].pm_up_ask - rows[*row_idx].pm_up_ask);
                    }
                    break;
                }
            }
            rows[*row_idx].future_up_ask_change_30s = future_change;
        }
    }
}

fn attach_future_pm_labels_60(rows: &mut [FactorObservation], horizon_secs: i64) {
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
                    if rows[*row_idx].pm_up_ask.is_finite() && rows[*next_idx].pm_up_ask.is_finite() {
                        future_change = Some(rows[*next_idx].pm_up_ask - rows[*row_idx].pm_up_ask);
                    }
                    break;
                }
            }
            rows[*row_idx].future_up_ask_change_60s = future_change;
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
                last_tick_ts: rows.iter().map(|row| row.tick_ts).max().unwrap_or(first.tick_ts),
                settlement_up: first.settlement_up,
                signed_distance_to_beat: mean(rows.iter().map(|row| row.signed_distance_to_beat)),
                abs_distance_to_beat: mean(rows.iter().map(|row| row.abs_distance_to_beat)),
                drift_10s: mean(rows.iter().map(|row| row.drift_10s)),
                drift_30s: mean(rows.iter().map(|row| row.drift_30s)),
                flip_age_secs: mean(rows.iter().map(|row| row.flip_age_secs)),
                post_flip_drift: mean(rows.iter().map(|row| row.post_flip_drift)),
                sigma_horizon: mean(rows.iter().map(|row| row.sigma_horizon)),
                distance_over_sigma: mean(rows.iter().map(|row| row.distance_over_sigma)),
                model_prob_up: mean(rows.iter().map(|row| row.model_prob_up)),
                model_edge_up: mean(rows.iter().map(|row| row.model_edge_up)),
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
                pm_up_ask: mean(rows.iter().map(|row| row.pm_up_ask)),
                pm_down_ask: mean(rows.iter().map(|row| row.pm_down_ask)),
                pm_lag_secs: mean(rows.iter().map(|row| row.pm_lag_secs)),
            })
        })
        .collect()
}

pub fn factor_metrics(rows: &[FactorObservation], event_rows: &[EventFactorSummary]) -> Vec<FactorMetric> {
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
                    if std <= 1e-9 {
                        None
                    } else {
                        Some(mean / std)
                    }
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
        "distance_over_sigma" => rows.iter().map(|row| row.distance_over_sigma).collect::<Vec<_>>(),
        "model_prob_up" => rows.iter().map(|row| row.model_prob_up).collect::<Vec<_>>(),
        "model_edge_up" => rows.iter().map(|row| row.model_edge_up).collect::<Vec<_>>(),
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
        "pm_up_ask" => rows.iter().map(|row| row.pm_up_ask).collect::<Vec<_>>(),
        "pm_down_ask" => rows.iter().map(|row| row.pm_down_ask).collect::<Vec<_>>(),
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
        ("distance_over_sigma", |row| row.distance_over_sigma),
        ("model_prob_up", |row| row.model_prob_up),
        ("model_edge_up", |row| row.model_edge_up),
        ("obi", |row| row.obi),
        ("spread_bps", |row| row.spread_bps),
        ("microprice_offset_bps", |row| row.microprice_offset_bps),
        ("depth_ratio", |row| row.depth_ratio),
        ("depth_imbalance", |row| row.depth_imbalance),
        ("depth_far_ratio", |row| row.depth_far_ratio),
        ("depth_acceleration", |row| row.depth_acceleration),
        ("obi_10", |row| row.obi_10),
        ("pm_up_ask", |row| row.pm_up_ask),
        ("pm_lag_secs", |row| row.pm_lag_secs),
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
        ("distance_over_sigma", |row| row.distance_over_sigma),
        ("model_prob_up", |row| row.model_prob_up),
        ("model_edge_up", |row| row.model_edge_up),
        ("obi", |row| row.obi),
        ("spread_bps", |row| row.spread_bps),
        ("microprice_offset_bps", |row| row.microprice_offset_bps),
        ("depth_ratio", |row| row.depth_ratio),
        ("depth_imbalance", |row| row.depth_imbalance),
        ("depth_far_ratio", |row| row.depth_far_ratio),
        ("depth_acceleration", |row| row.depth_acceleration),
        ("obi_10", |row| row.obi_10),
        ("pm_up_ask", |row| row.pm_up_ask),
        ("pm_lag_secs", |row| row.pm_lag_secs),
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

fn crypto_fee_cost(entry_price: f64) -> f64 {
    0.02 * entry_price * (1.0 - entry_price)
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
        | MarketUpdate::ReferencePrice { ts, .. }
        | MarketUpdate::Kline { ts, .. } => *ts,
        MarketUpdate::EventDiscovered {
            end_time,
            window_secs,
            ..
        } => *end_time - chrono::Duration::seconds(*window_secs as i64) - chrono::Duration::hours(1),
        MarketUpdate::EventExpired { end_time, .. } => *end_time,
    }
}

fn bucket_icir(bucketed: &[(i64, f64, f64)], min_points: usize) -> Option<f64> {
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
            if ic.is_finite() {
                Some(ic)
            } else {
                None
            }
        })
        .collect();

    if ics.len() < 2 {
        return None;
    }
    let mean_ic = ics.iter().sum::<f64>() / ics.len() as f64;
    let std_ic = (ics.iter().map(|ic| (ic - mean_ic).powi(2)).sum::<f64>() / ics.len() as f64).sqrt();
    if std_ic <= 1e-9 {
        None
    } else {
        Some(mean_ic / std_ic)
    }
}

#[cfg(test)]
mod tests {
    use super::{attach_future_pm_labels, pearson_ic, spearman_ic, FactorObservation};
    use chrono::Utc;
    use serde_json::json;

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
        assert!(ic.abs() < 1e-9, "expected tie-correct spearman near 0, got {ic}");
    }

    #[test]
    fn future_label_attachment_is_order_invariant_after_sort() {
        let ts0 = chrono::DateTime::from_timestamp(0, 0).unwrap().with_timezone(&Utc);
        let ts1 = chrono::DateTime::from_timestamp(40, 0).unwrap().with_timezone(&Utc);
        let ts2 = chrono::DateTime::from_timestamp(80, 0).unwrap().with_timezone(&Utc);

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
                    distance_over_sigma: 0.0,
                    model_prob_up: 0.5,
                    model_edge_up: 0.0,
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
                    pm_up_ask: 0.40,
                    pm_down_ask: 0.60,
                    pm_lag_secs: 0.0,
                    settlement_up: 1.0,
                    future_up_ask_change_30s: None,
                    future_up_ask_change_60s: None,
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
                    distance_over_sigma: 0.0,
                    model_prob_up: 0.5,
                    model_edge_up: 0.0,
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
                    pm_up_ask: 0.10,
                    pm_down_ask: 0.90,
                    pm_lag_secs: 0.0,
                    settlement_up: 1.0,
                    future_up_ask_change_30s: None,
                    future_up_ask_change_60s: None,
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
                    distance_over_sigma: 0.0,
                    model_prob_up: 0.5,
                    model_edge_up: 0.0,
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
                    pm_up_ask: 0.25,
                    pm_down_ask: 0.75,
                    pm_lag_secs: 0.0,
                    settlement_up: 1.0,
                    future_up_ask_change_30s: None,
                    future_up_ask_change_60s: None,
                },
            ]
        };

        let mut rows = make_rows();
        rows.sort_by_key(|row| (row.event_id.clone(), row.tick_ts));
        attach_future_pm_labels(&mut rows, 30);

        let first = rows
            .iter()
            .find(|row| row.tick_ts == ts0)
            .expect("first row");
        assert_eq!(first.future_up_ask_change_30s, Some(0.15));
    }

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
