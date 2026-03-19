use super::*;

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
    pub(super) async fn queue_pending_signal(
        &self,
        signal: MomentumSignal,
        event: &EventInfo,
        window_id: String,
        estimated_cost: Decimal,
    ) -> Result<()> {
        let pending = PendingSignal {
            signal: signal.clone(),
            event: event.clone(),
            edge: signal.edge,
            cost_usd: estimated_cost,
            timestamp: Utc::now(),
        };

        {
            let mut tracker = self.window_tracker.write().await;
            tracker.add_pending_signal(&window_id, pending);
        }

        info!(
            "📋 Queued: {} {} edge={:.2}% (window {})",
            signal.symbol,
            signal.direction,
            signal.edge * dec!(100),
            window_id
        );

        Ok(())
    }

    /// Process pending signals and execute best edge (if ready)
    pub(super) async fn process_pending_signals(&self) -> Result<()> {
        if !self.config.best_edge_only {
            return Ok(());
        }

        let ready_windows = {
            let tracker = self.window_tracker.read().await;
            tracker.get_ready_windows(self.config.signal_collection_delay_ms)
        };

        for window_id in ready_windows {
            let best_signal = {
                let tracker = self.window_tracker.read().await;
                if tracker.has_executed(&window_id) {
                    continue;
                }
                tracker.get_best_signal(&window_id)
            };

            if let Some(pending) = best_signal {
                let can_execute = {
                    let tracker = self.window_tracker.read().await;
                    let current_exposure = tracker.get_exposure(&window_id);
                    let max_exposure = self.config.max_window_exposure_usd;

                    max_exposure == Decimal::ZERO
                        || current_exposure + pending.cost_usd <= max_exposure
                };

                if can_execute {
                    info!(
                        "🏆 Best edge selected: {} {} edge={:.2}% (window {})",
                        pending.signal.symbol,
                        pending.signal.direction,
                        pending.edge * dec!(100),
                        window_id
                    );

                    self.execute_pending_trade(pending.clone()).await?;

                    {
                        let mut tracker = self.window_tracker.write().await;
                        tracker.mark_executed(&window_id);
                        tracker.add_exposure(&window_id, pending.cost_usd);
                        tracker.clear_pending(&window_id);
                    }
                } else {
                    info!(
                        "⚠️ Window {} at exposure limit, skipping {}",
                        window_id, pending.signal.symbol
                    );

                    let mut tracker = self.window_tracker.write().await;
                    tracker.clear_pending(&window_id);
                }
            }
        }

        {
            let mut tracker = self.window_tracker.write().await;
            tracker.cleanup_old();
        }

        Ok(())
    }

    /// Execute a pending trade
    async fn execute_pending_trade(&self, pending: PendingSignal) -> Result<()> {
        let signal = &pending.signal;
        let event = &pending.event;
        let _entry_guard = self.entry_mutex.lock().await;

        {
            let positions = self.positions.read().await;
            if positions.values().any(|p| p.symbol == signal.symbol) {
                debug!("Already have position in {}, skipping", signal.symbol);
                return Ok(());
            }
            if positions
                .values()
                .any(|p| p.condition_id == event.condition_id)
            {
                debug!(
                    "Already have position in event {}, skipping",
                    event.condition_id
                );
                return Ok(());
            }
        }

        let base_shares = if let Some(ref fm) = self.fund_manager {
            match fm
                .can_open_position(&event.condition_id, &signal.symbol, signal.pm_price)
                .await
            {
                Ok(PositionSizeResult::Approved { shares, amount_usd }) => {
                    info!(
                        "💰 Fund manager approved: {} shares @ {:.2}¢ = ${:.2}",
                        shares,
                        signal.pm_price * dec!(100),
                        amount_usd
                    );
                    shares
                }
                Ok(PositionSizeResult::Rejected(reason)) => {
                    debug!("Fund manager rejected: {}", reason);
                    return Ok(());
                }
                Err(e) => {
                    warn!("Fund manager error: {}, skipping trade for safety", e);
                    return Ok(());
                }
            }
        } else {
            self.config.shares_per_trade
        };
        let shares_to_trade = self.apply_signal_position_sizing(base_shares, signal);
        if shares_to_trade < 5 {
            debug!(
                "Position size {} below Polymarket minimum 5 shares (base={})",
                shares_to_trade, base_shares
            );
            return Ok(());
        }

        let token_id = match signal.direction {
            Direction::Up => &event.up_token_id,
            Direction::Down => &event.down_token_id,
        };

        if self.dry_run {
            info!(
                "[DRY RUN] Best edge trade: {} {} {} shares @ {:.2}¢",
                signal.symbol,
                signal.direction,
                shares_to_trade,
                signal.pm_price * dec!(100)
            );
        } else {
            let order = OrderRequest::buy_limit(
                token_id.clone(),
                signal.direction.into(),
                shares_to_trade,
                signal.pm_price,
            );

            match self.executor.execute(&order).await {
                Ok(result) => {
                    let fill_price = result.avg_fill_price.unwrap_or(signal.pm_price);
                    let tracked_shares = if result.filled_shares > 0 {
                        result.filled_shares
                    } else {
                        shares_to_trade
                    };
                    let entry_notional = fill_price * Decimal::from(tracked_shares);
                    let trade_count = self.record_trade().await;

                    info!(
                        "Order filled: {} shares @ {:.2}¢ (trade #{} today)",
                        tracked_shares,
                        fill_price * dec!(100),
                        trade_count
                    );

                    if let Some(ref fm) = self.fund_manager {
                        fm.record_position_opened_with_amount(
                            &event.condition_id,
                            &signal.symbol,
                            entry_notional,
                        )
                        .await;
                    }

                    let position = Position {
                        token_id: token_id.clone(),
                        symbol: signal.symbol.clone(),
                        direction: signal.direction,
                        entry_price: fill_price,
                        entry_notional,
                        shares: tracked_shares,
                        entry_time: Utc::now(),
                        highest_price: fill_price,
                        event_end_time: event.end_time,
                        event_slug: event.slug.clone(),
                        condition_id: event.condition_id.clone(),
                        entry_p_hat: None,
                        window_open_price: None,
                    };

                    let mut positions = self.positions.write().await;
                    positions.insert(signal.symbol.clone(), position);

                    if let Some(ref logger) = self.trade_logger {
                        logger
                            .record_entry(
                                &signal.symbol,
                                &event.slug,
                                &event.condition_id,
                                &format!("{}", signal.direction),
                                fill_price,
                                tracked_shares,
                                signal.cex_move_pct,
                                signal.edge,
                            )
                            .await;
                    }

                    let mut last_trade = self.last_trade_time.write().await;
                    last_trade.insert(signal.symbol.clone(), Utc::now());
                }
                Err(e) => {
                    error!("Order failed: {}", e);
                }
            }
        }

        Ok(())
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
