//! Core pattern-memory engine (no market IO).

use chrono::{DateTime, Utc};
use std::collections::VecDeque;

/// Pearson correlation in [-1, 1]. Returns 0.0 on degenerate input.
pub fn pearson_corr(x: &[f64], y: &[f64]) -> f64 {
    if x.len() != y.len() || x.len() < 2 {
        return 0.0;
    }

    let n = x.len() as f64;
    let mean_x = x.iter().sum::<f64>() / n;
    let mean_y = y.iter().sum::<f64>() / n;

    let mut cov = 0.0;
    let mut var_x = 0.0;
    let mut var_y = 0.0;
    for (&xi, &yi) in x.iter().zip(y.iter()) {
        let dx = xi - mean_x;
        let dy = yi - mean_y;
        cov += dx * dy;
        var_x += dx * dx;
        var_y += dy * dy;
    }

    if var_x == 0.0 || var_y == 0.0 {
        return 0.0;
    }

    let denom = (var_x * var_y).sqrt();
    if denom == 0.0 {
        return 0.0;
    }

    (cov / denom).clamp(-1.0, 1.0)
}

/// Map a correlation threshold match to a weight in [0, 1].
///
/// Default: `w = clamp((corr - thr) / (1 - thr), 0..1)`.
pub fn corr_weight(corr: f64, thr: f64) -> f64 {
    if corr <= thr {
        return 0.0;
    }
    let denom = 1.0 - thr;
    if denom <= 0.0 {
        return 0.0;
    }
    ((corr - thr) / denom).clamp(0.0, 1.0)
}

/// Beta posterior for a Bernoulli probability with (possibly weighted) counts.
pub fn beta_posterior(alpha: f64, beta: f64, up_w: f64, down_w: f64) -> f64 {
    let denom = alpha + beta + up_w + down_w;
    if denom <= 0.0 {
        return 0.5;
    }
    ((alpha + up_w) / denom).clamp(0.0, 1.0)
}

