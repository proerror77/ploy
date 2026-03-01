//! Data Plane Freshness Tracker — per-symbol, per-source data freshness monitoring.
//!
//! Tracks when each (source, symbol) pair last received data, enabling detection
//! of partial feed failures that the global `last_ws_message` timestamp misses.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Data source identifiers
// ---------------------------------------------------------------------------

/// Identifies the upstream data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataSource {
    PolymarketWs,
    BinanceSpot,
    BinanceKline,
    BinanceLob,
    ChainlinkRtds,
}

impl DataSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PolymarketWs => "polymarket_ws",
            Self::BinanceSpot => "binance_spot",
            Self::BinanceKline => "binance_kline",
            Self::BinanceLob => "binance_lob",
            Self::ChainlinkRtds => "chainlink_rtds",
        }
    }
}

// ---------------------------------------------------------------------------
// Per-symbol freshness entry
// ---------------------------------------------------------------------------

/// Freshness state for a single (source, symbol) pair.
#[derive(Debug)]
pub struct SymbolFreshness {
    /// Last update timestamp (unix millis for atomic storage).
    last_update_ms: AtomicU64,
    /// Total updates received.
    update_count: AtomicU64,
}
impl SymbolFreshness {
    fn new() -> Self {
        Self {
            last_update_ms: AtomicU64::new(0),
            update_count: AtomicU64::new(0),
        }
    }

