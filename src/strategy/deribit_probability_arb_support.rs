use crate::error::{PloyError, Result};
use chrono::{DateTime, NaiveDate, Utc};
use ordered_float::OrderedFloat;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::str::FromStr;

/// Shared time conversion for Deribit expiry math.
pub(super) const SECONDS_PER_YEAR: f64 = 365.0 * 24.0 * 60.0 * 60.0;
pub(super) const DERIBIT_PUBLIC_API: &str = "https://www.deribit.com/api/v2/public";

/// A single volatility surface point.
#[derive(Debug, Clone)]
pub struct SurfacePoint {
    /// Time to expiry in years.
    pub t_years: f64,
    /// Option strike.
    pub strike: f64,
    /// Implied volatility (decimal, e.g. 0.55 = 55% annualized).
    pub iv: f64,
}

/// Surface snapshot grouped by maturity.
#[derive(Debug, Clone)]
pub struct VolSurfaceSnapshot {
    /// Maturity -> list of (strike, iv) pairs.
    pub by_maturity: BTreeMap<OrderedFloat<f64>, Vec<(f64, f64)>>,
    pub asof: DateTime<Utc>,
}

impl Default for VolSurfaceSnapshot {
    fn default() -> Self {
        Self {
            by_maturity: BTreeMap::new(),
            asof: Utc::now(),
        }
    }
}

/// Parsed market metadata from Polymarket question text.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedPolymarketQuestion {
    pub symbol: String,
    pub strike: Decimal,
}

/// Parse symbol + strike from market text.
pub fn parse_polymarket_question(text: &str) -> Option<ParsedPolymarketQuestion> {
    let symbol = detect_symbol(text)?;
    let strike = parse_dollar_amount(text).or_else(|| parse_number_amount(text))?;
    Some(ParsedPolymarketQuestion { symbol, strike })
}

fn detect_symbol(text: &str) -> Option<String> {
    let upper = text.to_ascii_uppercase();
    if upper.contains("BITCOIN") || upper.contains(" BTC") || upper.starts_with("BTC") {
        Some("BTC".to_string())
    } else if upper.contains("ETHEREUM") || upper.contains(" ETH") || upper.starts_with("ETH") {
        Some("ETH".to_string())
    } else if upper.contains("SOLANA") || upper.contains(" SOL") || upper.starts_with("SOL") {
        Some("SOL".to_string())
    } else if upper.contains(" XRP") || upper.starts_with("XRP") {
        Some("XRP".to_string())
    } else {
        None
    }
}

fn parse_dollar_amount(text: &str) -> Option<Decimal> {
    let idx = text.find('$')?;
    let rest = &text[idx + 1..];
    let mut raw = String::new();
    for ch in rest.chars() {
        if ch.is_ascii_digit() || ch == ',' || ch == '.' {
            raw.push(ch);
        } else if !raw.is_empty() {
            break;
        }
    }
    if raw.is_empty() {
        return None;
    }
    Decimal::from_str(&raw.replace(',', "")).ok()
}

fn parse_number_amount(text: &str) -> Option<Decimal> {
    let mut best: Option<Decimal> = None;
    for token in text.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')) {
        if token.is_empty() {
            continue;
        }
        let clean = token.replace(',', "");
        let Ok(value) = Decimal::from_str(&clean) else {
            continue;
        };
        // Filter out likely time/day fragments and keep realistic strikes.
        if value >= dec!(100) && value <= dec!(1_000_000) {
            best = Some(value);
            break;
        }
    }
    best
}

/// Standard normal CDF.
pub fn norm_cdf(x: f64) -> f64 {
    // Abramowitz-Stegun approximation
    let a1 = 0.254_829_592;
    let a2 = -0.284_496_736;
    let a3 = 1.421_413_741;
    let a4 = -1.453_152_027;
    let a5 = 1.061_405_429;
    let p = 0.327_591_1;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x_abs = x.abs() / (2.0f64).sqrt();
    let t = 1.0 / (1.0 + p * x_abs);
    let y = 1.0 - (((((a5 * t + a4) * t + a3) * t + a2) * t + a1) * t * (-(x_abs * x_abs)).exp());
    0.5 * (1.0 + sign * y)
}

