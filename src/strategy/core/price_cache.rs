//! Price cache for real-time quote tracking

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Simple local price cache for split arbitrage
#[derive(Debug, Clone, Default)]
pub struct PriceCache {
    /// Map token_id -> (best_bid, best_ask, timestamp)
    prices: HashMap<String, (Option<Decimal>, Option<Decimal>, DateTime<Utc>)>,
}

impl PriceCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update price for a token
    pub fn update(&mut self, token_id: &str, bid: Option<Decimal>, ask: Option<Decimal>) {
        self.prices
            .insert(token_id.to_string(), (bid, ask, Utc::now()));
    }

    /// Get best ask price for a token
    pub fn get_ask(&self, token_id: &str) -> Option<Decimal> {
        self.prices.get(token_id).and_then(|(_, ask, _)| *ask)
    }

    /// Get best bid price for a token
    pub fn get_bid(&self, token_id: &str) -> Option<Decimal> {
        self.prices.get(token_id).and_then(|(bid, _, _)| *bid)
    }

    /// Get both bid and ask
    pub fn get_prices(&self, token_id: &str) -> Option<(Option<Decimal>, Option<Decimal>)> {
        self.prices.get(token_id).map(|(bid, ask, _)| (*bid, *ask))
    }

    /// Get last update time for a token
    pub fn get_timestamp(&self, token_id: &str) -> Option<DateTime<Utc>> {
        self.prices.get(token_id).map(|(_, _, ts)| *ts)
    }

    /// Check if we have prices for a token
    pub fn has_token(&self, token_id: &str) -> bool {
        self.prices.contains_key(token_id)
    }

    /// Get all tracked token IDs
    pub fn token_ids(&self) -> Vec<String> {
        self.prices.keys().cloned().collect()
    }

    /// Clear all prices
    pub fn clear(&mut self) {
        self.prices.clear();
    }
}