/// Objective B label: resolves UP only when next return strictly exceeds required return.
pub fn classify_up(next_return: f64, required_return: f64) -> bool {
    next_return > required_return
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PatternSample<const N: usize> {
    pub pattern: [f64; N],
    pub next_return: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Posterior {
    pub p_up: f64,
    pub up_w: f64,
    pub down_w: f64,
    pub n_eff: f64,
    pub matches: usize,
}

/// Maintains a rolling return window and a library of historical patterns with realized next returns.
pub struct PatternMemory<const N: usize> {
    returns: VecDeque<f64>,
    pending_pattern: Option<[f64; N]>,
    pending_timestamp: Option<DateTime<Utc>>,
    samples: Vec<PatternSample<N>>,
    max_samples: usize,
}

/// Default max samples (~7 days of 5m bars).
const DEFAULT_MAX_SAMPLES: usize = 2000;

impl<const N: usize> PatternMemory<N> {
    pub fn new() -> Self {
        Self {
            returns: VecDeque::with_capacity(N),
            pending_pattern: None,
            pending_timestamp: None,
            samples: Vec::new(),
            max_samples: DEFAULT_MAX_SAMPLES,
        }
    }

    pub fn with_max_samples(mut self, max: usize) -> Self {
        self.max_samples = max;
        self
    }

    pub fn samples_len(&self) -> usize {
        self.samples.len()
    }

    pub fn samples(&self) -> &[PatternSample<N>] {
        &self.samples
    }

    pub fn push_sample(&mut self, sample: PatternSample<N>) {
        self.samples.push(sample);
    }

    /// Returns the current pattern (last N returns), oldest→newest.
    pub fn current_pattern(&self) -> Option<[f64; N]> {
        if self.returns.len() != N {
            return None;
        }
        let mut arr = [0.0_f64; N];
        for (i, v) in self.returns.iter().enumerate() {
            arr[i] = *v;
        }
        Some(arr)
    }

    /// Ingest a newly closed-bar return for this timeframe.
    ///
    /// The incoming return labels the previous pending pattern as its realized `next_return`.
    pub fn ingest_return(&mut self, r: f64, now: DateTime<Utc>) {
        if let Some(p) = self.pending_pattern.take() {
            let ts = self.pending_timestamp.unwrap_or(now);
            self.samples.push(PatternSample {
                pattern: p,
                next_return: r,
                timestamp: ts,
            });
            // Evict oldest if over capacity
            if self.samples.len() > self.max_samples {
                self.samples.remove(0);
            }
        }

        self.returns.push_back(r);
        while self.returns.len() > N {
            self.returns.pop_front();
        }

        self.pending_pattern = self.current_pattern();
        self.pending_timestamp = Some(now);
    }

    pub fn posterior_for_required_return(
        &self,
        required_return: f64,
        corr_threshold: f64,
        alpha: f64,
        beta: f64,
        age_decay_lambda: f64,
    ) -> Posterior {
        let now = Utc::now();
        let Some(cur) = self.current_pattern() else {
            let p_up = beta_posterior(alpha, beta, 0.0, 0.0);
            return Posterior {
                p_up,
                up_w: 0.0,
                down_w: 0.0,
                n_eff: 0.0,
                matches: 0,
            };
        };

        let cur_slice: &[f64] = &cur;
        let mut up_w: f64 = 0.0;
        let mut down_w: f64 = 0.0;
        let mut matches: usize = 0;

        for s in &self.samples {
            let corr = pearson_corr(cur_slice, &s.pattern);
            if corr < corr_threshold {
                continue;
            }
            matches = matches.saturating_add(1);
            let cw = corr_weight(corr, corr_threshold);
            if cw <= 0.0 {
                continue;
            }
            // Time decay: w = corr_weight * exp(-lambda * age_minutes)
            let decay = if age_decay_lambda > 0.0 {
                let age_min = now
                    .signed_duration_since(s.timestamp)
                    .num_minutes()
                    .max(0) as f64;
                (-age_decay_lambda * age_min).exp()
            } else {
                1.0
            };
            let w = cw * decay;
            if classify_up(s.next_return, required_return) {
                up_w += w;
            } else {
                down_w += w;
            }
        }

        let n_eff = up_w + down_w;
        let p_up = beta_posterior(alpha, beta, up_w, down_w);

        Posterior {
            p_up,
            up_w,
            down_w,
            n_eff,
            matches,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pearson_corr_identical_is_oneish() {
        let x = [0.0, 1.0, 2.0, 3.0];
        let y = [0.0, 1.0, 2.0, 3.0];
        let c = pearson_corr(&x, &y);
        assert!((c - 1.0).abs() < 1e-9, "corr={}", c);
    }

    #[test]
    fn pearson_corr_inverse_is_negative_oneish() {
        let x = [0.0, 1.0, 2.0, 3.0];
        let y = [3.0, 2.0, 1.0, 0.0];
        let c = pearson_corr(&x, &y);
        assert!((c + 1.0).abs() < 1e-9, "corr={}", c);
    }

    #[test]
    fn pearson_corr_constant_returns_zero() {
        let x = [1.0, 1.0, 1.0, 1.0];
        let y = [0.0, 1.0, 2.0, 3.0];
        let c = pearson_corr(&x, &y);
        assert_eq!(c, 0.0);
    }

    #[test]
    fn corr_weight_clamps_to_unit_interval() {
        let thr = 0.7;
        assert_eq!(corr_weight(0.0, thr), 0.0);
        assert_eq!(corr_weight(thr, thr), 0.0);
        assert!((corr_weight(1.0, thr) - 1.0).abs() < 1e-12);
        assert_eq!(corr_weight(-1.0, thr), 0.0);
    }

    #[test]
    fn beta_posterior_matches_hand_calc() {
        // Uniform prior Beta(1,1). Up_w=3, down_w=1 => (1+3)/(1+1+4)=4/6.
        let p = beta_posterior(1.0, 1.0, 3.0, 1.0);
        assert!((p - (4.0 / 6.0)).abs() < 1e-12, "p={}", p);
    }

    #[test]
    fn classify_strictly_greater_than_required_return() {
        let r_req = 0.01;
        assert!(classify_up(0.0100000001, r_req));
        assert!(!classify_up(0.01, r_req));
        assert!(!classify_up(-0.5, r_req));
    }

    #[test]
    fn pattern_memory_labels_previous_pattern_with_next_return() {
        let now = Utc::now();
        let mut mem = PatternMemory::<3>::new();
        mem.ingest_return(0.1, now);
        mem.ingest_return(0.2, now);
        assert_eq!(mem.samples_len(), 0);

        // First time we have a full pattern, it becomes pending (no label yet).
        mem.ingest_return(0.3, now);
        assert_eq!(mem.samples_len(), 0);
        assert_eq!(mem.current_pattern().unwrap(), [0.1, 0.2, 0.3]);

        // Next return labels the previous pending pattern.
        mem.ingest_return(0.4, now);
        assert_eq!(mem.samples_len(), 1);
        let s0 = mem.samples()[0];
        assert_eq!(s0.pattern, [0.1, 0.2, 0.3]);
        assert!((s0.next_return - 0.4).abs() < 1e-12);
        assert_eq!(mem.current_pattern().unwrap(), [0.2, 0.3, 0.4]);
    }

    #[test]
    fn posterior_uses_required_return_threshold() {
        let now = Utc::now();
        let mut mem = PatternMemory::<3>::new();
        // Build a single stored sample with pattern [0.1,0.2,0.3] and next_return 0.4
        mem.ingest_return(0.1, now);
        mem.ingest_return(0.2, now);
        mem.ingest_return(0.3, now); // pending
        mem.ingest_return(0.4, now); // labels pending, stores sample

        // Current pattern is [0.2,0.3,0.4]. Add one more return so pending/current becomes [0.1,0.2,0.3].
        // We want current pattern to match the stored one for corr ~ 1.
        let mut mem2 = PatternMemory::<3>::new();
        mem2.ingest_return(0.1, now);
        mem2.ingest_return(0.2, now);
        mem2.ingest_return(0.3, now);
        // At this point current pattern == [0.1,0.2,0.3], but there is no stored sample yet.
        // Copy the stored sample from `mem`.
        mem2.push_sample(mem.samples()[0]);

        let post = mem2.posterior_for_required_return(0.0, 0.7, 1.0, 1.0, 0.0);
        // One matched sample, weight=1, next_return=0.4 > 0 => up_w=1.
        assert!((post.p_up - (2.0 / 3.0)).abs() < 1e-9, "p_up={}", post.p_up);
        assert!((post.n_eff - 1.0).abs() < 1e-12);

        // If required return is higher than next_return, it should count as DOWN.
        let post2 = mem2.posterior_for_required_return(0.5, 0.7, 1.0, 1.0, 0.0);
        assert!(
            (post2.p_up - (1.0 / 3.0)).abs() < 1e-9,
            "p_up={}",
            post2.p_up
        );
    }
}
