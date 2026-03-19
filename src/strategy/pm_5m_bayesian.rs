//! Bayesian win-probability estimator for pm_5m_directional
//!
//! Maintains a Beta(α, β) posterior per market-condition bucket.
//! Each bucket is keyed by (price_band, time_band, vol_regime).
//!
//! ## How it integrates with the strategy
//!
//! 1. Before entry: `posterior_lower_bound()` returns the 5th-percentile of
//!    the posterior. The strategy can require this to exceed `p_entry` instead
//!    of (or in addition to) the raw p_hat. This prevents overconfidence when
//!    the bucket has few observations.
//!
//! 2. After settlement: `record_outcome()` updates α or β for the bucket.
//!    The posterior converges toward the true win rate as trades accumulate.
//!
//! ## Bucket design
//!
//! - price_band: 5-cent buckets (0.30–0.35, 0.35–0.40, …, 0.70–0.75)
//! - time_band: early (>180s), mid (60–180s), late (<60s)
//! - vol_regime: low (<0.0008), normal (0.0008–0.002), high (>0.002)
//!
//! Buckets with fewer than `min_obs` observations fall back to a
//! weakly-informative prior Beta(2, 2) (mean=0.5, std=0.22).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Minimum observations before the posterior is trusted over the prior
const MIN_OBS: u32 = 5;
/// Weakly-informative prior: Beta(2,2) → mean=0.5
const PRIOR_ALPHA: f64 = 2.0;
const PRIOR_BETA: f64 = 2.0;

/// Condition bucket key
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BucketKey {
    /// 5-cent price band index: floor(price / 0.05) clamped to [6, 14]
    pub price_band: u8,
    /// 0=late(<60s), 1=mid(60-180s), 2=early(>180s)
    pub time_band: u8,
    /// 0=low, 1=normal, 2=high
    pub vol_regime: u8,
}

impl BucketKey {
    pub fn from_conditions(price: f64, time_remaining_secs: f64, sigma: f64) -> Self {
        let price_band = ((price / 0.05).floor() as u8).clamp(6, 14);
        let time_band = if time_remaining_secs > 180.0 {
            2
        } else if time_remaining_secs > 60.0 {
            1
        } else {
            0
        };
        let vol_regime = if sigma < 0.0008 {
            0
        } else if sigma < 0.002 {
            1
        } else {
            2
        };
        Self {
            price_band,
            time_band,
            vol_regime,
        }
    }
}

/// Beta distribution posterior for one bucket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BetaPosterior {
    pub alpha: f64,
    pub beta: f64,
    pub observations: u32,
}

impl BetaPosterior {
    fn new() -> Self {
        Self {
            alpha: PRIOR_ALPHA,
            beta: PRIOR_BETA,
            observations: 0,
        }
    }

    fn record_win(&mut self) {
        self.alpha += 1.0;
        self.observations += 1;
    }

    fn record_loss(&mut self) {
        self.beta += 1.0;
        self.observations += 1;
    }

    /// Posterior mean: E[p] = α / (α + β)
    pub fn mean(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }

    /// Posterior variance: Var[p] = αβ / ((α+β)²(α+β+1))
    pub fn variance(&self) -> f64 {
        let n = self.alpha + self.beta;
        (self.alpha * self.beta) / (n * n * (n + 1.0))
    }

    /// Posterior std deviation
    pub fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }

    /// Conservative lower bound: mean - z_alpha * std
    /// z=1.645 → 5th percentile (one-sided 95% credible lower bound)
    pub fn lower_bound(&self, z: f64) -> f64 {
        (self.mean() - z * self.std_dev()).max(0.0)
    }

    /// Whether this bucket has enough data to be trusted
    pub fn is_mature(&self) -> bool {
        self.observations >= MIN_OBS
    }
}

/// Bayesian win-probability tracker across all condition buckets
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BayesianPrior {
    buckets: HashMap<BucketKey, BetaPosterior>,
}

impl BayesianPrior {
    pub fn new() -> Self {
        Self::default()
    }

    fn bucket_mut(&mut self, key: &BucketKey) -> &mut BetaPosterior {
        self.buckets
            .entry(key.clone())
            .or_insert_with(BetaPosterior::new)
    }

    fn bucket(&self, key: &BucketKey) -> Option<&BetaPosterior> {
        self.buckets.get(key)
    }

    /// Record a trade outcome for the given conditions
    pub fn record_outcome(&mut self, price: f64, time_remaining_secs: f64, sigma: f64, won: bool) {
        let key = BucketKey::from_conditions(price, time_remaining_secs, sigma);
        let bucket = self.bucket_mut(&key);
        if won {
            bucket.record_win();
        } else {
            bucket.record_loss();
        }
    }

    /// Returns the posterior lower bound (5th percentile) for the given conditions.
    ///
    /// If the bucket is immature (< MIN_OBS), blends the prior mean with the
    /// model's p_hat using a weight proportional to observations.
    pub fn posterior_lower_bound(
        &self,
        price: f64,
        time_remaining_secs: f64,
        sigma: f64,
        model_p_hat: f64,
        z: f64,
    ) -> f64 {
        let key = BucketKey::from_conditions(price, time_remaining_secs, sigma);
        match self.bucket(&key) {
            Some(b) if b.is_mature() => b.lower_bound(z),
            Some(b) => {
                // Blend: weight toward model_p_hat when few observations
                let obs_weight = b.observations as f64 / MIN_OBS as f64;
                let bayes_lb = b.lower_bound(z);
                obs_weight * bayes_lb + (1.0 - obs_weight) * model_p_hat
            }
            None => model_p_hat, // no data yet, trust the model
        }
    }

    /// Posterior mean for the bucket (for logging/debugging)
    pub fn posterior_mean(&self, price: f64, time_remaining_secs: f64, sigma: f64) -> f64 {
        let key = BucketKey::from_conditions(price, time_remaining_secs, sigma);
        self.bucket(&key)
            .map(|b| b.mean())
            .unwrap_or(PRIOR_ALPHA / (PRIOR_ALPHA + PRIOR_BETA))
    }

    /// Number of observations in the bucket
    pub fn bucket_obs(&self, price: f64, time_remaining_secs: f64, sigma: f64) -> u32 {
        let key = BucketKey::from_conditions(price, time_remaining_secs, sigma);
        self.bucket(&key).map(|b| b.observations).unwrap_or(0)
    }

    /// Total observations across all buckets
    pub fn total_observations(&self) -> u32 {
        self.buckets.values().map(|b| b.observations).sum()
    }

    /// Summary of all mature buckets for diagnostics
    pub fn mature_buckets(&self) -> Vec<(BucketKey, f64, u32)> {
        self.buckets
            .iter()
            .filter(|(_, b)| b.is_mature())
            .map(|(k, b)| (k.clone(), b.mean(), b.observations))
            .collect()
    }
}
