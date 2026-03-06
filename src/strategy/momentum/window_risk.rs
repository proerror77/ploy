use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;

use super::{EventInfo, MomentumSignal};

/// Daily trade counter for rate limiting.
#[derive(Debug, Default)]
pub(super) struct DailyTradeCounter {
    count: u32,
    reset_date: Option<chrono::NaiveDate>,
}

impl DailyTradeCounter {
    pub(super) fn increment(&mut self) -> u32 {
        let today = Utc::now().date_naive();
        if self.reset_date != Some(today) {
            self.count = 0;
            self.reset_date = Some(today);
        }
        self.count += 1;
        self.count
    }

    pub(super) fn current(&mut self) -> u32 {
        let today = Utc::now().date_naive();
        if self.reset_date != Some(today) {
            self.count = 0;
            self.reset_date = Some(today);
        }
        self.count
    }
}

/// Pending signal for best-edge selection.
#[derive(Debug, Clone)]
pub(super) struct PendingSignal {
    pub(super) signal: MomentumSignal,
    pub(super) event: EventInfo,
    pub(super) edge: Decimal,
    pub(super) cost_usd: Decimal,
    pub(super) timestamp: DateTime<Utc>,
}

/// Window risk tracker for cross-symbol exposure limits.
/// Tracks exposure per 15-min window (grouped by event end time).
#[derive(Debug, Default)]
pub(super) struct WindowRiskTracker {
    /// Exposure by window ID (event end time as string)
    window_exposure: HashMap<String, Decimal>,
    /// Pending signals per window (for best-edge selection)
    pending_signals: HashMap<String, Vec<PendingSignal>>,
    /// Windows that have been executed (to prevent duplicates)
    executed_windows: HashMap<String, bool>,
}

impl WindowRiskTracker {
    /// Get window ID from event end time (rounded to 15-min).
    pub(super) fn window_id(event_end: &DateTime<Utc>) -> String {
        let ts = event_end.timestamp();
        let rounded = (ts / 900) * 900;
        DateTime::from_timestamp(rounded, 0)
            .unwrap_or(*event_end)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    }

    /// Check if window already has an executed trade.
    pub(super) fn has_executed(&self, window_id: &str) -> bool {
        self.executed_windows
            .get(window_id)
            .copied()
            .unwrap_or(false)
    }

    /// Mark window as executed.
    pub(super) fn mark_executed(&mut self, window_id: &str) {
        self.executed_windows.insert(window_id.to_string(), true);
    }

    /// Get current exposure for a window.
    pub(super) fn get_exposure(&self, window_id: &str) -> Decimal {
        self.window_exposure
            .get(window_id)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    /// Add exposure to a window.
    pub(super) fn add_exposure(&mut self, window_id: &str, amount: Decimal) {
        let current = self.get_exposure(window_id);
        self.window_exposure
            .insert(window_id.to_string(), current + amount);
    }

    /// Add pending signal for a window.
    pub(super) fn add_pending_signal(&mut self, window_id: &str, signal: PendingSignal) {
        self.pending_signals
            .entry(window_id.to_string())
            .or_default()
            .push(signal);
    }

    /// Get best signal for a window (highest edge).
    pub(super) fn get_best_signal(&self, window_id: &str) -> Option<PendingSignal> {
        self.pending_signals
            .get(window_id)
            .and_then(|signals| signals.iter().max_by(|a, b| a.edge.cmp(&b.edge)).cloned())
    }

    /// Clear pending signals for a window.
    pub(super) fn clear_pending(&mut self, window_id: &str) {
        self.pending_signals.remove(window_id);
    }

    /// Check if there are pending signals ready for execution (past delay threshold).
    pub(super) fn get_ready_windows(&self, delay_ms: u64) -> Vec<String> {
        let now = Utc::now();
        let threshold = ChronoDuration::milliseconds(delay_ms as i64);

        self.pending_signals
            .keys()
            .filter(|window_id| {
                if let Some(signals) = self.pending_signals.get(*window_id) {
                    if let Some(oldest) = signals.iter().min_by_key(|signal| signal.timestamp) {
                        return now.signed_duration_since(oldest.timestamp) >= threshold;
                    }
                }
                false
            })
            .cloned()
            .collect()
    }

    /// Cleanup old windows (older than 30 min).
    pub(super) fn cleanup_old(&mut self) {
        let now = Utc::now();
        let cutoff = now - ChronoDuration::minutes(30);
        let cutoff_str = Self::window_id(&cutoff);

        self.window_exposure.retain(|key, _| key >= &cutoff_str);
        self.executed_windows.retain(|key, _| key >= &cutoff_str);
        self.pending_signals.retain(|key, _| key >= &cutoff_str);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::momentum::Direction;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    fn sample_pending(edge: Decimal, seconds_ago: i64) -> PendingSignal {
        let now = Utc::now();
        PendingSignal {
            signal: MomentumSignal {
                symbol: "BTCUSDT".into(),
                direction: Direction::Up,
                cex_move_pct: dec!(0.01),
                pm_price: dec!(0.45),
                edge,
                confidence: 0.7,
                timestamp: now - ChronoDuration::seconds(seconds_ago),
            },
            event: EventInfo {
                slug: "btc-above".into(),
                title: "BTC above".into(),
                up_token_id: "up".into(),
                down_token_id: "down".into(),
                start_time: now - ChronoDuration::minutes(10),
                end_time: now + ChronoDuration::minutes(5),
                condition_id: "cond".into(),
                series_id: "series".into(),
                horizon: "5m".into(),
                price_to_beat: Some(dec!(100000)),
            },
            edge,
            cost_usd: dec!(12),
            timestamp: now - ChronoDuration::seconds(seconds_ago),
        }
    }

    #[test]
    fn window_id_rounds_down_to_fifteen_minute_boundary() {
        let event_end = Utc.with_ymd_and_hms(2026, 3, 6, 10, 37, 42).unwrap();
        assert_eq!(WindowRiskTracker::window_id(&event_end), "2026-03-06 10:30");
    }

    #[test]
    fn best_signal_and_ready_windows_follow_edge_and_delay() {
        let mut tracker = WindowRiskTracker::default();
        tracker.add_pending_signal("w1", sample_pending(dec!(0.03), 5));
        tracker.add_pending_signal("w1", sample_pending(dec!(0.07), 5));
        tracker.add_pending_signal("w2", sample_pending(dec!(0.02), 0));

        let best = tracker.get_best_signal("w1").unwrap();
        assert_eq!(best.edge, dec!(0.07));

        let ready = tracker.get_ready_windows(1_000);
        assert!(ready.iter().any(|window| window == "w1"));
        assert!(!ready.iter().any(|window| window == "w2"));
    }
}
