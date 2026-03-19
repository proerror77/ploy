use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use tracing::{debug, info};

use super::VolatilityArbEngine;

pub use pricing_math::{
    calculate_fair_yes_price, calculate_implied_volatility, calculate_kelly_fraction, norm_cdf,
};

mod pricing_math {
    use std::f64::consts::PI;

    /// Standard normal CDF approximation (Abramowitz and Stegun)
    pub fn norm_cdf(x: f64) -> f64 {
        let a1 = 0.254829592;
        let a2 = -0.284496736;
        let a3 = 1.421413741;
        let a4 = -1.453152027;
        let a5 = 1.061405429;
        let p = 0.3275911;

        let sign = if x < 0.0 { -1.0 } else { 1.0 };
        let z = x.abs() / 2.0_f64.sqrt();

        let t = 1.0 / (1.0 + p * z);
        let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-z * z).exp();

        0.5 * (1.0 + sign * y)
    }

    fn norm_pdf(x: f64) -> f64 {
        (-x * x / 2.0).exp() / (2.0 * PI).sqrt()
    }

    fn norm_inv(p: f64) -> f64 {
        if p <= 0.0 {
            return f64::NEG_INFINITY;
        }
        if p >= 1.0 {
            return f64::INFINITY;
        }

        let a = [
            -3.969683028665376e+01,
            2.209460984245205e+02,
            -2.759285104469687e+02,
            1.383577518672690e+02,
            -3.066479806614716e+01,
            2.506628277459239e+00,
        ];
        let b = [
            -5.447609879822406e+01,
            1.615858368580409e+02,
            -1.556989798598866e+02,
            6.680131188771972e+01,
            -1.328068155288572e+01,
        ];
        let c = [
            -7.784894002430293e-03,
            -3.223964580411365e-01,
            -2.400758277161838e+00,
            -2.549732539343734e+00,
            4.374664141464968e+00,
            2.938163982698783e+00,
        ];
        let d = [
            7.784695709041462e-03,
            3.224671290700398e-01,
            2.445134137142996e+00,
            3.754408661907416e+00,
        ];

        let p_low = 0.02425;
        let p_high = 1.0 - p_low;

        if p < p_low {
            let q = (-2.0 * p.ln()).sqrt();
            (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
                / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
        } else if p <= p_high {
            let q = p - 0.5;
            let r = q * q;
            (((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q
                / (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1.0)
        } else {
            let q = (-2.0 * (1.0 - p).ln()).sqrt();
            -(((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
                / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
        }
    }

    pub fn calculate_fair_yes_price(
        buffer: f64,
        volatility: f64,
        time_remaining_fraction: f64,
    ) -> f64 {
        if volatility <= 0.0 || time_remaining_fraction <= 0.0 {
            return if buffer > 0.0 { 1.0 } else { 0.0 };
        }

        let adjusted_vol = volatility * time_remaining_fraction.sqrt();
        if adjusted_vol < 1e-10 {
            return if buffer > 0.0 { 1.0 } else { 0.0 };
        }

        let d2 = buffer / adjusted_vol;
        let prob = norm_cdf(d2);
        prob.max(0.001).min(0.999)
    }

    pub fn calculate_implied_volatility(
        yes_price: f64,
        buffer: f64,
        time_remaining_fraction: f64,
    ) -> Option<f64> {
        if yes_price <= 0.0 || yes_price >= 1.0 || time_remaining_fraction <= 0.0 {
            return None;
        }
        if buffer.abs() < 1e-10 {
            return Some(0.003);
        }

        let d2_target = norm_inv(yes_price);
        if d2_target.abs() < 1e-10 {
            return Some(0.003);
        }

        let sqrt_t = time_remaining_fraction.sqrt();
        let initial_vol = (buffer / (d2_target * sqrt_t)).abs();
        let mut vol = initial_vol.max(0.0001).min(0.1);

        for _ in 0..20 {
            let adjusted_vol = vol * sqrt_t;
            if adjusted_vol < 1e-10 {
                break;
            }

            let d2 = buffer / adjusted_vol;
            let price = norm_cdf(d2);
            let error = price - yes_price;
            if error.abs() < 1e-8 {
                break;
            }

            let vega = -norm_pdf(d2) * d2 / vol;
            if vega.abs() < 1e-10 {
                break;
            }

            vol -= error / vega;
            vol = vol.max(0.0001).min(0.1);
        }

        Some(vol)
    }

    pub fn calculate_kelly_fraction(win_probability: f64, entry_price: f64) -> f64 {
        if entry_price <= 0.0 || entry_price >= 1.0 {
            return 0.0;
        }

        let p = win_probability;
        let q = 1.0 - p;
        let b = (1.0 - entry_price) / entry_price;
        let kelly = (p * b - q) / b;
        kelly.max(0.0)
    }
}

/// Volatility estimate with confidence
#[derive(Debug, Clone)]
pub struct VolatilityEstimate {
    pub kline_vol: f64,
    pub tick_vol: f64,
    pub combined_vol: f64,
    pub confidence: f64,
    pub sample_size: usize,
}

/// Market pricing information
#[derive(Debug, Clone)]
pub struct MarketPricing {
    pub yes_price: Decimal,
    pub no_price: Decimal,
    pub yes_ask: Decimal,
    pub yes_bid: Decimal,
    pub spread: Decimal,
    pub implied_vol: f64,
}

/// Trading signal from volatility arbitrage
#[derive(Debug, Clone)]
pub struct VolArbSignal {
    pub symbol: String,
    pub market_id: String,
    pub condition_id: String,
    pub buy_yes: bool,
    pub fair_value: Decimal,
    pub market_price: Decimal,
    pub price_edge: Decimal,
    pub vol_edge_pct: f64,
    pub position_size: u64,
    pub confidence: f64,
    pub time_remaining_secs: u64,
    pub spot_price: Decimal,
    pub threshold_price: Decimal,
    pub buffer_pct: Decimal,
    pub timestamp: DateTime<Utc>,
}

impl VolatilityArbEngine {
    pub fn estimate_volatility(
        &self,
        symbol: &str,
        tick_volatility: Option<f64>,
    ) -> VolatilityEstimate {
        let kline_vol = self.kline_vol_cache.get(symbol).copied().unwrap_or(0.003);
        let tick_vol = tick_volatility.unwrap_or(kline_vol);

        let weight_sum = self.config.kline_weight + self.config.tick_weight;
        let (wk, wt) = if weight_sum > 0.0 {
            (
                self.config.kline_weight / weight_sum,
                self.config.tick_weight / weight_sum,
            )
        } else {
            (0.5, 0.5)
        };
        let combined = (wk * kline_vol * kline_vol + wt * tick_vol * tick_vol).sqrt();

        let mut confidence = if self.kline_vol_cache.contains_key(symbol) {
            if tick_volatility.is_some() { 0.9 } else { 0.7 }
        } else if tick_volatility.is_some() {
            0.5
        } else {
            0.3
        };

        if tick_volatility.is_some() {
            let denom = combined.max(1e-9);
            let disagreement = ((kline_vol - tick_vol).abs() / denom).min(1.0);
            let agreement_factor = (1.0 - disagreement).clamp(0.3, 1.0);
            confidence *= agreement_factor;
        }

        VolatilityEstimate {
            kline_vol,
            tick_vol,
            combined_vol: combined,
            confidence: confidence.clamp(0.0, 1.0),
            sample_size: self.config.vol_lookback_periods,
        }
    }

    pub fn analyze_market(
        &self,
        symbol: &str,
        market_id: &str,
        condition_id: &str,
        spot_price: Decimal,
        threshold_price: Decimal,
        yes_price: Decimal,
        yes_ask: Decimal,
        time_remaining_secs: u64,
        tick_volatility: Option<f64>,
    ) -> Option<VolArbSignal> {
        if time_remaining_secs < self.config.min_time_remaining_secs {
            debug!(time_remaining_secs, "Too little time remaining");
            return None;
        }
        if time_remaining_secs > self.config.max_time_remaining_secs {
            debug!(time_remaining_secs, "Too much time remaining");
            return None;
        }

        if let Some(last_time) = self.last_trade_time.get(market_id) {
            let elapsed = Utc::now().signed_duration_since(*last_time).num_seconds() as u64;
            if elapsed < self.config.cooldown_secs {
                return None;
            }
        }

        let buffer_pct = if threshold_price > Decimal::ZERO {
            (spot_price - threshold_price) / threshold_price
        } else {
            return None;
        };
        if buffer_pct.abs() < self.config.min_buffer_pct {
            debug!(%buffer_pct, "Buffer too small (coin flip)");
            return None;
        }
        if buffer_pct.abs() > self.config.max_buffer_pct {
            debug!(%buffer_pct, "Buffer too large (outcome certain)");
            return None;
        }

        let vol_estimate = self.estimate_volatility(symbol, tick_volatility);
        let time_fraction = (time_remaining_secs as f64) / 900.0;
        let yes_price_f64 = yes_price.to_f64().unwrap_or(0.5);
        let buffer_f64 = buffer_pct.to_f64().unwrap_or(0.0);

        let implied_vol = calculate_implied_volatility(yes_price_f64, buffer_f64, time_fraction)?;
        let fair_value_f64 =
            calculate_fair_yes_price(buffer_f64, vol_estimate.combined_vol, time_fraction);
        let fair_value = Decimal::from_f64(fair_value_f64).unwrap_or(dec!(0.5));

        let vol_edge_pct = (vol_estimate.combined_vol - implied_vol).abs() / implied_vol;
        if vol_edge_pct < self.config.min_vol_edge_pct {
            debug!(
                vol_edge_pct,
                min = self.config.min_vol_edge_pct,
                "Insufficient vol edge"
            );
            return None;
        }

        let (buy_yes, price_edge, entry_price) = if vol_estimate.combined_vol < implied_vol {
            let edge = fair_value - yes_ask;
            (true, edge, yes_ask)
        } else {
            let no_price = Decimal::ONE - yes_price;
            let no_fair = Decimal::ONE - fair_value;
            let edge = no_fair - no_price;
            (false, edge, no_price)
        };

        let net_edge = price_edge - self.config.pm_fee_rate;
        if net_edge < self.config.min_price_edge {
            debug!(%net_edge, min = %self.config.min_price_edge, "Insufficient price edge");
            return None;
        }

        let time_confidence = if time_remaining_secs >= self.config.optimal_time_range.0
            && time_remaining_secs <= self.config.optimal_time_range.1
        {
            1.0
        } else {
            0.7
        };
        let confidence =
            (vol_estimate.confidence * time_confidence * (1.0 + vol_edge_pct)).min(1.0);

        let win_prob = if buy_yes {
            fair_value_f64
        } else {
            1.0 - fair_value_f64
        };
        let kelly = calculate_kelly_fraction(win_prob, entry_price.to_f64().unwrap_or(0.5));
        let mut adjusted_kelly = kelly * self.config.kelly_fraction * confidence;
        if vol_estimate.combined_vol > self.config.high_vol_threshold {
            adjusted_kelly *= self.config.high_vol_kelly_multiplier;
        }
        adjusted_kelly = adjusted_kelly.clamp(0.0, 1.0);

        let max_shares = (self.config.max_position_usd / entry_price)
            .to_u64()
            .unwrap_or(100);
        let kelly_shares = (adjusted_kelly * max_shares as f64).round() as u64;
        let position_size = kelly_shares.max(10).min(max_shares);

        info!(
            symbol,
            %buffer_pct,
            our_vol = vol_estimate.combined_vol,
            implied_vol,
            vol_edge_pct,
            %fair_value,
            %entry_price,
            %price_edge,
            buy_yes,
            position_size,
            confidence,
            "Volatility arbitrage signal"
        );

        Some(VolArbSignal {
            symbol: symbol.to_string(),
            market_id: market_id.to_string(),
            condition_id: condition_id.to_string(),
            buy_yes,
            fair_value,
            market_price: if buy_yes {
                yes_ask
            } else {
                Decimal::ONE - yes_price
            },
            price_edge,
            vol_edge_pct,
            position_size,
            confidence,
            time_remaining_secs,
            spot_price,
            threshold_price,
            buffer_pct,
            timestamp: Utc::now(),
        })
    }
}
