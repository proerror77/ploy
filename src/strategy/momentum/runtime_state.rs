use super::*;

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
    /// Exposure by window ID (event end time as string).
    window_exposure: HashMap<String, Decimal>,
    /// Pending signals per window (for best-edge selection).
    pending_signals: HashMap<String, Vec<PendingSignal>>,
    /// Windows that have been executed (to prevent duplicates).
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

    pub(super) fn has_executed(&self, window_id: &str) -> bool {
        self.executed_windows
            .get(window_id)
            .copied()
            .unwrap_or(false)
    }

    pub(super) fn mark_executed(&mut self, window_id: &str) {
        self.executed_windows.insert(window_id.to_string(), true);
    }

    pub(super) fn get_exposure(&self, window_id: &str) -> Decimal {
        self.window_exposure
            .get(window_id)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    pub(super) fn add_exposure(&mut self, window_id: &str, amount: Decimal) {
        let current = self.get_exposure(window_id);
        self.window_exposure
            .insert(window_id.to_string(), current + amount);
    }

    pub(super) fn add_pending_signal(&mut self, window_id: &str, signal: PendingSignal) {
        self.pending_signals
            .entry(window_id.to_string())
            .or_default()
            .push(signal);
    }

    pub(super) fn get_best_signal(&self, window_id: &str) -> Option<PendingSignal> {
        self.pending_signals
            .get(window_id)
            .and_then(|signals| signals.iter().max_by(|a, b| a.edge.cmp(&b.edge)).cloned())
    }

    pub(super) fn clear_pending(&mut self, window_id: &str) {
        self.pending_signals.remove(window_id);
    }

    pub(super) fn get_ready_windows(&self, delay_ms: u64) -> Vec<String> {
        let now = Utc::now();
        let threshold = ChronoDuration::milliseconds(delay_ms as i64);

        self.pending_signals
            .keys()
            .filter(|window_id| {
                if let Some(signals) = self.pending_signals.get(*window_id) {
                    if let Some(oldest) = signals.iter().min_by_key(|s| s.timestamp) {
                        return now.signed_duration_since(oldest.timestamp) >= threshold;
                    }
                }
                false
            })
            .cloned()
            .collect()
    }

    pub(super) fn cleanup_old(&mut self) {
        let now = Utc::now();
        let cutoff = now - ChronoDuration::minutes(30);
        let cutoff_str = Self::window_id(&cutoff);

        self.window_exposure.retain(|k, _| k >= &cutoff_str);
        self.executed_windows.retain(|k, _| k >= &cutoff_str);
        self.pending_signals.retain(|k, _| k >= &cutoff_str);
    }
}

impl MomentumEngine {
    pub(super) async fn daily_limit_reached(&self) -> bool {
        if self.config.max_daily_trades == 0 {
            return false;
        }
        let mut counter = self.daily_trades.write().await;
        counter.current() >= self.config.max_daily_trades
    }

    pub(super) async fn record_trade(&self) -> u32 {
        let mut counter = self.daily_trades.write().await;
        counter.increment()
    }

    fn estimated_win_probability(&self, signal: &MomentumSignal) -> Decimal {
        (signal.pm_price + signal.edge)
            .max(Decimal::ZERO)
            .min(Decimal::ONE)
    }

    fn signal_kelly_fraction(&self, signal: &MomentumSignal) -> Decimal {
        if signal.pm_price <= Decimal::ZERO || signal.pm_price >= Decimal::ONE {
            return Decimal::ZERO;
        }

        let p = self.estimated_win_probability(signal);
        let denom = Decimal::ONE - signal.pm_price;
        if denom <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        ((p - signal.pm_price) / denom)
            .max(Decimal::ZERO)
            .min(Decimal::ONE)
    }

    pub(super) fn apply_signal_position_sizing(
        &self,
        base_shares: u64,
        signal: &MomentumSignal,
    ) -> u64 {
        if base_shares == 0 {
            return 0;
        }

        let mut multiplier = Decimal::ONE;

        if self.config.dynamic_position_sizing {
            let conf = Decimal::from_f64(signal.confidence.clamp(0.0, 1.0)).unwrap_or(Decimal::ONE);
            multiplier *= conf;
        }

        if self.config.use_kelly_sizing {
            let kelly = self.signal_kelly_fraction(signal);
            let cap = self.config.kelly_fraction_cap.max(dec!(0.0001));
            let normalized = (kelly / cap).min(Decimal::ONE);
            multiplier *= normalized;
        }

        let scaled = (Decimal::from(base_shares) * multiplier)
            .floor()
            .to_u64()
            .unwrap_or(0);

        if scaled == 0 {
            debug!(
                "Position size scaled to 0 (base_shares={}, multiplier={:.4})",
                base_shares, multiplier
            );
        }

        scaled
    }

    pub async fn positions_count(&self) -> usize {
        self.positions.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_signal(edge: Decimal, timestamp: DateTime<Utc>) -> PendingSignal {
        PendingSignal {
            signal: MomentumSignal {
                symbol: "BTCUSDT".into(),
                direction: Direction::Up,
                cex_move_pct: dec!(0.01),
                pm_price: dec!(0.25),
                edge,
                confidence: 0.8,
                timestamp,
            },
            event: EventInfo {
                slug: "event".into(),
                title: "event".into(),
                up_token_id: "up".into(),
                down_token_id: "down".into(),
                start_time: timestamp - ChronoDuration::minutes(10),
                end_time: timestamp + ChronoDuration::minutes(5),
                condition_id: "condition".into(),
                series_id: "series".into(),
                horizon: "5m".into(),
                price_to_beat: None,
            },
            edge,
            cost_usd: dec!(25),
            timestamp,
        }
    }

    #[test]
    fn test_window_id_rounds_down_to_15m_boundary() {
        let event_end = DateTime::parse_from_rfc3339("2026-03-10T12:22:45Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(WindowRiskTracker::window_id(&event_end), "2026-03-10 12:15");
    }

    #[test]
    fn test_window_tracker_prefers_highest_edge_signal() {
        let now = Utc::now();
        let mut tracker = WindowRiskTracker::default();
        tracker.add_pending_signal("w1", pending_signal(dec!(0.04), now));
        tracker.add_pending_signal(
            "w1",
            pending_signal(dec!(0.09), now + ChronoDuration::seconds(1)),
        );

        let best = tracker.get_best_signal("w1").expect("best signal");
        assert_eq!(best.edge, dec!(0.09));
    }
}
