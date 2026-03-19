//! Rolling spot-price cache and statistics for the Binance spot adapter.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tokio::sync::RwLock;

/// How many price samples to keep per symbol.
///
/// We store at most one sample per second (see `SpotPrice::update`), so this also bounds
/// the maximum lookback window in seconds. Keep enough history for 15m window returns.
const MAX_PRICE_HISTORY: usize = 5_000;

/// Spot price with historical data for momentum calculation.
#[derive(Debug, Clone)]
pub struct SpotPrice {
    pub price: Decimal,
    pub timestamp: DateTime<Utc>,
    /// Price history for momentum calculation (newest first)
    /// Stores (price, quantity, timestamp). Quantity is the Binance trade size when available.
    history: VecDeque<(Decimal, Option<Decimal>, DateTime<Utc>)>,
}

impl SpotPrice {
    pub fn new(price: Decimal, quantity: Option<Decimal>, timestamp: DateTime<Utc>) -> Self {
        let mut history = VecDeque::with_capacity(MAX_PRICE_HISTORY);
        history.push_front((price, quantity, timestamp));
        Self {
            price,
            timestamp,
            history,
        }
    }

    /// Update with new price, maintaining history.
    pub fn update(&mut self, price: Decimal, quantity: Option<Decimal>, timestamp: DateTime<Utc>) {
        self.price = price;
        self.timestamp = timestamp;
        if let Some((front_price, front_qty, front_ts)) = self.history.front_mut() {
            if front_ts.timestamp() == timestamp.timestamp() {
                *front_price = price;
                *front_ts = timestamp;
                let prev = front_qty.take();
                *front_qty = match (prev, quantity) {
                    (Some(a), Some(b)) => Some(a + b),
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                };
                return;
            }
        }

        self.history.push_front((price, quantity, timestamp));
        while self.history.len() > MAX_PRICE_HISTORY {
            self.history.pop_back();
        }
    }

    pub fn price_secs_ago(&self, secs: u64) -> Option<Decimal> {
        let target_time = self.timestamp - chrono::Duration::seconds(secs as i64);
        for (price, _qty, ts) in &self.history {
            if *ts <= target_time {
                return Some(*price);
            }
        }
        self.history.back().map(|(p, _, _)| *p)
    }

    pub fn oldest_timestamp(&self) -> Option<DateTime<Utc>> {
        self.history.back().map(|(_, _, ts)| *ts)
    }

    pub fn momentum(&self, lookback_secs: u64) -> Option<Decimal> {
        let past_price = self.price_secs_ago(lookback_secs)?;
        if past_price.is_zero() {
            return None;
        }
        Some((self.price - past_price) / past_price)
    }

    pub fn price_1s_ago(&self) -> Option<Decimal> {
        self.price_secs_ago(1)
    }

    pub fn price_5s_ago(&self) -> Option<Decimal> {
        self.price_secs_ago(5)
    }

    pub fn price_15s_ago(&self) -> Option<Decimal> {
        self.price_secs_ago(15)
    }

    pub fn weighted_momentum(&self) -> Option<Decimal> {
        let mom_10s = self.momentum(10)?;
        let mom_30s = self.momentum(30)?;
        let mom_60s = self.momentum(60)?;
        let weighted = mom_10s * Decimal::new(2, 1)
            + mom_30s * Decimal::new(3, 1)
            + mom_60s * Decimal::new(5, 1);
        Some(weighted)
    }

    pub fn weighted_momentum_custom(
        &self,
        w_10s: Decimal,
        w_30s: Decimal,
        w_60s: Decimal,
    ) -> Option<Decimal> {
        let mom_10s = self.momentum(10)?;
        let mom_30s = self.momentum(30)?;
        let mom_60s = self.momentum(60)?;
        Some(mom_10s * w_10s + mom_30s * w_30s + mom_60s * w_60s)
    }

