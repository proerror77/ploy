//! Crypto UP/DOWN backtest (5m + 15m) using:
//! - Polymarket Gamma series events (ground-truth settlement)
//! - Binance REST klines for spot prices at start/entry
//! - Optional Postgres orderbook snapshots for historical entry asks (EV/PnL)

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::str::FromStr;
use tracing::{info, warn};

use crate::adapters::{GammaEventInfo, PolymarketClient};
use crate::collector::{BinanceKlineClient, Kline};
use crate::domain::Side;
use crate::error::{PloyError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpDownBacktestConfig {
    /// Comma-separated symbols, e.g. "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT".
    pub symbols: Vec<String>,
    /// Look back this many days of settled events.
    pub lookback_days: u64,
    /// Max settled events per series (cap API + runtime).
    pub max_events_per_series: usize,
    /// Entry times as "seconds remaining" (comma-separated).
    pub entry_remaining_secs: Vec<u64>,
    /// Filter out near-flat windows: require |(entry-start)/start| >= threshold.
    pub min_window_move_pcts: Vec<Decimal>,
    /// Binance kline interval (recommended: "1m").
    pub binance_interval: String,
    /// Lookback window in minutes used to estimate volatility at entry time.
    pub vol_lookback_minutes: usize,
    /// If true, query Postgres `clob_orderbook_snapshots` for historical best ask prices.
    pub use_db_prices: bool,
    /// Optional DB URL override. If None, will use `PLOY_DATABASE__URL` / `DATABASE_URL`.
    pub db_url: Option<String>,
    /// Reject orderbook snapshots older than this (seconds) relative to entry time.
    pub max_snapshot_age_secs: i64,
}

impl Default for UpDownBacktestConfig {
    fn default() -> Self {
        Self {
            symbols: vec![
                "BTCUSDT".into(),
                "ETHUSDT".into(),
                "SOLUSDT".into(),
                "XRPUSDT".into(),
            ],
            lookback_days: 7,
            max_events_per_series: 500,
            entry_remaining_secs: vec![60, 120, 300, 600, 900],
            min_window_move_pcts: vec![Decimal::ZERO, Decimal::new(1, 4), Decimal::new(2, 4)], // 0, 0.0001, 0.0002
            binance_interval: "1m".into(),
            vol_lookback_minutes: 60,
            use_db_prices: false,
            db_url: None,
            max_snapshot_age_secs: 120,
        }
    }
}

#[derive(Debug, Clone)]
struct ResolvedWindow {
    symbol: String,
    series_id: String,
    horizon: String,
    event_id: String,
    slug: String,
    title: String,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    up_token_id: String,
    down_token_id: String,
    winner: Side,
}

#[derive(Debug, Clone, Default)]
struct AggStats {
    n: u64,
    wins: u64,
    // only populated when DB prices are available
    priced_n: u64,
    sum_entry_price: Decimal,
    sum_edge: Decimal,
    sum_ev_per_share: Decimal,
    sum_realized_pnl_per_share: Decimal,
}

impl AggStats {
    fn record_directional(&mut self, won: bool) {
        self.n += 1;
        if won {
            self.wins += 1;
        }
    }

    fn record_priced(&mut self, entry_price: Decimal, p_win: Decimal, won: bool) {
        self.priced_n += 1;
        self.sum_entry_price += entry_price;
        let edge = p_win - entry_price;
        self.sum_edge += edge;
        self.sum_ev_per_share += edge;
        let realized = if won {
            Decimal::ONE - entry_price
        } else {
            Decimal::ZERO - entry_price
        };
        self.sum_realized_pnl_per_share += realized;
    }

    fn win_rate(&self) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        self.wins as f64 / self.n as f64
    }
}

fn normal_cdf(x: f64) -> f64 {
    // Abramowitz-Stegun approximation
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();

    let t = 1.0 / (1.0 + p * x);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-x * x / 2.0).exp();

    0.5 * (1.0 + sign * y)
}

