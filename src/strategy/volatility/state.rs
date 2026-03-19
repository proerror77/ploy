use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use tracing::info;

use crate::domain::Side;

/// Record of a completed 15-minute event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub symbol: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub start_price: Decimal,
    pub end_price: Decimal,
    pub high_price: Decimal,
    pub low_price: Decimal,
    pub outcome: Side,          // UP or DOWN
    pub deviation_pct: Decimal, // (end - start) / start
    pub range_pct: Decimal,     // (high - low) / start
}

/// Active event being tracked
#[derive(Debug, Clone)]
pub struct ActiveEvent {
    pub symbol: String,
    pub event_id: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub start_price: Decimal,
    pub current_price: Decimal,
    pub high_price: Decimal,
    pub low_price: Decimal,
    pub last_update: DateTime<Utc>,
}

impl ActiveEvent {
    pub fn new(
        symbol: String,
        event_id: String,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        start_price: Decimal,
    ) -> Self {
        Self {
            symbol,
            event_id,
            start_time,
            end_time,
            start_price,
            current_price: start_price,
            high_price: start_price,
            low_price: start_price,
            last_update: start_time,
        }
    }

    /// Update with new price
    pub fn update_price(&mut self, price: Decimal, timestamp: DateTime<Utc>) {
        self.current_price = price;
        self.last_update = timestamp;
        if price > self.high_price {
            self.high_price = price;
        }
        if price < self.low_price {
            self.low_price = price;
        }
    }

    /// Get deviation from start price as percentage
    pub fn deviation_pct(&self) -> Decimal {
        if self.start_price.is_zero() {
            return Decimal::ZERO;
        }
        (self.current_price - self.start_price) / self.start_price
    }

    /// Get range as percentage of start price
    pub fn range_pct(&self) -> Decimal {
        if self.start_price.is_zero() {
            return Decimal::ZERO;
        }
        (self.high_price - self.low_price) / self.start_price
    }

    /// Get time remaining in seconds
    pub fn time_remaining_secs(&self) -> i64 {
        (self.end_time - Utc::now()).num_seconds().max(0)
    }

    /// Check if event is still active
    pub fn is_active(&self) -> bool {
        Utc::now() < self.end_time
    }

    /// Predicted outcome based on current position
    pub fn predicted_outcome(&self) -> Side {
        if self.current_price >= self.start_price {
            Side::Up
        } else {
            Side::Down
        }
    }
}

/// Tracks events and maintains historical data
#[derive(Debug)]
pub struct EventTracker {
    /// Active events by (symbol, event_id)
    active_events: HashMap<String, ActiveEvent>,
    /// Historical event records by symbol
    history: HashMap<String, VecDeque<EventRecord>>,
    /// Maximum history to keep per symbol
    max_history: usize,
}

impl EventTracker {
    pub fn new(max_history: usize) -> Self {
        Self {
            active_events: HashMap::new(),
            history: HashMap::new(),
            max_history,
        }
    }

    /// Register a new event
    pub fn register_event(
        &mut self,
        symbol: &str,
        event_id: &str,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        start_price: Decimal,
    ) {
        let key = format!("{}:{}", symbol, event_id);

        if self.active_events.contains_key(&key) {
            return; // Already tracking
        }

        info!(
            "Tracking new event: {} {} start_price=${:.2}",
            symbol, event_id, start_price
        );

        let event = ActiveEvent::new(
            symbol.to_string(),
            event_id.to_string(),
            start_time,
            end_time,
            start_price,
        );

        self.active_events.insert(key, event);
    }

    /// Update price for active events
    pub fn update_price(&mut self, symbol: &str, price: Decimal, timestamp: DateTime<Utc>) {
        for (key, event) in self.active_events.iter_mut() {
            if key.starts_with(&format!("{}:", symbol)) && event.is_active() {
                event.update_price(price, timestamp);
            }
        }
    }

    /// Get active event for a symbol
    pub fn get_active_event(&self, symbol: &str, event_id: &str) -> Option<&ActiveEvent> {
        let key = format!("{}:{}", symbol, event_id);
        self.active_events.get(&key)
    }

    /// Check if an event is already being tracked (by event_id only)
    pub fn has_active_event(&self, event_id: &str) -> bool {
        self.active_events
            .keys()
            .any(|k| k.ends_with(&format!(":{}", event_id)))
    }

