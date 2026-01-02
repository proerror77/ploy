//! Multi-event monitoring system for tracking arbitrage opportunities across
//! all active events in a series.

use crate::adapters::{GammaEventInfo, PolymarketClient};
use crate::config::StrategyConfig;
use crate::domain::{DumpSignal, Quote, Side};
use crate::error::Result;
use crate::strategy::SignalDetector;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use tracing::{debug, info, warn};

/// Tracks a single event within a series
#[derive(Debug)]
pub struct EventTracker {
    /// Unique event ID
    pub event_id: String,
    /// Event slug for display
    pub event_slug: String,
    /// UP token ID
    pub up_token_id: String,
    /// DOWN token ID
    pub down_token_id: String,
    /// Event end time
    pub end_time: DateTime<Utc>,
    /// Signal detector for this event
    pub signal_detector: SignalDetector,
    /// Current UP quote
    pub up_quote: Option<Quote>,
    /// Current DOWN quote
    pub down_quote: Option<Quote>,
    /// Whether this event is still active
    pub is_active: bool,
}

impl EventTracker {
    /// Create a new event tracker
    pub fn new(
        event: &GammaEventInfo,
        up_token_id: String,
        down_token_id: String,
        config: StrategyConfig,
    ) -> Self {
        let end_time = event
            .end_date
            .as_ref()
            .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        Self {
            event_id: event.id.clone(),
            event_slug: event.slug.clone().unwrap_or_default(),
            up_token_id,
            down_token_id,
            end_time,
            signal_detector: SignalDetector::new(config),
            up_quote: None,
            down_quote: None,
            is_active: true,
        }
    }

    /// Get time remaining until event ends
    pub fn time_remaining(&self) -> Duration {
        let now = Utc::now();
        if self.end_time > now {
            self.end_time - now
        } else {
            Duration::zero()
        }
    }

    /// Check if the event is still tradeable
    pub fn is_tradeable(&self) -> bool {
        self.is_active && self.time_remaining() > Duration::seconds(30)
    }

    /// Update quote for this event
    pub fn update_quote(&mut self, token_id: &str, quote: Quote) {
        if token_id == self.up_token_id {
            self.up_quote = Some(quote);
        } else if token_id == self.down_token_id {
            self.down_quote = Some(quote);
        }
    }