fn series_ids_for_symbol(symbol: &str) -> Vec<String> {
    match symbol.trim().to_ascii_uppercase().as_str() {
        "BTCUSDT" => vec!["10684".into(), "10192".into()],
        "ETHUSDT" => vec!["10683".into(), "10191".into()],
        "SOLUSDT" => vec!["10686".into(), "10423".into()],
        "XRPUSDT" => vec!["10685".into(), "10422".into()],
        _ => Vec::new(),
    }
}

fn horizon_for_series(series_id: &str) -> &'static str {
    match series_id {
        "10684" | "10683" | "10686" | "10685" => "5m",
        "10192" | "10191" | "10423" | "10422" => "15m",
        _ => "other",
    }
}

fn parse_time(raw: &Option<String>) -> Option<DateTime<Utc>> {
    let s = raw.as_ref()?;
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
}

fn classify_outcome_label(outcome: &str) -> Option<Side> {
    let o = outcome.trim().to_ascii_lowercase();
    if o == "up" || o == "yes" || o.starts_with('↑') {
        return Some(Side::Up);
    }
    if o == "down" || o == "no" || o.starts_with('↓') {
        return Some(Side::Down);
    }
    if o.contains("up") {
        return Some(Side::Up);
    }
    if o.contains("down") {
        return Some(Side::Down);
    }
    None
}

fn parse_json_vec(raw: &Option<String>) -> Vec<String> {
    raw.as_ref()
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .unwrap_or_default()
}

fn resolved_window_from_gamma(
    symbol: &str,
    series_id: &str,
    event: &GammaEventInfo,
) -> Option<ResolvedWindow> {
    if !event.closed {
        return None;
    }

    let start_time = parse_time(&event.start_time)?;
    let end_time = parse_time(&event.end_date)?;

    let market = event.markets.first()?;
    let outcomes = parse_json_vec(&market.outcomes);
    let tokens = parse_json_vec(&market.clob_token_ids);
    let prices_raw = parse_json_vec(&market.outcome_prices);

    if outcomes.len() < 2 || tokens.len() < 2 || prices_raw.len() < 2 {
        return None;
    }
    if outcomes.len() != tokens.len() || outcomes.len() != prices_raw.len() {
        return None;
    }

    let mut up_idx: Option<usize> = None;
    let mut down_idx: Option<usize> = None;
    for (idx, o) in outcomes.iter().enumerate() {
        match classify_outcome_label(o) {
            Some(Side::Up) => up_idx = up_idx.or(Some(idx)),
            Some(Side::Down) => down_idx = down_idx.or(Some(idx)),
            _ => {}
        }
    }
    let up_idx = up_idx.unwrap_or(0);
    let down_idx = down_idx.unwrap_or(1);

    let mut max_price = Decimal::ZERO;
    let mut winner_idx: Option<usize> = None;
    for (idx, raw) in prices_raw.iter().enumerate() {
        let p = Decimal::from_str(raw).unwrap_or(Decimal::ZERO);
        if p > max_price {
            max_price = p;
            winner_idx = Some(idx);
        }
    }
    let winner_idx = winner_idx?;

    let winner = if winner_idx == up_idx {
        Side::Up
    } else if winner_idx == down_idx {
        Side::Down
    } else {
        // Unexpected ordering; fall back to label classification.
        classify_outcome_label(&outcomes[winner_idx])?
    };

    Some(ResolvedWindow {
        symbol: symbol.to_string(),
        series_id: series_id.to_string(),
        horizon: horizon_for_series(series_id).to_string(),
        event_id: event.id.clone(),
        slug: event.slug.clone().unwrap_or_default(),
        title: event.title.clone().unwrap_or_default(),
        start_time,
        end_time,
        up_token_id: tokens.get(up_idx).cloned().unwrap_or_default(),
        down_token_id: tokens.get(down_idx).cloned().unwrap_or_default(),
        winner,
    })
}