    pub fn volatility(&self, lookback_secs: u64) -> Option<Decimal> {
        if self.history.len() < 10 {
            return None;
        }

        let cutoff_time = self.timestamp - chrono::Duration::seconds(lookback_secs as i64);
        let prices: Vec<Decimal> = self
            .history
            .iter()
            .filter(|(_, _, ts)| *ts >= cutoff_time)
            .map(|(p, _, _)| *p)
            .collect();

        if prices.len() < 5 {
            return None;
        }

        let mut returns = Vec::with_capacity(prices.len() - 1);
        for i in 0..prices.len() - 1 {
            if !prices[i + 1].is_zero() {
                let ret = (prices[i] - prices[i + 1]) / prices[i + 1];
                returns.push(ret);
            }
        }

        if returns.is_empty() {
            return None;
        }

        let sum: Decimal = returns.iter().copied().sum();
        let mean = sum / Decimal::from(returns.len());
        let variance_sum: Decimal = returns
            .iter()
            .map(|r| {
                let diff = *r - mean;
                diff * diff
            })
            .sum();
        let variance = variance_sum / Decimal::from(returns.len());
        decimal_sqrt(variance)
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    pub fn vwap(&self, lookback_secs: u64) -> Option<Decimal> {
        let cutoff_time = self.timestamp - chrono::Duration::seconds(lookback_secs as i64);
        let mut sum_pq = Decimal::ZERO;
        let mut sum_q = Decimal::ZERO;

        for (price, qty, ts) in &self.history {
            if *ts < cutoff_time {
                continue;
            }
            let Some(q) = qty.as_ref() else { continue };
            if *q <= Decimal::ZERO {
                continue;
            }
            sum_pq += *price * *q;
            sum_q += *q;
        }

        if sum_q <= Decimal::ZERO {
            return None;
        }
        Some(sum_pq / sum_q)
    }

    pub fn has_sufficient_history(&self) -> bool {
        if self.history.len() < 60 {
            return false;
        }
        if let Some((_, _, oldest_ts)) = self.history.back() {
            let age = self.timestamp - *oldest_ts;
            return age.num_seconds() >= 60;
        }
        false
    }
}

fn decimal_sqrt(x: Decimal) -> Option<Decimal> {
    if x < Decimal::ZERO {
        return None;
    }
    if x.is_zero() {
        return Some(Decimal::ZERO);
    }

    let mut guess = x / Decimal::TWO;
    let tolerance = Decimal::new(1, 10);
    for _ in 0..50 {
        if guess.is_zero() {
            return Some(Decimal::ZERO);
        }
        let next_guess = (guess + x / guess) / Decimal::TWO;
        let diff = if next_guess > guess {
            next_guess - guess
        } else {
            guess - next_guess
        };
        if diff < tolerance {
            return Some(next_guess);
        }
        guess = next_guess;
    }
    Some(guess)
}

/// Thread-safe cache for spot prices.
#[derive(Debug, Clone, Default)]
pub struct PriceCache {
    prices: Arc<RwLock<HashMap<String, SpotPrice>>>,
}

impl PriceCache {
    pub fn new() -> Self {
        Self {
            prices: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn update(
        &self,
        symbol: &str,
        price: Decimal,
        quantity: Option<Decimal>,
        timestamp: DateTime<Utc>,
    ) {
        let mut prices = self.prices.write().await;

        if let Some(spot) = prices.get_mut(symbol) {
            spot.update(price, quantity, timestamp);
        } else {
            prices.insert(
                symbol.to_string(),
                SpotPrice::new(price, quantity, timestamp),
            );
        }
    }

    pub async fn get(&self, symbol: &str) -> Option<SpotPrice> {
        let prices = self.prices.read().await;
        prices.get(symbol).cloned()
    }

    pub async fn get_all(&self) -> HashMap<String, SpotPrice> {
        let prices = self.prices.read().await;
        prices.clone()
    }

    pub async fn momentum(&self, symbol: &str, lookback_secs: u64) -> Option<Decimal> {
        let prices = self.prices.read().await;
        prices.get(symbol)?.momentum(lookback_secs)
    }

    pub async fn weighted_momentum(&self, symbol: &str) -> Option<Decimal> {
        let prices = self.prices.read().await;
        prices.get(symbol)?.weighted_momentum()
    }

    pub async fn volatility(&self, symbol: &str, lookback_secs: u64) -> Option<Decimal> {
        let prices = self.prices.read().await;
        prices.get(symbol)?.volatility(lookback_secs)
    }

    pub async fn vwap(&self, symbol: &str, lookback_secs: u64) -> Option<Decimal> {
        let prices = self.prices.read().await;
        prices.get(symbol)?.vwap(lookback_secs)
    }

    pub async fn has_sufficient_history(&self, symbol: &str) -> bool {
        let prices = self.prices.read().await;
        prices
            .get(symbol)
            .map(|s| s.has_sufficient_history())
            .unwrap_or(false)
    }

    pub async fn len(&self) -> usize {
        let prices = self.prices.read().await;
        prices.len()
    }

    pub async fn is_empty(&self) -> bool {
        let prices = self.prices.read().await;
        prices.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_spot_price_momentum() {
        let now = Utc::now();
        let mut spot = SpotPrice::new(dec!(100), None, now - chrono::Duration::seconds(10));

        spot.update(dec!(101), None, now - chrono::Duration::seconds(5));
        spot.update(dec!(102), None, now);

        assert_eq!(spot.price, dec!(102));

        let momentum = spot.momentum(5);
        assert!(momentum.is_some());
        assert!(momentum.unwrap() > Decimal::ZERO);
    }

    #[test]
    fn test_price_history_bounded() {
        let now = Utc::now();
        let mut spot = SpotPrice::new(dec!(100), None, now);

        for i in 0..MAX_PRICE_HISTORY + 10 {
            spot.update(
                Decimal::from(100 + i as i64),
                None,
                now + chrono::Duration::seconds(i as i64),
            );
        }

        assert!(spot.history_len() <= MAX_PRICE_HISTORY);
    }

    #[tokio::test]
    async fn test_price_cache() {
        let cache = PriceCache::new();
        let now = Utc::now();

        cache.update("BTCUSDT", dec!(50000), None, now).await;
        cache.update("ETHUSDT", dec!(3000), None, now).await;

        let btc = cache.get("BTCUSDT").await;
        assert!(btc.is_some());
        assert_eq!(btc.unwrap().price, dec!(50000));

        let eth = cache.get("ETHUSDT").await;
        assert!(eth.is_some());
        assert_eq!(eth.unwrap().price, dec!(3000));

        assert_eq!(cache.len().await, 2);
    }
}