    fn record(&self) {
        let now_ms = Utc::now().timestamp_millis() as u64;
        self.last_update_ms.store(now_ms, Ordering::Relaxed);
        self.update_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Seconds since last update, or None if never updated.
    pub fn staleness_secs(&self) -> Option<f64> {
        let ms = self.last_update_ms.load(Ordering::Relaxed);
        if ms == 0 {
            return None;
        }
        let now_ms = Utc::now().timestamp_millis() as u64;
        Some((now_ms.saturating_sub(ms)) as f64 / 1000.0)
    }

    pub fn last_update(&self) -> Option<DateTime<Utc>> {
        let ms = self.last_update_ms.load(Ordering::Relaxed) as i64;
        if ms == 0 {
            return None;
        }
        DateTime::from_timestamp_millis(ms)
    }

    pub fn count(&self) -> u64 {
        self.update_count.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Freshness key
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FreshnessKey {
    source: DataSource,
    symbol: String,
}

// ---------------------------------------------------------------------------
// DataPlaneFreshness — the shared tracker
// ---------------------------------------------------------------------------

/// Tracks per-(source, symbol) data freshness across the platform.
/// Thread-safe and lock-free for hot-path recording.
#[derive(Debug, Clone)]
pub struct DataPlaneFreshness {
    entries: Arc<DashMap<FreshnessKey, SymbolFreshness>>,
    /// Per-source connection status (true = connected).
    source_connected: Arc<DashMap<DataSource, bool>>,
    /// Per-source total message count.
    source_message_count: Arc<DashMap<DataSource, AtomicU64>>,
    /// Per-source subscription count (tokens/symbols subscribed).
    source_subscription_count: Arc<DashMap<DataSource, AtomicU64>>,
    /// Broadcast channel lag events (dropped messages).
    broadcast_lag_count: Arc<AtomicU64>,
}

impl DataPlaneFreshness {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(DashMap::new()),
            source_connected: Arc::new(DashMap::new()),
            source_message_count: Arc::new(DashMap::new()),
            source_subscription_count: Arc::new(DashMap::new()),
            broadcast_lag_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Record a data update for a (source, symbol) pair.
    /// Called from WS adapters on every incoming message.
    pub fn record_update(&self, source: DataSource, symbol: &str) {
        let key = FreshnessKey {
            source,
            symbol: symbol.to_string(),
        };
        self.entries
            .entry(key)
            .or_insert_with(SymbolFreshness::new)
            .record();

        self.source_message_count
            .entry(source)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record a source connection state change.
    pub fn set_source_connected(&self, source: DataSource, connected: bool) {
        self.source_connected.insert(source, connected);
    }

    /// Record a broadcast channel lag event.
    pub fn record_broadcast_lag(&self, count: u64) {
        self.broadcast_lag_count.fetch_add(count, Ordering::Relaxed);
    }

    /// Update the subscription count for a source (called when tokens are added/removed).
    pub fn set_subscription_count(&self, source: DataSource, count: u64) {
        self.source_subscription_count
            .entry(source)
            .or_insert_with(|| AtomicU64::new(0))
            .store(count, Ordering::Relaxed);
    }

    /// Get staleness for a specific (source, symbol).
    pub fn staleness(&self, source: DataSource, symbol: &str) -> Option<f64> {
        let key = FreshnessKey {
            source,
            symbol: symbol.to_string(),
        };
        self.entries.get(&key).and_then(|e| e.staleness_secs())
    }

    /// Check if any symbol exceeds the staleness threshold.
    pub fn stale_symbols(&self, threshold_secs: f64) -> Vec<(DataSource, String, f64)> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let staleness = entry.value().staleness_secs()?;
                if staleness > threshold_secs {
                    Some((entry.key().source, entry.key().symbol.clone(), staleness))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Total unique (source, symbol) pairs being tracked.
    pub fn tracked_symbol_count(&self) -> usize {
        self.entries.len()
    }

    /// Total broadcast lag events.
    pub fn total_broadcast_lag(&self) -> u64 {
        self.broadcast_lag_count.load(Ordering::Relaxed)
    }

    /// Export per-symbol freshness metrics in Prometheus text format.
    pub fn prometheus_metrics(&self) -> String {
        let mut out = String::with_capacity(4096);

        // Per-symbol staleness
        out.push_str("# HELP ploy_symbol_staleness_seconds Seconds since last update per symbol\n");
        out.push_str("# TYPE ploy_symbol_staleness_seconds gauge\n");
        for entry in self.entries.iter() {
            if let Some(staleness) = entry.value().staleness_secs() {
                out.push_str(&format!(
                    "ploy_symbol_staleness_seconds{{source=\"{}\",symbol=\"{}\"}} {:.3}\n",
                    entry.key().source.as_str(),
                    entry.key().symbol,
                    staleness,
                ));
            }
        }

        // Per-symbol update count
        out.push_str("\n# HELP ploy_symbol_updates_total Total updates received per symbol\n");
        out.push_str("# TYPE ploy_symbol_updates_total counter\n");
        for entry in self.entries.iter() {
            out.push_str(&format!(
                "ploy_symbol_updates_total{{source=\"{}\",symbol=\"{}\"}} {}\n",
                entry.key().source.as_str(),
                entry.key().symbol,
                entry.value().count(),
            ));
        }

        // Per-source connection status
        out.push_str("\n# HELP ploy_source_connected Data source connection status\n");
        out.push_str("# TYPE ploy_source_connected gauge\n");
        for entry in self.source_connected.iter() {
            out.push_str(&format!(
                "ploy_source_connected{{source=\"{}\"}} {}\n",
                entry.key().as_str(),
                if *entry.value() { 1 } else { 0 },
            ));
        }

        // Per-source message count
        out.push_str("\n# HELP ploy_source_messages_total Total messages per source\n");
        out.push_str("# TYPE ploy_source_messages_total counter\n");
        for entry in self.source_message_count.iter() {
            out.push_str(&format!(
                "ploy_source_messages_total{{source=\"{}\"}} {}\n",
                entry.key().as_str(),
                entry.value().load(Ordering::Relaxed),
            ));
        }

        // Tracked symbol count
        out.push_str(&format!(
            "\n# HELP ploy_tracked_symbols_total Total unique symbols being tracked\n\
             # TYPE ploy_tracked_symbols_total gauge\n\
             ploy_tracked_symbols_total {}\n",
            self.entries.len(),
        ));

        // Per-source subscription count
        out.push_str("\n# HELP ploy_source_subscriptions_total Subscribed tokens/symbols per source\n");
        out.push_str("# TYPE ploy_source_subscriptions_total gauge\n");
        for entry in self.source_subscription_count.iter() {
            out.push_str(&format!(
                "ploy_source_subscriptions_total{{source=\"{}\"}} {}\n",
                entry.key().as_str(),
                entry.value().load(Ordering::Relaxed),
            ));
        }

        // Broadcast lag
        out.push_str(&format!(
            "\n# HELP ploy_broadcast_lag_total Total broadcast channel lag events\n\
             # TYPE ploy_broadcast_lag_total counter\n\
             ploy_broadcast_lag_total {}\n",
            self.broadcast_lag_count.load(Ordering::Relaxed),
        ));

        out
    }
}

impl Default for DataPlaneFreshness {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_query_freshness() {
        let f = DataPlaneFreshness::new();

        // No data yet
        assert!(f.staleness(DataSource::BinanceSpot, "BTCUSDT").is_none());

        // Record an update
        f.record_update(DataSource::BinanceSpot, "BTCUSDT");

        // Should have very low staleness (just recorded)
        let staleness = f.staleness(DataSource::BinanceSpot, "BTCUSDT").unwrap();
        assert!(staleness < 1.0);

        // Count should be 1
        let key = FreshnessKey {
            source: DataSource::BinanceSpot,
            symbol: "BTCUSDT".into(),
        };
        assert_eq!(f.entries.get(&key).unwrap().count(), 1);
    }

    #[test]
    fn different_sources_tracked_independently() {
        let f = DataPlaneFreshness::new();

        f.record_update(DataSource::BinanceSpot, "BTCUSDT");
        f.record_update(DataSource::PolymarketWs, "tok-up-1");
        f.record_update(DataSource::BinanceSpot, "ETHUSDT");

        assert_eq!(f.tracked_symbol_count(), 3);
        assert!(f.staleness(DataSource::BinanceSpot, "BTCUSDT").is_some());
        assert!(f.staleness(DataSource::PolymarketWs, "tok-up-1").is_some());
        assert!(f.staleness(DataSource::PolymarketWs, "BTCUSDT").is_none()); // wrong source
    }

    #[test]
    fn stale_symbols_detection() {
        let f = DataPlaneFreshness::new();

        f.record_update(DataSource::BinanceSpot, "BTCUSDT");
        f.record_update(DataSource::BinanceSpot, "ETHUSDT");

        // Nothing should be stale (just recorded)
        let stale = f.stale_symbols(30.0);
        assert!(stale.is_empty());

        // With a very low threshold, everything is "stale"
        // (can't easily test real staleness without sleeping, but we can test the threshold logic)
        // Record with a past timestamp by manipulating the atomic directly
        let key = FreshnessKey {
            source: DataSource::BinanceSpot,
            symbol: "ETHUSDT".into(),
        };
        if let Some(entry) = f.entries.get(&key) {
            // Set last_update to 60 seconds ago
            let past_ms = (Utc::now().timestamp_millis() - 60_000) as u64;
            entry.last_update_ms.store(past_ms, Ordering::Relaxed);
        }

        let stale = f.stale_symbols(30.0);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].1, "ETHUSDT");
        assert!(stale[0].2 > 50.0); // should be ~60s
    }

    #[test]
    fn source_connection_tracking() {
        let f = DataPlaneFreshness::new();

        f.set_source_connected(DataSource::BinanceSpot, true);
        f.set_source_connected(DataSource::PolymarketWs, false);

        assert_eq!(*f.source_connected.get(&DataSource::BinanceSpot).unwrap(), true);
        assert_eq!(*f.source_connected.get(&DataSource::PolymarketWs).unwrap(), false);
    }

    #[test]
    fn broadcast_lag_counting() {
        let f = DataPlaneFreshness::new();

        f.record_broadcast_lag(5);
        f.record_broadcast_lag(3);

        assert_eq!(f.total_broadcast_lag(), 8);
    }

    #[test]
    fn prometheus_output_format() {
        let f = DataPlaneFreshness::new();

        f.record_update(DataSource::BinanceSpot, "BTCUSDT");
        f.set_source_connected(DataSource::BinanceSpot, true);
        f.set_subscription_count(DataSource::BinanceSpot, 3);
        f.record_broadcast_lag(2);

        let output = f.prometheus_metrics();

        assert!(output.contains("ploy_symbol_staleness_seconds{source=\"binance_spot\",symbol=\"BTCUSDT\"}"));
        assert!(output.contains("ploy_symbol_updates_total{source=\"binance_spot\",symbol=\"BTCUSDT\"} 1"));
        assert!(output.contains("ploy_source_connected{source=\"binance_spot\"} 1"));
        assert!(output.contains("ploy_source_messages_total{source=\"binance_spot\"} 1"));
        assert!(output.contains("ploy_source_subscriptions_total{source=\"binance_spot\"} 3"));
        assert!(output.contains("ploy_tracked_symbols_total 1"));
        assert!(output.contains("ploy_broadcast_lag_total 2"));
    }

    #[test]
    fn subscription_count_tracking() {
        let f = DataPlaneFreshness::new();

        f.set_subscription_count(DataSource::PolymarketWs, 10);
        f.set_subscription_count(DataSource::BinanceSpot, 3);

        let output = f.prometheus_metrics();
        assert!(output.contains("ploy_source_subscriptions_total{source=\"polymarket_ws\"} 10"));
        assert!(output.contains("ploy_source_subscriptions_total{source=\"binance_spot\"} 3"));

        // Update count
        f.set_subscription_count(DataSource::PolymarketWs, 15);
        let output = f.prometheus_metrics();
        assert!(output.contains("ploy_source_subscriptions_total{source=\"polymarket_ws\"} 15"));
    }
}