fn is_minute_aligned(t: DateTime<Utc>) -> bool {
    t.timestamp() % 60 == 0 && t.timestamp_subsec_nanos() == 0
}

fn price_at(klines: &[Kline], t: DateTime<Utc>) -> Option<Decimal> {
    if klines.is_empty() {
        return None;
    }
    // If aligned to candle open, prefer OPEN of that candle.
    if is_minute_aligned(t) {
        if let Ok(idx) = klines.binary_search_by_key(&t, |k| k.open_time) {
            return Some(klines[idx].open);
        }
    }

    // Otherwise: last candle with open_time <= t, use CLOSE.
    let idx = match klines.binary_search_by_key(&t, |k| k.open_time) {
        Ok(i) => i,
        Err(0) => return None,
        Err(i) => i - 1,
    };
    Some(klines[idx].close)
}

fn sigma_1m_at(klines: &[Kline], t: DateTime<Utc>, lookback_minutes: usize) -> Option<f64> {
    if klines.len() < 10 {
        return None;
    }

    let idx = match klines.binary_search_by_key(&t, |k| k.open_time) {
        Ok(i) => i,
        Err(0) => return None,
        Err(i) => i - 1,
    };

    let needed = lookback_minutes.max(5);
    let start = idx.saturating_sub(needed);
    let slice = &klines[start..=idx];
    if slice.len() < 6 {
        return None;
    }

    let mut returns: Vec<f64> = Vec::with_capacity(slice.len().saturating_sub(1));
    for w in slice.windows(2) {
        let prev = w[1].close.to_f64().unwrap_or(0.0);
        let curr = w[0].close.to_f64().unwrap_or(0.0);
        if prev > 0.0 && curr.is_finite() && prev.is_finite() {
            returns.push((curr - prev) / prev);
        }
    }
    if returns.len() < 5 {
        return None;
    }

    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let var = returns
        .iter()
        .map(|r| {
            let d = r - mean;
            d * d
        })
        .sum::<f64>()
        / returns.len() as f64;
    Some(var.sqrt())
}

fn estimate_p_up(window_move: Decimal, sigma_1m: Option<f64>, remaining_secs: u64) -> Decimal {
    let w = window_move.to_f64().unwrap_or(0.0);
    let Some(sig_1m) = sigma_1m else {
        return Decimal::new(50, 2); // 0.50
    };
    if !sig_1m.is_finite() || sig_1m <= 0.0 {
        return Decimal::new(50, 2);
    }
    let rem_min = (remaining_secs as f64 / 60.0).max(0.0);
    let sigma_rem = sig_1m * rem_min.sqrt();
    if !sigma_rem.is_finite() || sigma_rem <= 0.0 {
        return Decimal::new(50, 2);
    }
    let p = normal_cdf(w / sigma_rem).clamp(0.001, 0.999);
    Decimal::from_f64(p).unwrap_or(Decimal::new(50, 2))
}

async fn best_ask_before(
    pool: &PgPool,
    token_id: &str,
    entry_time: DateTime<Utc>,
    max_age_secs: i64,
) -> Option<Decimal> {
    let row = sqlx::query(
        r#"
        SELECT received_at, asks->0->>'price' AS best_ask
        FROM clob_orderbook_snapshots
        WHERE token_id = $1 AND received_at <= $2
        ORDER BY received_at DESC
        LIMIT 1
        "#,
    )
    .bind(token_id)
    .bind(entry_time)
    .fetch_optional(pool)
    .await
    .ok()??;

    let received_at: DateTime<Utc> = row.try_get("received_at").ok()?;
    if entry_time
        .signed_duration_since(received_at)
        .num_seconds()
        .abs()
        > max_age_secs
    {
        return None;
    }

    let best_ask: String = row.try_get("best_ask").ok()?;
    Decimal::from_str(&best_ask).ok()
}