    /// Get active event by event_id only (searches across all symbols)
    pub fn get_event(&self, event_id: &str) -> Option<&ActiveEvent> {
        let suffix = format!(":{}", event_id);
        self.active_events
            .iter()
            .find(|(k, _)| k.ends_with(&suffix))
            .map(|(_, v)| v)
    }

    /// Start tracking a new event (convenience wrapper)
    pub fn start_event(
        &mut self,
        symbol: String,
        event_id: String,
        end_time: DateTime<Utc>,
        start_price: Decimal,
    ) {
        self.register_event(&symbol, &event_id, Utc::now(), end_time, start_price);
    }

    /// Update price for an event by event_id only (searches by event_id)
    pub fn update_price_by_event_id(&mut self, event_id: &str, price: Decimal) {
        let now = Utc::now();
        for (key, event) in self.active_events.iter_mut() {
            if key.ends_with(&format!(":{}", event_id)) && event.is_active() {
                event.update_price(price, now);
                return;
            }
        }
    }

    /// Finalize completed events and move to history
    pub fn finalize_completed_events(&mut self) {
        let now = Utc::now();
        let completed: Vec<String> = self
            .active_events
            .iter()
            .filter(|(_, e)| e.end_time <= now)
            .map(|(k, _)| k.clone())
            .collect();

        for key in completed {
            if let Some(event) = self.active_events.remove(&key) {
                let record = EventRecord {
                    symbol: event.symbol.clone(),
                    start_time: event.start_time,
                    end_time: event.end_time,
                    start_price: event.start_price,
                    end_price: event.current_price,
                    high_price: event.high_price,
                    low_price: event.low_price,
                    outcome: event.predicted_outcome(),
                    deviation_pct: event.deviation_pct(),
                    range_pct: event.range_pct(),
                };

                info!(
                    "Event completed: {} outcome={:?} deviation={:.3}% range={:.3}%",
                    key,
                    record.outcome,
                    record.deviation_pct * dec!(100),
                    record.range_pct * dec!(100)
                );

                let history = self.history.entry(event.symbol).or_default();
                history.push_back(record);
                while history.len() > self.max_history {
                    history.pop_front();
                }
            }
        }
    }

    /// Get historical volatility (average range) for a symbol
    pub fn historical_volatility(&self, symbol: &str) -> Option<Decimal> {
        let history = self.history.get(symbol)?;
        if history.is_empty() {
            return None;
        }

        let sum: Decimal = history.iter().map(|r| r.range_pct).sum();
        Some(sum / Decimal::from(history.len()))
    }

    /// Get historical win rate for UP outcomes
    pub fn up_win_rate(&self, symbol: &str) -> Option<Decimal> {
        let history = self.history.get(symbol)?;
        if history.is_empty() {
            return None;
        }

        let up_count = history.iter().filter(|r| r.outcome == Side::Up).count();
        Some(Decimal::from(up_count) / Decimal::from(history.len()))
    }

    /// Get average deviation for a symbol
    pub fn average_deviation(&self, symbol: &str) -> Option<Decimal> {
        let history = self.history.get(symbol)?;
        if history.is_empty() {
            return None;
        }

        let sum: Decimal = history.iter().map(|r| r.deviation_pct.abs()).sum();
        Some(sum / Decimal::from(history.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use rust_decimal_macros::dec;

    #[test]
    fn test_active_event() {
        let start = Utc::now();
        let end = start + Duration::minutes(15);
        let mut event = ActiveEvent::new(
            "BTCUSDT".to_string(),
            "event1".to_string(),
            start,
            end,
            dec!(100000),
        );

        event.update_price(dec!(100100), start + Duration::seconds(60));
        assert_eq!(event.deviation_pct(), dec!(0.001));
        assert_eq!(event.predicted_outcome(), Side::Up);

        event.update_price(dec!(99900), start + Duration::seconds(120));
        assert_eq!(event.deviation_pct(), dec!(-0.001));
        assert_eq!(event.predicted_outcome(), Side::Down);

        assert_eq!(event.range_pct(), dec!(0.002));
    }

    #[test]
    fn test_event_tracker() {
        let mut tracker = EventTracker::new(10);
        let start = Utc::now();
        let end = start + Duration::minutes(15);

        tracker.register_event("BTCUSDT", "event1", start, end, dec!(100000));
        tracker.update_price("BTCUSDT", dec!(100100), start + Duration::seconds(60));

        let event = tracker.get_active_event("BTCUSDT", "event1").unwrap();
        assert_eq!(event.current_price, dec!(100100));
    }
}
