use super::*;

/// An active event window being monitored for entry signals.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(super) struct LiveWindow {
    pub(super) event_id: String,
    pub(super) symbol: String,
    pub(super) up_token: String,
    pub(super) down_token: String,
    pub(super) condition_id: Option<String>,
    pub(super) end_time: DateTime<Utc>,
    pub(super) open_price: Option<Decimal>,
    pub(super) window_secs: u64,
}

#[derive(Debug, Clone)]
pub(super) struct QuoteRoute {
    pub(super) event_id: String,
    pub(super) symbol: String,
    pub(super) direction: Direction,
}

impl StaggeredArbAdapter {
    pub(super) fn estimated_live_locked_capital(&self) -> Decimal {
        let open_leg1: Decimal = self
            .positions
            .iter()
            .filter(|p| p.state == PaperPositionState::Leg1Filled)
            .map(|p| p.leg1_price * Decimal::from(p.leg1_shares) + p.leg1_fee)
            .sum();

        let pending_orders: Decimal = self
            .live_orders
            .values()
            .map(|track| {
                let notional = track.price * Decimal::from(track.shares);
                let fee = notional * self.config.fee_rate;
                notional + fee
            })
            .sum();

        open_leg1 + pending_orders
    }

    pub(super) fn available_balance_for_leg1(&self) -> Decimal {
        if self.dry_run {
            self.equity
        } else {
            (self.equity - self.estimated_live_locked_capital()).max(Decimal::ZERO)
        }
    }

    pub(super) fn current_sigma_for_symbol(
        &self,
        symbol: &str,
        bc: &StaggeredArbBacktestConfig,
    ) -> f64 {
        self.spot_prices
            .get(symbol)
            .and_then(|s| s.volatility(bc.vol_lookback_secs))
            .and_then(|v| v.to_f64())
            .map(|tick_vol| {
                let n = self
                    .spot_prices
                    .get(symbol)
                    .map(|s| s.history_len().min(5000) as f64)
                    .unwrap_or(100.0);
                (tick_vol * n.sqrt()).max(bc.vol_floor)
            })
            .unwrap_or(bc.vol_floor)
    }

    pub(super) fn record_pm_quote(
        &mut self,
        event_id: &str,
        direction: Direction,
        ask: Option<Decimal>,
        ask_size: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        let state = self
            .pm_quote_state_by_event
            .entry(event_id.to_string())
            .or_default();
        let side = match direction {
            Direction::Up => Side::Up,
            Direction::Down => Side::Down,
        };
        let side_state = state.side_mut(side);
        if self.config.backtest_config.pm_quote_max_stale_secs > 0 {
            if let Some(last_seen_at) = side_state.last_seen_at {
                if (ts - last_seen_at).num_seconds()
                    > self.config.backtest_config.pm_quote_max_stale_secs as i64
                {
                    side_state.clear();
                }
            }
        }
        state.update(side, ask, ask_size, ts);
        self.pm_asks_by_event
            .insert(event_id.to_string(), state.asks());
    }

    pub(super) fn event_quote_state(
        &self,
        event_id: &str,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) -> PmEventQuoteState {
        self.pm_quote_state_by_event
            .get(event_id)
            .copied()
            .unwrap_or_else(|| PmEventQuoteState::synthetic(up_ask, down_ask, ts))
    }

    /// Count currently active cycles (open Leg1 positions + pending Leg1 orders).
    pub(super) fn active_cycle_count(&self) -> usize {
        let open_positions = self
            .positions
            .iter()
            .filter(|p| p.state == PaperPositionState::Leg1Filled)
            .count();
        let pending_leg1 = self.pending_leg1_events.len();
        open_positions + pending_leg1
    }

    /// Check if a specific event already has an active cycle (open or pending).
    pub(super) fn has_active_cycle_for_event(&self, event_id: &str) -> bool {
        self.positions
            .iter()
            .any(|p| p.event_id == event_id && p.state == PaperPositionState::Leg1Filled)
            || self.pending_leg1_events.contains(event_id)
    }
}
