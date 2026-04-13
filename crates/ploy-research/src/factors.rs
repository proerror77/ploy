use std::collections::{BTreeMap, HashMap, VecDeque};

use chrono::{DateTime, Utc};
use ploy_strategy_bundles::traits::MarketUpdate;
use polars::prelude::*;
use rust_decimal::prelude::ToPrimitive;

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
    pub bid_depth_near: f64,
    pub ask_depth_near: f64,
    pub depth_ratio: f64,
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
    pub bid_depth_near: f64,
    pub ask_depth_near: f64,
    pub depth_ratio: f64,
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
    spread_bps: u32,
    bid_depth_near: f64,
    ask_depth_near: f64,
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

pub fn build_factor_observations(updates: &[MarketUpdate]) -> Vec<FactorObservation> {
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
    let mut rows = Vec::new();

    for update in updates {
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
                state.spread_bps = *spread_bps;
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
                        spread_bps: *spread_bps,
                        bid_depth_near: *bid_depth_near,
                        ask_depth_near: *ask_depth_near,
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

                    let lob_state = lob.get(symbol).cloned().unwrap_or_default();
                    let depth_ratio = if lob_state.ask_depth_near > 0.0 {
                        lob_state.bid_depth_near / lob_state.ask_depth_near
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
                        spread_bps: lob_state.spread_bps as f64,
                        bid_depth_near: lob_state.bid_depth_near,
                        ask_depth_near: lob_state.ask_depth_near,
                        depth_ratio,
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

    attach_future_pm_labels(&mut rows, 30);
    attach_future_pm_labels_60(&mut rows, 60);
    rows
}

fn attach_future_pm_labels(rows: &mut [FactorObservation], horizon_secs: i64) {
    let mut grouped: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, row) in rows.iter().enumerate() {
        grouped.entry(row.event_id.clone()).or_default().push(idx);
    }

    for indexes in grouped.values() {
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

    for indexes in grouped.values() {
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
                bid_depth_near: mean(rows.iter().map(|row| row.bid_depth_near)),
                ask_depth_near: mean(rows.iter().map(|row| row.ask_depth_near)),
                depth_ratio: mean(rows.iter().map(|row| row.depth_ratio)),
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
        "bid_depth_near" => rows.iter().map(|row| row.bid_depth_near).collect::<Vec<_>>(),
        "ask_depth_near" => rows.iter().map(|row| row.ask_depth_near).collect::<Vec<_>>(),
        "depth_ratio" => rows.iter().map(|row| row.depth_ratio).collect::<Vec<_>>(),
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
        ("depth_ratio", |row| row.depth_ratio),
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
        ("depth_ratio", |row| row.depth_ratio),
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
    for (rank, (idx, _)) in indexed.into_iter().enumerate() {
        ranks[idx] = rank as f64;
    }
    ranks
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
    use super::{pearson_ic, spearman_ic};

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
}