/// Risk-neutral digital call probability under Black model with forward input.
pub fn binary_call_prob_forward(
    forward: f64,
    strike: f64,
    vol: f64,
    time_years: f64,
) -> Option<f64> {
    if !(forward.is_finite() && strike.is_finite() && vol.is_finite() && time_years.is_finite()) {
        return None;
    }
    if forward <= 0.0 || strike <= 0.0 || vol <= 0.0 || time_years <= 0.0 {
        return None;
    }

    let sigma_sqrt_t = vol * time_years.sqrt();
    if sigma_sqrt_t <= 1e-12 {
        return Some(if forward > strike { 1.0 } else { 0.0 });
    }

    let d2 = ((forward / strike).ln() - 0.5 * vol * vol * time_years) / sigma_sqrt_t;
    Some(norm_cdf(d2).clamp(0.0, 1.0))
}

fn interp_smile_at_strike(smile: &[(f64, f64)], target_strike: f64) -> Option<f64> {
    let mut points: Vec<(f64, f64)> = smile
        .iter()
        .copied()
        .filter(|(k, iv)| *k > 0.0 && *iv > 0.0 && k.is_finite() && iv.is_finite())
        .collect();
    if points.is_empty() {
        return None;
    }
    points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));

    if target_strike <= points[0].0 {
        return Some(points[0].1);
    }
    if target_strike >= points[points.len() - 1].0 {
        return Some(points[points.len() - 1].1);
    }

    for window in points.windows(2) {
        let (k1, iv1) = window[0];
        let (k2, iv2) = window[1];
        if target_strike >= k1 && target_strike <= k2 {
            let width = (k2 - k1).abs();
            if width < 1e-12 {
                return Some((iv1 + iv2) * 0.5);
            }
            let w = (target_strike - k1) / (k2 - k1);
            return Some(iv1 + (iv2 - iv1) * w);
        }
    }

    None
}

/// Interpolate IV at target maturity/strike using piecewise-linear smile and variance-time interpolation.
pub fn interpolate_iv_linear(
    surface: &VolSurfaceSnapshot,
    target_t_years: f64,
    target_strike: f64,
) -> Option<f64> {
    if target_t_years <= 0.0 || target_strike <= 0.0 {
        return None;
    }
    if surface.by_maturity.is_empty() {
        return None;
    }

    let mut maturities: Vec<f64> = surface
        .by_maturity
        .keys()
        .map(|k| k.into_inner())
        .filter(|t| *t > 0.0)
        .collect();
    maturities.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    maturities.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
    if maturities.is_empty() {
        return None;
    }

    if maturities.len() == 1 {
        let t = maturities[0];
        let smile = surface.by_maturity.get(&OrderedFloat(t))?;
        return interp_smile_at_strike(smile, target_strike);
    }

    let first_t = maturities[0];
    let last_t = maturities[maturities.len() - 1];

    let (t_low, t_high) = if target_t_years <= first_t {
        (first_t, first_t)
    } else if target_t_years >= last_t {
        (last_t, last_t)
    } else {
        let mut low = first_t;
        let mut high = last_t;
        for t in &maturities {
            if *t <= target_t_years {
                low = *t;
            }
            if *t >= target_t_years {
                high = *t;
                break;
            }
        }
        (low, high)
    };

    let iv_low = interp_smile_at_strike(
        surface.by_maturity.get(&OrderedFloat(t_low))?,
        target_strike,
    )?;
    if (t_high - t_low).abs() < 1e-12 {
        return Some(iv_low.max(1e-6));
    }

    let iv_high = interp_smile_at_strike(
        surface.by_maturity.get(&OrderedFloat(t_high))?,
        target_strike,
    )?;

    // Interpolate in total variance for better term-structure behavior.
    let v1 = iv_low * iv_low * t_low.max(1e-10);
    let v2 = iv_high * iv_high * t_high.max(1e-10);
    let w = ((target_t_years - t_low) / (t_high - t_low)).clamp(0.0, 1.0);
    let vt = v1 + (v2 - v1) * w;
    Some((vt / target_t_years.max(1e-10)).sqrt().max(1e-6))
}