    /// Get the combined ask sum (up_ask + down_ask)
    pub fn ask_sum(&self) -> Option<Decimal> {
        match (&self.up_quote, &self.down_quote) {
            (Some(up), Some(down)) => {
                match (up.best_ask, down.best_ask) {
                    (Some(up_ask), Some(down_ask)) => Some(up_ask + down_ask),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// Arbitrage opportunity detected across events
#[derive(Debug, Clone)]
pub struct ArbitrageOpportunity {
    /// Event ID where opportunity exists
    pub event_id: String,
    /// Event slug for display
    pub event_slug: String,
    /// The dump signal that triggered this
    pub signal: DumpSignal,
    /// UP token quote
    pub up_quote: Quote,
    /// DOWN token quote
    pub down_quote: Quote,
    /// Combined ask sum (up_ask + down_ask)
    pub sum: Decimal,
    /// Estimated profit per share
    pub profit_per_share: Decimal,
    /// Time remaining in this event
    pub time_remaining: Duration,
    /// UP token ID
    pub up_token_id: String,
    /// DOWN token ID
    pub down_token_id: String,
}

impl ArbitrageOpportunity {
    /// Estimate total profit for given shares
    pub fn estimate_profit(&self, shares: u64) -> Decimal {
        self.profit_per_share * Decimal::from(shares)
    }
}

/// Monitors all events in a series for arbitrage opportunities
pub struct MultiEventMonitor {
    /// Series ID being monitored
    pub series_id: String,
    /// All tracked events (event_id -> tracker)
    events: HashMap<String, EventTracker>,
    /// Token to event mapping (token_id -> event_id)
    token_to_event: HashMap<String, String>,
    /// Strategy configuration
    config: StrategyConfig,
    /// Last refresh time
    last_refresh: Option<DateTime<Utc>>,
}

impl MultiEventMonitor {
    /// Create a new multi-event monitor
    pub fn new(series_id: &str, config: StrategyConfig) -> Self {
        Self {
            series_id: series_id.to_string(),
            events: HashMap::new(),
            token_to_event: HashMap::new(),
            config,
            last_refresh: None,
        }
    }

    /// Get the number of tracked events
    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Get all token IDs being tracked
    pub fn all_token_ids(&self) -> Vec<String> {
        self.token_to_event.keys().cloned().collect()
    }

    /// Refresh event list from API
    /// Returns new token IDs that need to be subscribed
    pub async fn refresh_events(&mut self, client: &PolymarketClient) -> Result<Vec<String>> {
        let all_tokens = client.get_series_all_tokens(&self.series_id).await?;
        let now = Utc::now();
        let mut new_token_ids = Vec::new();

        for (event, up_token, down_token) in all_tokens {
            if !self.events.contains_key(&event.id) {
                // New event discovered
                info!(
                    "Discovered new event: {} ({})",
                    event.slug.as_deref().unwrap_or("unknown"),
                    event.id
                );

                let tracker = EventTracker::new(&event, up_token.clone(), down_token.clone(), self.config.clone());

                self.token_to_event.insert(up_token.clone(), event.id.clone());
                self.token_to_event.insert(down_token.clone(), event.id.clone());
                new_token_ids.push(up_token);
                new_token_ids.push(down_token);
                self.events.insert(event.id, tracker);
            }
        }

        // Mark expired events as inactive and clean up
        let expired_events: Vec<String> = self.events
            .iter()
            .filter(|(_, tracker)| tracker.end_time <= now)
            .map(|(id, _)| id.clone())
            .collect();

        for event_id in expired_events {
            if let Some(tracker) = self.events.remove(&event_id) {
                info!("Removed expired event: {} ({})", tracker.event_slug, event_id);
                self.token_to_event.remove(&tracker.up_token_id);
                self.token_to_event.remove(&tracker.down_token_id);
            }
        }

        self.last_refresh = Some(now);
        Ok(new_token_ids)
    }

    /// Process a quote update
    pub fn update_quote(&mut self, token_id: &str, quote: &Quote) {
        if let Some(event_id) = self.token_to_event.get(token_id).cloned() {
            if let Some(tracker) = self.events.get_mut(&event_id) {
                tracker.update_quote(token_id, quote.clone());
            }
        }
    }

    /// Process a quote and check for dump signals
    pub fn process_quote(&mut self, token_id: &str, quote: &Quote) -> Option<ArbitrageOpportunity> {
        let event_id = self.token_to_event.get(token_id)?.clone();
        let tracker = self.events.get_mut(&event_id)?;

        // Update the quote
        tracker.update_quote(token_id, quote.clone());

        // Check if event is still tradeable
        if !tracker.is_tradeable() {
            return None;
        }

        // Update signal detector
        let signal = tracker.signal_detector.update(quote, Some(&tracker.event_slug))?;

        // Build opportunity if we have both quotes
        self.build_opportunity(&event_id, signal)
    }

    /// Build an arbitrage opportunity from a signal
    fn build_opportunity(&self, event_id: &str, signal: DumpSignal) -> Option<ArbitrageOpportunity> {
        let tracker = self.events.get(event_id)?;

        let up_quote = tracker.up_quote.clone()?;
        let down_quote = tracker.down_quote.clone()?;

        let up_ask = up_quote.best_ask?;
        let down_ask = down_quote.best_ask?;
        let sum = up_ask + down_ask;

        // Calculate profit: if sum < 1, we profit from the difference
        // profit_per_share = 1 - sum (simplified, before fees)
        let profit_per_share = if sum < Decimal::ONE {
            Decimal::ONE - sum
        } else {
            Decimal::ZERO
        };

        Some(ArbitrageOpportunity {
            event_id: event_id.to_string(),
            event_slug: tracker.event_slug.clone(),
            signal,
            up_quote,
            down_quote,
            sum,
            profit_per_share,
            time_remaining: tracker.time_remaining(),
            up_token_id: tracker.up_token_id.clone(),
            down_token_id: tracker.down_token_id.clone(),
        })
    }

    /// Find all current arbitrage opportunities across all events
    pub fn find_all_opportunities(&self) -> Vec<ArbitrageOpportunity> {
        let target = self.config.effective_sum_target();

        self.events
            .values()
            .filter(|tracker| tracker.is_tradeable())
            .filter_map(|tracker| {
                let up_quote = tracker.up_quote.clone()?;
                let down_quote = tracker.down_quote.clone()?;
                let sum = tracker.ask_sum()?;

                // Only include if sum is below target (profitable)
                if sum <= target {
                    Some(ArbitrageOpportunity {
                        event_id: tracker.event_id.clone(),
                        event_slug: tracker.event_slug.clone(),
                        signal: DumpSignal {
                            side: Side::Up, // placeholder
                            trigger_price: Decimal::ZERO,
                            reference_price: Decimal::ZERO,
                            drop_pct: Decimal::ZERO,
                            timestamp: Utc::now(),
                            spread_bps: 0,
                        },
                        up_quote,
                        down_quote,
                        sum,
                        profit_per_share: Decimal::ONE - sum,
                        time_remaining: tracker.time_remaining(),
                        up_token_id: tracker.up_token_id.clone(),
                        down_token_id: tracker.down_token_id.clone(),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find the best arbitrage opportunity across all events
    pub fn find_best_opportunity(&self) -> Option<ArbitrageOpportunity> {
        let mut opportunities = self.find_all_opportunities();

        // Sort by: lowest sum (highest profit), then most time remaining
        opportunities.sort_by(|a, b| {
            a.sum
                .cmp(&b.sum)
                .then_with(|| b.time_remaining.cmp(&a.time_remaining))
        });

        opportunities.into_iter().next()
    }

    /// Get a summary of all tracked events
    pub fn summary(&self) -> Vec<EventSummary> {
        self.events
            .values()
            .map(|tracker| EventSummary {
                event_id: tracker.event_id.clone(),
                event_slug: tracker.event_slug.clone(),
                up_ask: tracker.up_quote.as_ref().and_then(|q| q.best_ask),
                down_ask: tracker.down_quote.as_ref().and_then(|q| q.best_ask),
                sum: tracker.ask_sum(),
                time_remaining: tracker.time_remaining(),
                is_tradeable: tracker.is_tradeable(),
            })
            .collect()
    }
}

/// Summary of an event's current state
#[derive(Debug, Clone)]
pub struct EventSummary {
    pub event_id: String,
    pub event_slug: String,
    pub up_ask: Option<Decimal>,
    pub down_ask: Option<Decimal>,
    pub sum: Option<Decimal>,
    pub time_remaining: Duration,
    pub is_tradeable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn test_config() -> StrategyConfig {
        StrategyConfig {
            shares: 20,
            window_min: 2,
            move_pct: dec!(0.15),
            sum_target: dec!(0.95),
            fee_buffer: dec!(0.005),
            slippage_buffer: dec!(0.02),
            profit_buffer: dec!(0.01),
        }
    }

    #[test]
    fn test_event_tracker_time_remaining() {
        let event = GammaEventInfo {
            id: "test-event".to_string(),
            slug: Some("test-slug".to_string()),
            title: Some("Test Event".to_string()),
            end_date: Some((Utc::now() + Duration::minutes(10)).to_rfc3339()),
            closed: false,
            markets: vec![],
        };

        let tracker = EventTracker::new(&event, "up-token".to_string(), "down-token".to_string(), test_config());

        assert!(tracker.time_remaining() > Duration::minutes(9));
        assert!(tracker.time_remaining() < Duration::minutes(11));
        assert!(tracker.is_tradeable());
    }

    #[test]
    fn test_multi_event_monitor_creation() {
        let config = test_config();
        let monitor = MultiEventMonitor::new("10423", config);

        assert_eq!(monitor.series_id, "10423");
        assert_eq!(monitor.event_count(), 0);
    }

    #[test]
    fn test_ask_sum_calculation() {
        let event = GammaEventInfo {
            id: "test-event".to_string(),
            slug: Some("test-slug".to_string()),
            title: Some("Test Event".to_string()),
            end_date: Some((Utc::now() + Duration::minutes(10)).to_rfc3339()),
            closed: false,
            markets: vec![],
        };

        let mut tracker = EventTracker::new(&event, "up-token".to_string(), "down-token".to_string(), test_config());

        // Set quotes
        tracker.update_quote("up-token", Quote {
            side: Side::Up,
            best_bid: Some(dec!(0.44)),
            best_ask: Some(dec!(0.45)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp: Utc::now(),
        });

        tracker.update_quote("down-token", Quote {
            side: Side::Down,
            best_bid: Some(dec!(0.49)),
            best_ask: Some(dec!(0.50)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp: Utc::now(),
        });

        let sum = tracker.ask_sum().unwrap();
        assert_eq!(sum, dec!(0.95)); // 0.45 + 0.50 = 0.95
    }
}
