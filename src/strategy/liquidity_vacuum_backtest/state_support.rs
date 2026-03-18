use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::adapters::SpotPrice;

#[derive(Debug, Clone)]
pub(super) struct EmaState {
    alpha: Decimal,
    period: u64,
    value: Option<Decimal>,
    samples: u64,
}

impl EmaState {
    pub(super) fn new(period: u64) -> Self {
        let alpha = Decimal::from(2u64) / Decimal::from(period + 1);
        Self {
            alpha,
            period,
            value: None,
            samples: 0,
        }
    }

    pub(super) fn update(&mut self, price: Decimal) -> Decimal {
        self.samples += 1;
        let next = match self.value {
            Some(v) => self.alpha * price + (Decimal::ONE - self.alpha) * v,
            None => price,
        };
        self.value = Some(next);
        next
    }

    pub(super) fn warm_value(&self) -> Option<Decimal> {
        if self.samples >= self.period {
            self.value
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SymbolState {
    pub(super) spot: SpotPrice,
    pub(super) ema: EmaState,
    volume_samples: VecDeque<(DateTime<Utc>, Decimal)>,
    volume_window_history: VecDeque<Decimal>,
    flow_samples: VecDeque<(DateTime<Utc>, Decimal)>,
    deviation_samples: VecDeque<Decimal>,
    pub(super) latest_lob_depth: Option<u64>,
    last_volume_window_ts: Option<DateTime<Utc>>,
    pub(super) daily_trade_count: u32,
    daily_trade_date: Option<chrono::NaiveDate>,
}

impl SymbolState {
    pub(super) fn new(
        price: Decimal,
        quantity: Option<Decimal>,
        ts: DateTime<Utc>,
        ema_period: u64,
        default_qty: Decimal,
    ) -> Self {
        let mut ema = EmaState::new(ema_period);
        ema.update(price);

        let mut volume_samples = VecDeque::new();
        volume_samples.push_back((ts, quantity.unwrap_or(default_qty).max(Decimal::ZERO)));

        Self {
            spot: SpotPrice::new(price, quantity, ts),
            ema,
            volume_samples,
            volume_window_history: VecDeque::new(),
            flow_samples: VecDeque::new(),
            deviation_samples: VecDeque::new(),
            latest_lob_depth: None,
            last_volume_window_ts: None,
            daily_trade_count: 0,
            daily_trade_date: Some(ts.date_naive()),
        }
    }

    pub(super) fn update_spot(
        &mut self,
        price: Decimal,
        quantity: Option<Decimal>,
        ts: DateTime<Utc>,
        default_qty: Decimal,
    ) {
        self.spot.update(price, quantity, ts);
        self.ema.update(price);
        self.volume_samples
            .push_back((ts, quantity.unwrap_or(default_qty).max(Decimal::ZERO)));
    }

    pub(super) fn record_flow_sample(&mut self, ts: DateTime<Utc>, value: Decimal) {
        self.flow_samples.push_back((ts, value));
    }

    pub(super) fn update_lob_depth(&mut self, depth: u64) {
        self.latest_lob_depth = Some(depth);
    }

    pub(super) fn prune_old(
        &mut self,
        now: DateTime<Utc>,
        window_secs: u64,
        baseline_samples: usize,
    ) {
        let keep_cutoff = now - chrono::Duration::seconds((window_secs * 4) as i64);

        while let Some((ts, _)) = self.volume_samples.front() {
            if *ts < keep_cutoff {
                let _ = self.volume_samples.pop_front();
            } else {
                break;
            }
        }

        while let Some((ts, _)) = self.flow_samples.front() {
            if *ts < keep_cutoff {
                let _ = self.flow_samples.pop_front();
            } else {
                break;
            }
        }

        while self.volume_window_history.len() > baseline_samples + 2 {
            let _ = self.volume_window_history.pop_front();
        }
    }

    fn volume_in_window(&self, now: DateTime<Utc>, window_secs: u64) -> Decimal {
        let cutoff = now - chrono::Duration::seconds(window_secs as i64);
        self.volume_samples
            .iter()
            .filter(|(ts, _)| *ts >= cutoff)
            .map(|(_, q)| *q)
            .sum()
    }

    pub(super) fn maybe_sample_volume_window(
        &mut self,
        now: DateTime<Utc>,
        window_secs: u64,
        baseline_samples: usize,
    ) -> Decimal {
        let current = self.volume_in_window(now, window_secs);
        let should_sample = self
            .last_volume_window_ts
            .map(|ts| (now - ts).num_seconds() >= 1)
            .unwrap_or(true);
        if should_sample {
            self.volume_window_history.push_back(current);
            self.last_volume_window_ts = Some(now);
            while self.volume_window_history.len() > baseline_samples + 2 {
                let _ = self.volume_window_history.pop_front();
            }
        }
        current
    }

    pub(super) fn volume_ratio(&self) -> Option<Decimal> {
        if self.volume_window_history.len() < 10 {
            return None;
        }
        let latest = *self.volume_window_history.back()?;
        let baseline_count = self.volume_window_history.len().saturating_sub(1);
        if baseline_count == 0 {
            return None;
        }
        let baseline_sum: Decimal = self
            .volume_window_history
            .iter()
            .take(baseline_count)
            .copied()
            .sum();
        let baseline = baseline_sum / Decimal::from(baseline_count as u64);
        if baseline <= Decimal::ZERO {
            return None;
        }
        Some(latest / baseline)
    }

    pub(super) fn flow_component(&self, now: DateTime<Utc>, window_secs: u64) -> Option<Decimal> {
        let cutoff = now - chrono::Duration::seconds(window_secs as i64);
        let mut sum = Decimal::ZERO;
        let mut n: u64 = 0;
        for (ts, value) in &self.flow_samples {
            if *ts >= cutoff {
                sum += *value;
                n += 1;
            }
        }
        if n == 0 {
            None
        } else {
            Some(sum / Decimal::from(n))
        }
    }

    pub(super) fn record_deviation_sample(
        &mut self,
        signed_deviation: Decimal,
        lookback_samples: usize,
    ) -> Option<Decimal> {
        let cap = lookback_samples.max(2);
        self.deviation_samples.push_back(signed_deviation);
        while self.deviation_samples.len() > cap {
            let _ = self.deviation_samples.pop_front();
        }
        self.latest_deviation_zscore()
    }

    pub(super) fn latest_deviation_zscore(&self) -> Option<Decimal> {
        compute_abs_zscore(&self.deviation_samples)
    }

    pub(super) fn reset_daily_counter_if_needed(&mut self, now: DateTime<Utc>) {
        let d = now.date_naive();
        if self.daily_trade_date != Some(d) {
            self.daily_trade_count = 0;
            self.daily_trade_date = Some(d);
        }
    }
}

fn compute_abs_zscore(samples: &VecDeque<Decimal>) -> Option<Decimal> {
    if samples.len() < 30 {
        return None;
    }
    let values: Vec<f64> = samples
        .iter()
        .map(|v| v.to_f64())
        .collect::<Option<Vec<_>>>()?;
    let n = values.len() as f64;
    if n <= 1.0 {
        return None;
    }
    let mean = values.iter().sum::<f64>() / n;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
    if variance <= 1e-12 {
        return None;
    }
    let std = variance.sqrt();
    let latest = *values.last()?;
    Decimal::from_f64(((latest - mean) / std).abs())
}

pub(super) fn clamp_decimal(v: Decimal, min_v: Decimal, max_v: Decimal) -> Decimal {
    if v < min_v {
        min_v
    } else if v > max_v {
        max_v
    } else {
        v
    }
}

pub(super) fn fair_up_probability_from_spot(spot: Decimal, strike: Decimal) -> Decimal {
    if strike <= Decimal::ZERO {
        return dec!(0.5);
    }
    let rel_move = (spot - strike) / strike;
    let scale = dec!(0.02);
    let p = dec!(0.5) + rel_move * dec!(0.5) / scale;
    clamp_decimal(p, dec!(0.01), dec!(0.99))
}

pub(super) fn is_valid_binary_quote_price(px: Decimal) -> bool {
    px >= dec!(0.01) && px <= dec!(0.99)
}

#[cfg(test)]
mod tests {
    use super::{compute_abs_zscore, fair_up_probability_from_spot, is_valid_binary_quote_price};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::collections::VecDeque;

    #[test]
    fn test_binary_quote_price_bounds() {
        assert!(is_valid_binary_quote_price(dec!(0.01)));
        assert!(is_valid_binary_quote_price(dec!(0.99)));
        assert!(!is_valid_binary_quote_price(dec!(0.009)));
        assert!(!is_valid_binary_quote_price(dec!(1.0)));
    }

    #[test]
    fn test_fair_up_probability_clamps_extremes() {
        assert_eq!(
            fair_up_probability_from_spot(dec!(200), dec!(100)),
            dec!(0.99)
        );
        assert_eq!(
            fair_up_probability_from_spot(dec!(1), dec!(100)),
            dec!(0.01)
        );
        assert_eq!(
            fair_up_probability_from_spot(dec!(100), Decimal::ZERO),
            dec!(0.5)
        );
    }

    #[test]
    fn test_abs_zscore_requires_variation_and_enough_samples() {
        let mut samples = VecDeque::new();
        for i in 0..29 {
            samples.push_back(Decimal::from(i));
        }
        assert_eq!(compute_abs_zscore(&samples), None);

        samples.push_back(dec!(100));
        assert!(compute_abs_zscore(&samples).is_some());
    }
}