/// Net edge after fees for a binary buy at price `ask`.
pub fn net_edge(model_prob: f64, ask: f64, fee_buffer: f64) -> f64 {
    model_prob - ask - fee_buffer
}

pub(super) fn normalize_symbol(raw: &str) -> String {
    raw.trim()
        .to_ascii_uppercase()
        .trim_end_matches("USDT")
        .to_string()
}

pub(super) fn symbol_to_deribit_currency(symbol: &str) -> Option<&'static str> {
    match normalize_symbol(symbol).as_str() {
        "BTC" => Some("BTC"),
        "ETH" => Some("ETH"),
        _ => None,
    }
}

pub(super) fn parse_event_end(end_date: Option<&String>) -> Option<DateTime<Utc>> {
    end_date
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

pub(super) fn parse_string_array(raw: Option<&String>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let s = raw.trim();
    if s.is_empty() {
        return Vec::new();
    }

    if let Ok(v) = serde_json::from_str::<Vec<String>>(s) {
        return v;
    }
    if let Ok(v) = serde_json::from_str::<Vec<serde_json::Value>>(s) {
        return v
            .into_iter()
            .map(|x| match x {
                serde_json::Value::String(v) => v,
                serde_json::Value::Number(v) => v.to_string(),
                serde_json::Value::Bool(v) => v.to_string(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            })
            .collect();
    }
    Vec::new()
}

pub(super) fn infer_yes_index(outcomes: &[String]) -> usize {
    outcomes
        .iter()
        .position(|o| {
            let u = o.to_ascii_uppercase();
            u.contains("YES") || u.contains("UP") || u.contains("ABOVE")
        })
        .unwrap_or(0)
}

pub(super) fn spread_bps(bid: Option<Decimal>, ask: Option<Decimal>) -> Option<u32> {
    let (Some(bid), Some(ask)) = (bid, ask) else {
        return None;
    };
    if bid <= Decimal::ZERO || ask <= bid {
        return None;
    }
    let bps = ((ask - bid) / bid * dec!(10000)).round();
    bps.to_u32()
}

pub(super) fn kelly_fraction_binary(win_probability: f64, entry_price: f64) -> f64 {
    if !(0.0..1.0).contains(&win_probability) || !(0.0..1.0).contains(&entry_price) {
        return 0.0;
    }
    let p = win_probability;
    let q = 1.0 - p;
    let b = (1.0 - entry_price) / entry_price;
    if b <= 0.0 {
        return 0.0;
    }
    ((p * b - q) / b).max(0.0)
}

#[derive(Debug, Deserialize)]
struct DeribitRpcResponse<T> {
    result: T,
}

#[derive(Debug, Deserialize)]
struct DeribitTicker {
    #[serde(default)]
    mark_price: Option<f64>,
    #[serde(default)]
    index_price: Option<f64>,
    #[serde(default)]
    last_price: Option<f64>,
    #[serde(default)]
    underlying_price: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct DeribitBookSummary {
    instrument_name: String,
    #[serde(default)]
    mark_iv: Option<f64>,
    #[serde(default)]
    bid_iv: Option<f64>,
    #[serde(default)]
    ask_iv: Option<f64>,
}

#[derive(Clone)]
pub(super) struct DeribitPublicClient {
    http: reqwest::Client,
    base_url: String,
}

impl DeribitPublicClient {
    pub(super) fn new(base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
        }
    }

    pub(super) async fn fetch_forward_price(&self, currency: &str) -> Result<f64> {
        let instrument = format!("{}-PERPETUAL", currency.to_ascii_uppercase());
        let url = format!(
            "{}/get_ticker?instrument_name={}",
            self.base_url, instrument
        );
        let resp = self.http.get(&url).send().await?.error_for_status()?;
        let body: DeribitRpcResponse<DeribitTicker> = resp.json().await?;
        let ticker = body.result;

        let forward = ticker
            .mark_price
            .or(ticker.index_price)
            .or(ticker.underlying_price)
            .or(ticker.last_price)
            .ok_or_else(|| {
                PloyError::MarketDataUnavailable("Deribit forward unavailable".into())
            })?;

        if !forward.is_finite() || forward <= 0.0 {
            return Err(PloyError::InvalidMarketData(format!(
                "invalid Deribit forward {} for {}",
                forward, currency
            )));
        }

        Ok(forward)
    }

    pub(super) async fn fetch_surface(
        &self,
        currency: &str,
        now: DateTime<Utc>,
    ) -> Result<VolSurfaceSnapshot> {
        let url = format!(
            "{}/get_book_summary_by_currency?currency={}&kind=option",
            self.base_url,
            currency.to_ascii_uppercase()
        );
        let resp = self.http.get(&url).send().await?.error_for_status()?;
        let body: DeribitRpcResponse<Vec<DeribitBookSummary>> = resp.json().await?;

        let mut surface = VolSurfaceSnapshot {
            by_maturity: BTreeMap::new(),
            asof: now,
        };

        for row in body.result {
            let Some((exp, strike)) = parse_deribit_instrument(&row.instrument_name) else {
                continue;
            };

            let t_secs = (exp - now).num_seconds();
            if t_secs <= 0 {
                continue;
            }

            let Some(iv_raw) = pick_iv(&row) else {
                continue;
            };
            let iv = normalize_iv(iv_raw);
            if !(0.0001..=5.0).contains(&iv) {
                continue;
            }

            let t = (t_secs as f64) / SECONDS_PER_YEAR;
            surface
                .by_maturity
                .entry(OrderedFloat(t))
                .or_default()
                .push((strike, iv));
        }

        if surface.by_maturity.is_empty() {
            return Err(PloyError::MarketDataUnavailable(format!(
                "empty Deribit surface for {}",
                currency
            )));
        }

        for smile in surface.by_maturity.values_mut() {
            smile.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
            // Merge duplicate strikes (e.g., call/put both present) with simple averaging.
            let mut merged: Vec<(f64, f64)> = Vec::new();
            for (strike, iv) in smile.drain(..) {
                if let Some(last) = merged.last_mut() {
                    if (last.0 - strike).abs() < 1e-8 {
                        last.1 = 0.5 * (last.1 + iv);
                        continue;
                    }
                }
                merged.push((strike, iv));
            }
            *smile = merged;
        }

        Ok(surface)
    }
}

fn pick_iv(row: &DeribitBookSummary) -> Option<f64> {
    row.mark_iv.or_else(|| match (row.bid_iv, row.ask_iv) {
        (Some(b), Some(a)) => Some((a + b) * 0.5),
        (Some(v), None) | (None, Some(v)) => Some(v),
        _ => None,
    })
}

fn normalize_iv(raw: f64) -> f64 {
    if raw > 3.0 {
        raw / 100.0
    } else {
        raw
    }
}

fn parse_deribit_instrument(name: &str) -> Option<(DateTime<Utc>, f64)> {
    // Example: BTC-29MAR24-100000-C
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() != 4 {
        return None;
    }

    let expiry = parse_deribit_expiry(parts[1])?;
    let strike = parts[2].parse::<f64>().ok()?;
    if strike <= 0.0 {
        return None;
    }

    Some((expiry, strike))
}

pub(super) fn parse_deribit_expiry(code: &str) -> Option<DateTime<Utc>> {
    // Format: DDMMMYY (e.g., 29MAR24), Deribit standard option expiry at 08:00 UTC.
    if code.len() != 7 {
        return None;
    }

    let day: u32 = code[0..2].parse().ok()?;
    let mon = match &code[2..5].to_ascii_uppercase()[..] {
        "JAN" => 1,
        "FEB" => 2,
        "MAR" => 3,
        "APR" => 4,
        "MAY" => 5,
        "JUN" => 6,
        "JUL" => 7,
        "AUG" => 8,
        "SEP" => 9,
        "OCT" => 10,
        "NOV" => 11,
        "DEC" => 12,
        _ => return None,
    };
    let yy: i32 = code[5..7].parse::<i32>().ok()?;
    let year = 2000 + yy;

    let date = NaiveDate::from_ymd_opt(year, mon, day)?;
    let naive = date.and_hms_opt(8, 0, 0)?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}
