//! Dynamic Fee Model
//!
//! Polymarket's actual parabolic fee curve for binary markets.
//! Fee formula: `shares * fee_rate * (p * (1 - p))^exponent`
//!
//! This replaces the flat-rate assumption in `trading_costs.rs` for
//! cost estimation. The fee curve produces zero fees at p=0 and p=1
//! (settled markets) and maximum fees at p=0.50 (maximum uncertainty).
//!
//! # Domain-specific parameters
//! - Crypto (5m/15m markets): fee_rate=0.25, exponent=2
//! - Sports: fee_rate=0.0175, exponent=1

use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use crate::error::{PloyError, Result};

// ---------------------------------------------------------------------------
// Fee model
// ---------------------------------------------------------------------------

/// Parabolic fee model matching Polymarket's actual fee curve.
///
/// `effective_rate(p) = fee_rate * (p * (1 - p))^exponent`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeModel {
    /// Base fee rate coefficient (e.g. 0.25 for crypto)
    pub fee_rate: Decimal,
    /// Exponent applied to `p * (1 - p)` (e.g. 2 for crypto)
    pub exponent: u32,
}

/// Breakdown of all-in trading cost at a given price point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllInCost {
    /// Effective taker fee rate from the parabolic curve
    pub taker_fee: Decimal,
    /// Half-spread cost: `(ask - bid) / 2`
    pub spread_cost: Decimal,
    /// Estimated slippage from L2 book depth
    pub depth_slippage: Decimal,
    /// Sum of all components
    pub total: Decimal,
}

impl FeeModel {
    /// Crypto 5m/15m market parameters (fee_rate=0.25, exponent=2).
    pub fn crypto() -> Self {
        FeeModel {
            fee_rate: dec!(0.25),
            exponent: 2,
        }
    }

    /// Sports market parameters (fee_rate=0.0175, exponent=1).
    pub fn sports() -> Self {
        FeeModel {
            fee_rate: dec!(0.0175),
            exponent: 1,
        }
    }

    /// Fee in shares for buying `shares` at price `p`.
    ///
    /// Formula: `shares * fee_rate * (p * (1 - p))^exponent`
    pub fn fee_shares(&self, shares: Decimal, price: Decimal) -> Decimal {
        let p_factor = price * (Decimal::ONE - price);
        let p_powered = match self.exponent {
            1 => p_factor,
            2 => p_factor * p_factor,
            n => p_factor.powd(Decimal::from(n)),
        };
        shares * self.fee_rate * p_powered
    }

    /// Effective fee rate at price `p` (ranges from 0.0 to ~0.016 for crypto).
    ///
    /// Formula: `fee_rate * (p * (1 - p))^exponent`
    pub fn effective_rate(&self, price: Decimal) -> Decimal {
        let p_factor = price * (Decimal::ONE - price);
        match self.exponent {
            1 => self.fee_rate * p_factor,
            2 => self.fee_rate * p_factor * p_factor,
            n => self.fee_rate * p_factor.powd(Decimal::from(n)),
        }
    }

    /// All-in cost given current book state.
    ///
    /// `depth_ratio` is `order_size / best_level_size` — used for a simple
    /// linear slippage estimate (0.5% per 100% depth ratio).
    pub fn all_in_cost(
        &self,
        price: Decimal,
        best_bid: Decimal,
        best_ask: Decimal,
        depth_ratio: Decimal,
    ) -> AllInCost {
        let taker_fee = self.effective_rate(price);
        let spread_cost = (best_ask - best_bid) / dec!(2);
        // Simple linear slippage model: 0.5% per 100% depth ratio
        let depth_slippage = depth_ratio * dec!(0.005);
        AllInCost {
            taker_fee,
            spread_cost,
            depth_slippage,
            total: taker_fee + spread_cost + depth_slippage,
        }
    }
}

// ---------------------------------------------------------------------------
// Fee rate API cache
// ---------------------------------------------------------------------------

/// Cached fee-rate fetcher for runtime validation against Polymarket's API.
///
/// Caches `fee_rate_bps` per token_id with a configurable TTL (default 5 min).
pub struct FeeRateCache {
    /// token_id -> (fee_rate_bps, fetched_at)
    cache: Arc<RwLock<HashMap<String, (u64, Instant)>>>,
    ttl: Duration,
}

