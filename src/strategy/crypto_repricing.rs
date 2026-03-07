//! Core types and decision logic for 5-minute crypto repricing trades.
//!
//! The baseline assumption is simple:
//! - trade only 5-minute crypto events
//! - enter during the early repricing window
//! - estimate fair probability from spot-vs-threshold distance plus short-window realized vol
//! - require Binance L2 confirmation before paying Polymarket taker costs

use std::collections::VecDeque;
use std::fmt;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::strategy::fee_model::FeeModel;
use crate::strategy::volatility::normal_cdf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepricingTradePhase {
    Observe,
    Trade,
    NoNewEntries,
    ReduceOnly,
    HardFlat,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepricingSide {
    Yes,
    No,
}

impl fmt::Display for RepricingSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RepricingSide::Yes => write!(f, "YES"),
            RepricingSide::No => write!(f, "NO"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionUrgency {
    MakerCandidate,
    Taker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryRejectReason {
    NotTradeWindow,
    MissingQuotes,
    SpreadTooWide,
    DepthTooThin,
    DirectionTooWeak,
    ZScoreTooLarge,
    GapBelowCost,
}

impl EntryRejectReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            EntryRejectReason::NotTradeWindow => "not_trade_window",
            EntryRejectReason::MissingQuotes => "missing_quotes",
            EntryRejectReason::SpreadTooWide => "spread_too_wide",
            EntryRejectReason::DepthTooThin => "depth_too_thin",
            EntryRejectReason::DirectionTooWeak => "direction_too_weak",
            EntryRejectReason::ZScoreTooLarge => "zscore_too_large",
            EntryRejectReason::GapBelowCost => "gap_below_cost",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolatilityCoefficients {
    pub beta0: f64,
    pub beta_30s: f64,
    pub beta_120s: f64,
    pub beta_300s: f64,
    pub beta_flow: f64,
    pub sigma_floor_return: f64,
    pub sigma_ceiling_return: f64,
}

impl Default for VolatilityCoefficients {
    fn default() -> Self {
        Self {
            beta0: -5.00,
            beta_30s: 0.45,
            beta_120s: 0.35,
            beta_300s: 0.20,
            beta_flow: 0.10,
            sigma_floor_return: 0.0005,
            sigma_ceiling_return: 0.05,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoRepricingConfig {
    pub symbols: Vec<String>,
    pub shares_per_trade: u64,
    pub trade_window_open_secs: u64,
    pub no_new_entries_secs: u64,
    pub reduce_only_secs: u64,
    pub hard_flat_secs: u64,
    pub direction_threshold: f64,
    pub max_abs_zscore: f64,
    pub tick_size: Decimal,
    pub max_spread_ticks: u32,
    pub require_depth_filter: bool,
    pub min_depth_multiple: Decimal,
    pub slippage_buffer: Decimal,
    pub min_net_gap_after_cost: Decimal,
    pub taker_urgency_gap: Decimal,
    pub mid_probability_extra_gap: Decimal,
    pub drift_per_window: f64,
    pub vol: VolatilityCoefficients,
}

impl Default for CryptoRepricingConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            shares_per_trade: 50,
            trade_window_open_secs: 240,
            no_new_entries_secs: 75,
            reduce_only_secs: 60,
            hard_flat_secs: 45,
            direction_threshold: 0.80,
            max_abs_zscore: 2.0,
            tick_size: dec!(0.01),
            max_spread_ticks: 3,
            require_depth_filter: false,
            min_depth_multiple: dec!(8),
            slippage_buffer: dec!(0.01),
            min_net_gap_after_cost: dec!(0.03),
            taker_urgency_gap: dec!(0.05),
            mid_probability_extra_gap: dec!(0.01),
            drift_per_window: 0.0,
            vol: VolatilityCoefficients::default(),
        }
    }
}

impl CryptoRepricingConfig {
    pub fn with_symbols(symbols: Vec<String>) -> Self {
        Self {
            symbols,
            ..Default::default()
        }
    }

    pub fn trade_phase(&self, remaining_secs: i64) -> RepricingTradePhase {
        if remaining_secs <= 0 {
            RepricingTradePhase::Expired
        } else if remaining_secs <= self.hard_flat_secs as i64 {
            RepricingTradePhase::HardFlat
        } else if remaining_secs <= self.reduce_only_secs as i64 {
            RepricingTradePhase::ReduceOnly
        } else if remaining_secs <= self.no_new_entries_secs as i64 {
            RepricingTradePhase::NoNewEntries
        } else if remaining_secs <= self.trade_window_open_secs as i64 {
            RepricingTradePhase::Trade
        } else {
            RepricingTradePhase::Observe
        }
    }

    pub fn max_spread(&self) -> Decimal {
        self.tick_size * Decimal::from(self.max_spread_ticks)
    }

    pub fn required_depth_shares(&self) -> u64 {
        (Decimal::from(self.shares_per_trade) * self.min_depth_multiple)
            .round()
            .to_u64()
            .unwrap_or(self.shares_per_trade)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct BinanceFeatureSnapshot {
    pub obi_5: Option<Decimal>,
    pub obi_10: Option<Decimal>,
    pub bid_volume_5: Option<Decimal>,
    pub ask_volume_5: Option<Decimal>,
    pub spread_bps: Option<Decimal>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct QuoteWithDepth {
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub ask_depth_shares: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct QuotePair {
    pub yes: QuoteWithDepth,
    pub no: QuoteWithDepth,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FairValueEstimate {
    pub probability_yes: f64,
    pub probability_no: f64,
    pub z_score: f64,
    pub sigma_return: f64,
    pub sigma_price: f64,
    pub rv_30s: f64,
    pub rv_120s: f64,
    pub rv_300s: f64,
    pub flow_shock: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CryptoRepricingDecision {
    pub side: RepricingSide,
    pub urgency: ExecutionUrgency,
    pub gross_gap: Decimal,
    pub cost_buffer: Decimal,
    pub net_gap_after_cost: Decimal,
    pub direction_score: f64,
    pub fair_probability: f64,
    pub quote_price: Decimal,
}

fn clamp_probability(p: f64) -> f64 {
    p.clamp(0.0, 1.0)
}

fn history_slice(
    history: &VecDeque<(DateTime<Utc>, Decimal)>,
    now: DateTime<Utc>,
    lookback_secs: i64,
) -> Vec<(DateTime<Utc>, Decimal)> {
    let cutoff = now - chrono::Duration::seconds(lookback_secs);
    history
        .iter()
        .copied()
        .filter(|(ts, _)| *ts >= cutoff && *ts <= now)
        .collect()
}

pub fn realized_variance(
    history: &VecDeque<(DateTime<Utc>, Decimal)>,
    now: DateTime<Utc>,
    lookback_secs: i64,
) -> Option<f64> {
    let slice = history_slice(history, now, lookback_secs);
    if slice.len() < 2 {
        return None;
    }

    let mut sum_sq = 0.0;
    let mut observations = 0usize;
    for window in slice.windows(2) {
        let prev = window[0].1.to_f64()?;
        let curr = window[1].1.to_f64()?;
        if prev <= 0.0 || curr <= 0.0 {
            continue;
        }
        let ret = (curr / prev).ln();
        sum_sq += ret * ret;
        observations += 1;
    }

    if observations == 0 {
        None
    } else {
        Some(sum_sq)
    }
}

pub fn flow_shock(
    history: &VecDeque<(DateTime<Utc>, Decimal)>,
    now: DateTime<Utc>,
    lookback_secs: i64,
) -> Option<f64> {
    let latest = history
        .iter()
        .rev()
        .find(|(ts, _)| *ts <= now)
        .or_else(|| history.back())
        .or_else(|| history.front())?;
    let latest_ts = latest.0;
    let latest_price = latest.1.to_f64()?;

    let target = latest_ts - chrono::Duration::seconds(lookback_secs);
    let reference = history
        .iter()
        .rev()
        .find(|(ts, _)| *ts <= target)
        .or_else(|| history.front())?;
    let reference_price = reference.1.to_f64()?;
    if latest_price <= 0.0 || reference_price <= 0.0 {
        return None;
    }
    Some((latest_price - reference_price) / reference_price)
}

pub fn estimate_remaining_fair_value(
    config: &CryptoRepricingConfig,
    spot_history: &VecDeque<(DateTime<Utc>, Decimal)>,
    now: DateTime<Utc>,
    spot: Decimal,
    strike: Decimal,
    remaining_secs: i64,
) -> Option<FairValueEstimate> {
    if remaining_secs <= 0 {
        let spot_f = spot.to_f64()?;
        let strike_f = strike.to_f64()?;
        let p = if spot_f > strike_f { 1.0 } else { 0.0 };
        return Some(FairValueEstimate {
            probability_yes: p,
            probability_no: 1.0 - p,
            z_score: if p >= 1.0 { 10.0 } else { -10.0 },
            sigma_return: config.vol.sigma_floor_return,
            sigma_price: config.vol.sigma_floor_return * spot_f,
            rv_30s: 0.0,
            rv_120s: 0.0,
            rv_300s: 0.0,
            flow_shock: 0.0,
        });
    }

    let rv_30s = realized_variance(spot_history, now, 30).unwrap_or(0.0);
    let rv_120s = realized_variance(spot_history, now, 120).unwrap_or(rv_30s);
    let rv_300s = realized_variance(spot_history, now, 300).unwrap_or(rv_120s);
    let flow = flow_shock(spot_history, now, 5).unwrap_or(0.0);

    let log_sigma = config.vol.beta0
        + config.vol.beta_30s * (rv_30s.max(1e-12)).ln()
        + config.vol.beta_120s * (rv_120s.max(1e-12)).ln()
        + config.vol.beta_300s * (rv_300s.max(1e-12)).ln()
        + config.vol.beta_flow * flow.abs();

    let sigma_return = log_sigma.exp().clamp(
        config.vol.sigma_floor_return,
        config.vol.sigma_ceiling_return,
    );

    let spot_f = spot.to_f64()?;
    let strike_f = strike.to_f64()?;
    if spot_f <= 0.0 {
        return None;
    }

    let tau = (remaining_secs as f64 / 300.0).clamp(1e-6, 1.0);
    let sigma_price = (sigma_return * spot_f).max(1e-6);
    let z = ((spot_f - strike_f) + config.drift_per_window * tau) / (sigma_price * tau.sqrt());
    let probability_yes = clamp_probability(normal_cdf(z));

    Some(FairValueEstimate {
        probability_yes,
        probability_no: 1.0 - probability_yes,
        z_score: z,
        sigma_return,
        sigma_price,
        rv_30s,
        rv_120s,
        rv_300s,
        flow_shock: flow,
    })
}

pub fn direction_score(binance: BinanceFeatureSnapshot, fair: FairValueEstimate) -> f64 {
    let obi_5 = binance.obi_5.and_then(|v| v.to_f64()).unwrap_or(0.0);
    let obi_10 = binance.obi_10.and_then(|v| v.to_f64()).unwrap_or(0.0);
    let queue_bias = match (binance.bid_volume_5, binance.ask_volume_5) {
        (Some(bid), Some(ask)) if bid + ask > Decimal::ZERO => {
            ((bid - ask) / (bid + ask)).to_f64().unwrap_or(0.0)
        }
        _ => 0.0,
    };

    let score = 0.45 * obi_5 + 0.20 * obi_10 + 0.20 * fair.flow_shock + 0.15 * queue_bias;
    score.clamp(-1.5, 1.5)
}

pub fn cost_buffer(
    config: &CryptoRepricingConfig,
    fee_model: &FeeModel,
    ask: Decimal,
    bid: Option<Decimal>,
) -> Decimal {
    let reference_bid = bid.unwrap_or_else(|| (ask - config.tick_size).max(config.tick_size));
    let spread = (ask - reference_bid).max(config.tick_size);
    let fee = ask * fee_model.effective_rate(ask);
    let mid_extra = if (dec!(0.45)..=dec!(0.55)).contains(&ask) {
        config.mid_probability_extra_gap
    } else {
        Decimal::ZERO
    };
    spread * dec!(1.2) + fee + config.slippage_buffer + mid_extra
}

pub fn evaluate_entry_candidate(
    config: &CryptoRepricingConfig,
    fee_model: &FeeModel,
    quotes: QuotePair,
    fair: FairValueEstimate,
    direction_score: f64,
    remaining_secs: i64,
) -> Result<CryptoRepricingDecision, EntryRejectReason> {
    if config.trade_phase(remaining_secs) != RepricingTradePhase::Trade {
        return Err(EntryRejectReason::NotTradeWindow);
    }
    if fair.z_score.abs() > config.max_abs_zscore {
        return Err(EntryRejectReason::ZScoreTooLarge);
    }

    let choose_yes = direction_score >= config.direction_threshold;
    let choose_no = direction_score <= -config.direction_threshold;
    if !choose_yes && !choose_no {
        return Err(EntryRejectReason::DirectionTooWeak);
    }

    let candidate = if choose_yes {
        let ask = quotes
            .yes
            .best_ask
            .ok_or(EntryRejectReason::MissingQuotes)?;
        let bid = quotes.yes.best_bid;
        let spread = ask - bid.unwrap_or_else(|| (ask - config.tick_size).max(config.tick_size));
        if spread > config.max_spread() {
            return Err(EntryRejectReason::SpreadTooWide);
        }
        if config.require_depth_filter {
            let depth = quotes
                .yes
                .ask_depth_shares
                .ok_or(EntryRejectReason::DepthTooThin)?;
            if depth < config.required_depth_shares() {
                return Err(EntryRejectReason::DepthTooThin);
            }
        }

        let gross_gap = Decimal::from_f64_retain(fair.probability_yes).unwrap_or(dec!(0.5)) - ask;
        let cost = cost_buffer(config, fee_model, ask, bid);
        let net = gross_gap - cost;
        if net < config.min_net_gap_after_cost {
            return Err(EntryRejectReason::GapBelowCost);
        }
        CryptoRepricingDecision {
            side: RepricingSide::Yes,
            urgency: if net >= config.taker_urgency_gap {
                ExecutionUrgency::Taker
            } else {
                ExecutionUrgency::MakerCandidate
            },
            gross_gap,
            cost_buffer: cost,
            net_gap_after_cost: net,
            direction_score,
            fair_probability: fair.probability_yes,
            quote_price: ask,
        }
    } else {
        let ask = quotes.no.best_ask.ok_or(EntryRejectReason::MissingQuotes)?;
        let bid = quotes.no.best_bid;
        let spread = ask - bid.unwrap_or_else(|| (ask - config.tick_size).max(config.tick_size));
        if spread > config.max_spread() {
            return Err(EntryRejectReason::SpreadTooWide);
        }
        if config.require_depth_filter {
            let depth = quotes
                .no
                .ask_depth_shares
                .ok_or(EntryRejectReason::DepthTooThin)?;
            if depth < config.required_depth_shares() {
                return Err(EntryRejectReason::DepthTooThin);
            }
        }

        let gross_gap = Decimal::from_f64_retain(fair.probability_no).unwrap_or(dec!(0.5)) - ask;
        let cost = cost_buffer(config, fee_model, ask, bid);
        let net = gross_gap - cost;
        if net < config.min_net_gap_after_cost {
            return Err(EntryRejectReason::GapBelowCost);
        }
        CryptoRepricingDecision {
            side: RepricingSide::No,
            urgency: if net >= config.taker_urgency_gap {
                ExecutionUrgency::Taker
            } else {
                ExecutionUrgency::MakerCandidate
            },
            gross_gap,
            cost_buffer: cost,
            net_gap_after_cost: net,
            direction_score,
            fair_probability: fair.probability_no,
            quote_price: ask,
        }
    };

    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn trade_phase_respects_requested_time_bands() {
        let cfg = CryptoRepricingConfig::default();
        assert_eq!(cfg.trade_phase(300), RepricingTradePhase::Observe);
        assert_eq!(cfg.trade_phase(240), RepricingTradePhase::Trade);
        assert_eq!(cfg.trade_phase(100), RepricingTradePhase::Trade);
        assert_eq!(cfg.trade_phase(75), RepricingTradePhase::NoNewEntries);
        assert_eq!(cfg.trade_phase(60), RepricingTradePhase::ReduceOnly);
        assert_eq!(cfg.trade_phase(45), RepricingTradePhase::HardFlat);
        assert_eq!(cfg.trade_phase(0), RepricingTradePhase::Expired);
    }

    #[test]
    fn mid_probability_cost_buffer_is_more_expensive() {
        let cfg = CryptoRepricingConfig::default();
        let fee = FeeModel::crypto();
        let atm = cost_buffer(&cfg, &fee, dec!(0.50), Some(dec!(0.49)));
        let wing = cost_buffer(&cfg, &fee, dec!(0.15), Some(dec!(0.14)));
        assert!(atm > wing);
    }

    #[test]
    fn evaluate_entry_requires_gap_above_costs() {
        let cfg = CryptoRepricingConfig::default();
        let fee = FeeModel::crypto();
        let fair = FairValueEstimate {
            probability_yes: 0.56,
            probability_no: 0.44,
            z_score: 0.25,
            sigma_return: 0.01,
            sigma_price: 1.0,
            rv_30s: 0.0,
            rv_120s: 0.0,
            rv_300s: 0.0,
            flow_shock: 0.02,
        };
        let quotes = QuotePair {
            yes: QuoteWithDepth {
                best_bid: Some(dec!(0.54)),
                best_ask: Some(dec!(0.55)),
                ask_depth_shares: Some(1000),
            },
            no: QuoteWithDepth {
                best_bid: Some(dec!(0.44)),
                best_ask: Some(dec!(0.45)),
                ask_depth_shares: Some(1000),
            },
        };

        let result = evaluate_entry_candidate(&cfg, &fee, quotes, fair, 0.9, 180);
        assert_eq!(result.unwrap_err(), EntryRejectReason::GapBelowCost);
    }

    #[test]
    fn evaluate_entry_picks_yes_when_gap_and_direction_align() {
        let mut cfg = CryptoRepricingConfig::default();
        cfg.min_net_gap_after_cost = dec!(0.01);
        let fee = FeeModel::crypto();
        let fair = FairValueEstimate {
            probability_yes: 0.42,
            probability_no: 0.58,
            z_score: -0.15,
            sigma_return: 0.01,
            sigma_price: 1.0,
            rv_30s: 0.0,
            rv_120s: 0.0,
            rv_300s: 0.0,
            flow_shock: 0.03,
        };
        let quotes = QuotePair {
            yes: QuoteWithDepth {
                best_bid: Some(dec!(0.33)),
                best_ask: Some(dec!(0.34)),
                ask_depth_shares: Some(1000),
            },
            no: QuoteWithDepth {
                best_bid: Some(dec!(0.65)),
                best_ask: Some(dec!(0.66)),
                ask_depth_shares: Some(1000),
            },
        };

        let decision = evaluate_entry_candidate(&cfg, &fee, quotes, fair, 0.95, 180).unwrap();
        assert_eq!(decision.side, RepricingSide::Yes);
        assert!(decision.net_gap_after_cost > Decimal::ZERO);
    }

    #[test]
    fn realized_variance_uses_rolling_prices() {
        let mut history = VecDeque::new();
        history.push_back((ts(0), dec!(100)));
        history.push_back((ts(10), dec!(100.5)));
        history.push_back((ts(20), dec!(101)));
        history.push_back((ts(30), dec!(100.8)));

        let rv = realized_variance(&history, ts(30), 30).unwrap();
        assert!(rv > 0.0);
    }
}