pub async fn run_updown_backtest(cfg: UpDownBacktestConfig) -> Result<()> {
    let since = Utc::now() - ChronoDuration::days(cfg.lookback_days as i64);

    let mut symbols: Vec<String> = cfg
        .symbols
        .iter()
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .collect();
    symbols.sort();
    symbols.dedup();

    if symbols.is_empty() {
        return Err(PloyError::Validation("no symbols provided".to_string()));
    }

    let pm = PolymarketClient::new("https://clob.polymarket.com", true)?;
    let binance = BinanceKlineClient::new();

    let mut pool: Option<PgPool> = None;
    if cfg.use_db_prices {
        let url = cfg
            .db_url
            .clone()
            .or_else(|| std::env::var("PLOY_DATABASE__URL").ok())
            .or_else(|| std::env::var("DATABASE_URL").ok());
        if let Some(url) = url {
            match PgPoolOptions::new().max_connections(2).connect(&url).await {
                Ok(p) => pool = Some(p),
                Err(e) => {
                    warn!(error = %e, "failed to connect DB; continuing without priced backtest")
                }
            }
        } else {
            warn!("use_db_prices=true but no db url found (PLOY_DATABASE__URL/DATABASE_URL)");
        }
    }

    // 1) Load settled windows from Gamma series
    let mut windows: Vec<ResolvedWindow> = Vec::new();
    for symbol in &symbols {
        let series_ids = series_ids_for_symbol(symbol);
        if series_ids.is_empty() {
            warn!(symbol, "no series mapping for symbol; skipping");
            continue;
        }
        for series_id in series_ids {
            let events = pm.get_all_events_in_series(&series_id).await?;
            let mut settled: Vec<ResolvedWindow> = events
                .iter()
                .filter_map(|e| resolved_window_from_gamma(symbol, &series_id, e))
                .filter(|w| w.end_time >= since)
                .collect();
            settled.sort_by_key(|w| w.end_time);
            if settled.len() > cfg.max_events_per_series {
                settled = settled
                    .into_iter()
                    .rev()
                    .take(cfg.max_events_per_series)
                    .collect::<Vec<_>>();
                settled.sort_by_key(|w| w.end_time);
            }
            info!(
                symbol,
                series_id,
                horizon = %horizon_for_series(&series_id),
                settled = settled.len(),
                "loaded settled windows"
            );
            windows.extend(settled);
        }
    }

    if windows.is_empty() {
        return Err(PloyError::Internal(
            "no settled windows found for backtest".to_string(),
        ));
    }

    // 2) Fetch Binance klines for each symbol over the required time range.
    let earliest = windows
        .iter()
        .map(|w| w.start_time)
        .min()
        .unwrap_or_else(Utc::now);
    let latest = windows
        .iter()
        .map(|w| w.end_time)
        .max()
        .unwrap_or_else(Utc::now);

    let fetch_start = earliest - ChronoDuration::minutes(cfg.vol_lookback_minutes as i64 + 5);
    let fetch_end = latest + ChronoDuration::minutes(5);

    let mut klines_by_symbol: HashMap<String, Vec<Kline>> = HashMap::new();
    for symbol in &symbols {
        let ks = binance
            .fetch_klines_range(symbol, &cfg.binance_interval, fetch_start, fetch_end)
            .await?;
        if ks.is_empty() {
            warn!(symbol, "no klines returned for symbol");
        }
        klines_by_symbol.insert(symbol.clone(), ks);
    }

    // 3) Sweep parameters.
    let mut stats: HashMap<String, AggStats> = HashMap::new();
    for w in &windows {
        let Some(ks) = klines_by_symbol.get(&w.symbol) else {
            continue;
        };

        let Some(start_price) = price_at(ks, w.start_time) else {
            continue;
        };
        if start_price <= Decimal::ZERO {
            continue;
        }

        for &remaining in &cfg.entry_remaining_secs {
            let entry_time = w.end_time - ChronoDuration::seconds(remaining as i64);
            if entry_time < w.start_time || entry_time >= w.end_time {
                continue;
            }

            let Some(entry_spot) = price_at(ks, entry_time) else {
                continue;
            };
            if entry_spot <= Decimal::ZERO {
                continue;
            }

            let window_move = (entry_spot - start_price) / start_price;
            let predicted = if window_move >= Decimal::ZERO {
                Side::Up
            } else {
                Side::Down
            };
            let won = predicted == w.winner;
            let sigma_1m = sigma_1m_at(ks, entry_time, cfg.vol_lookback_minutes);
            let p_up = estimate_p_up(window_move, sigma_1m, remaining);
            let p_win = match predicted {
                Side::Up => p_up,
                Side::Down => Decimal::ONE - p_up,
            };

            let token_id = match predicted {
                Side::Up => w.up_token_id.as_str(),
                Side::Down => w.down_token_id.as_str(),
            };

            for min_move in &cfg.min_window_move_pcts {
                if window_move.abs() < *min_move {
                    continue;
                }

                let key = format!("{}|{}|{}", w.horizon, remaining, min_move);
                let entry = stats.entry(key).or_default();
                entry.record_directional(won);

                if let Some(pool) = pool.as_ref() {
                    if let Some(entry_price) =
                        best_ask_before(pool, token_id, entry_time, cfg.max_snapshot_age_secs).await
                    {
                        if entry_price > Decimal::ZERO && entry_price < Decimal::ONE {
                            entry.record_priced(entry_price, p_win, won);
                        }
                    }
                }
            }
        }
    }

    // 4) Print summary table.
    println!();
    println!("Crypto UP/DOWN 回測（{} 天）", cfg.lookback_days);
    println!(
        "symbols={} interval={} entry_remaining_secs={:?} min_window_move_pcts={:?}",
        symbols.join(","),
        cfg.binance_interval,
        cfg.entry_remaining_secs,
        cfg.min_window_move_pcts
    );
    if pool.is_some() {
        println!("包含 DB 歷史 best-ask 估算 EV/PNL（clob_orderbook_snapshots）");
    } else {
        println!("未包含 DB 歷史 best-ask（只統計方向勝率/覆蓋率）");
    }
    println!();
    println!(
        "horizon  t_rem  min_move     n     win%   priced   avg_px   avg_edge   avg_ev   avg_pnl"
    );

    let mut rows: Vec<(String, AggStats)> = stats.into_iter().collect();
    rows.sort_by(|a, b| {
        b.1.win_rate()
            .partial_cmp(&a.1.win_rate())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for (key, s) in rows {
        let parts: Vec<&str> = key.split('|').collect();
        let horizon = parts.get(0).copied().unwrap_or("-");
        let t_rem = parts.get(1).copied().unwrap_or("-");
        let min_move = parts.get(2).copied().unwrap_or("-");

        let priced = s.priced_n;
        let avg_px = if priced > 0 {
            s.sum_entry_price / Decimal::from(priced)
        } else {
            Decimal::ZERO
        };
        let avg_edge = if priced > 0 {
            s.sum_edge / Decimal::from(priced)
        } else {
            Decimal::ZERO
        };
        let avg_ev = if priced > 0 {
            s.sum_ev_per_share / Decimal::from(priced)
        } else {
            Decimal::ZERO
        };
        let avg_pnl = if priced > 0 {
            s.sum_realized_pnl_per_share / Decimal::from(priced)
        } else {
            Decimal::ZERO
        };

        println!(
            "{:<6} {:>5} {:>9} {:>6} {:>7.2} {:>7} {:>8.3} {:>9.3} {:>8.3} {:>8.3}",
            horizon,
            t_rem,
            min_move,
            s.n,
            s.win_rate() * 100.0,
            priced,
            avg_px,
            avg_edge,
            avg_ev,
            avg_pnl
        );
    }

    println!();
    Ok(())
}