impl FeeRateCache {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            ttl: Duration::from_secs(300), // 5 minute TTL
        }
    }

    /// Fetch `fee_rate_bps` for a token, using cache when available.
    pub async fn get_fee_rate_bps(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        token_id: &str,
    ) -> Result<u64> {
        // Check cache first
        {
            let cache = self.cache.read().await;
            if let Some((bps, fetched_at)) = cache.get(token_id) {
                if fetched_at.elapsed() < self.ttl {
                    return Ok(*bps);
                }
            }
        }

        // Cache miss or expired — fetch from API
        let bps = fetch_fee_rate_bps(client, base_url, token_id).await?;

        // Update cache
        {
            let mut cache = self.cache.write().await;
            cache.insert(token_id.to_string(), (bps, Instant::now()));
        }

        Ok(bps)
    }
}

/// Response shape from Polymarket fee-rate endpoint.
#[derive(Deserialize)]
struct FeeRateResponse {
    fee_rate_bps: u64,
}

/// Raw API call: `GET {base_url}/fee-rate?token_id={id}`
pub async fn fetch_fee_rate_bps(
    client: &reqwest::Client,
    base_url: &str,
    token_id: &str,
) -> Result<u64> {
    let url = format!("{}/fee-rate?token_id={}", base_url, token_id);
    let resp: FeeRateResponse = client
        .get(&url)
        .send()
        .await?
        .error_for_status()
        .map_err(PloyError::Http)?
        .json()
        .await?;
    Ok(resp.fee_rate_bps)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crypto_fee_at_50_cents() {
        let model = FeeModel::crypto();
        let rate = model.effective_rate(dec!(0.50));
        // 0.25 * (0.5 * 0.5)^2 = 0.25 * 0.0625 = 0.015625
        assert!((rate - dec!(0.015625)).abs() < dec!(0.0001));
    }

    #[test]
    fn test_crypto_fee_at_10_cents() {
        let model = FeeModel::crypto();
        let rate = model.effective_rate(dec!(0.10));
        // 0.25 * (0.1 * 0.9)^2 = 0.25 * 0.0081 = 0.002025
        assert!((rate - dec!(0.002025)).abs() < dec!(0.0001));
    }

    #[test]
    fn test_crypto_fee_shares() {
        let model = FeeModel::crypto();
        let fee = model.fee_shares(dec!(100), dec!(0.50));
        // 100 * 0.015625 = 1.5625
        assert!((fee - dec!(1.5625)).abs() < dec!(0.01));
    }

    #[test]
    fn test_sports_fee_at_50_cents() {
        let model = FeeModel::sports();
        let rate = model.effective_rate(dec!(0.50));
        // 0.0175 * (0.5 * 0.5)^1 = 0.0175 * 0.25 = 0.004375
        assert!((rate - dec!(0.004375)).abs() < dec!(0.0001));
    }

    #[test]
    fn test_fee_at_extremes() {
        let model = FeeModel::crypto();
        // At p=0 or p=1, fee should be 0
        assert_eq!(model.effective_rate(dec!(0)), Decimal::ZERO);
        assert_eq!(model.effective_rate(dec!(1)), Decimal::ZERO);
    }

    #[test]
    fn test_all_in_cost() {
        let model = FeeModel::crypto();
        let cost = model.all_in_cost(dec!(0.35), dec!(0.33), dec!(0.37), dec!(0.5));
        assert!(cost.taker_fee > Decimal::ZERO);
        assert_eq!(cost.spread_cost, dec!(0.02)); // (0.37 - 0.33) / 2
        assert!(cost.total > cost.taker_fee);
    }

    #[test]
    fn test_all_in_cost_components_sum() {
        let model = FeeModel::crypto();
        let cost = model.all_in_cost(dec!(0.50), dec!(0.48), dec!(0.52), dec!(0.3));
        assert_eq!(cost.total, cost.taker_fee + cost.spread_cost + cost.depth_slippage);
    }

    #[test]
    fn test_fee_symmetry() {
        // Fee at p should equal fee at (1-p) — the curve is symmetric
        let model = FeeModel::crypto();
        let rate_20 = model.effective_rate(dec!(0.20));
        let rate_80 = model.effective_rate(dec!(0.80));
        assert_eq!(rate_20, rate_80);
    }

    #[test]
    fn test_fee_maximum_at_50() {
        // p=0.50 should give the maximum fee rate
        let model = FeeModel::crypto();
        let rate_50 = model.effective_rate(dec!(0.50));
        let rate_30 = model.effective_rate(dec!(0.30));
        let rate_70 = model.effective_rate(dec!(0.70));
        assert!(rate_50 > rate_30);
        assert!(rate_50 > rate_70);
    }
}
