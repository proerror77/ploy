//! Exchange order-update reconciliation for live staggered-arb orders.

use super::{StaggeredArbAdapter, StrategyAction, StrategyEvent, StrategyEventType};
use crate::domain::OrderStatus;
use crate::strategy::traits::OrderUpdate;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};

use super::lifecycle::{LiveOrderTrack, PaperPositionState};

const STALE_ORDER_SECS: i64 = 30;
const HARD_CLEANUP_SECS: i64 = 90;

impl StaggeredArbAdapter {
    pub(super) fn process_live_order_update(
        &mut self,
        update: &OrderUpdate,
    ) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        let client_id = match self.resolve_client_order_id(update) {
            Some(id) => id,
            None => return actions,
        };

        self.capture_exchange_order_id(&client_id, update);

        let track = match self.lookup_live_track(&client_id) {
            Some(track) => track,
            None => return actions,
        };

        let ts = update.timestamp;
        let fill_price = update.avg_fill_price.unwrap_or(track.price);
        let cumulative_filled = Self::effective_cumulative_filled_qty(&track, update);
        let filled_delta = Self::incremental_filled_shares(&track, update);

        match update.status {
            OrderStatus::Filled => self.handle_filled_order_update(
                &client_id,
                &track,
                ts,
                fill_price,
                cumulative_filled,
                filled_delta,
                &mut actions,
            ),
            OrderStatus::Cancelled | OrderStatus::Failed => {
                if self.should_wait_for_cancel_poll(update, &track) {
                    return actions;
                }
                self.update_balance_failure_pause(update);
                self.handle_terminal_order_update(
                    &client_id,
                    &track,
                    update,
                    ts,
                    fill_price,
                    cumulative_filled,
                    filled_delta,
                    &mut actions,
                );
            }
            OrderStatus::PartiallyFilled => self.handle_partial_order_update(
                &client_id,
                &track,
                update,
                ts,
                fill_price,
                cumulative_filled,
                filled_delta,
                &mut actions,
            ),
            OrderStatus::Submitted => actions.push(Self::order_update_log_event(
                &track,
                update,
                StrategyEventType::StateChanged,
            )),
            _ => {}
        }

        actions
    }

    pub(super) fn reconcile_stale_live_orders(
        &mut self,
        now: DateTime<Utc>,
    ) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        let cancel_ids: Vec<String> = self
            .live_orders
            .iter()
            .filter(|(_, track)| {
                track.cancel_requested_at.is_none()
                    && (now - track.submitted_at).num_seconds() > STALE_ORDER_SECS
            })
            .map(|(id, _)| id.clone())
            .collect();
        for client_id in &cancel_ids {
            if let Some(track) = self.live_orders.get_mut(client_id) {
                let cancel_id = track
                    .exchange_order_id
                    .clone()
                    .unwrap_or_else(|| client_id.clone());
                info!(
                    "[STAG-ARB] STALE ORDER CANCEL leg={} {} {} age={}s price={:.2}¢ exchange_id={}",
                    track.leg,
                    track.symbol,
                    track.event_id,
                    (now - track.submitted_at).num_seconds(),
                    track.price * dec!(100),
                    cancel_id,
                );
                track.cancel_requested_at = Some(now);
                actions.push(StrategyAction::CancelOrder {
                    order_id: cancel_id,
                });
            }
        }

        let orphan_ids: Vec<String> = self
            .live_orders
            .iter()
            .filter(|(_, track)| {
                track.cancel_requested_at.is_some()
                    && (now - track.submitted_at).num_seconds() > HARD_CLEANUP_SECS
            })
            .map(|(id, _)| id.clone())
            .collect();
        for client_id in orphan_ids {
            if let Some(track) = self.live_orders.remove(&client_id) {
                warn!(
                    "[STAG-ARB] ORPHAN ORDER ARCHIVE leg={} {} {} age={}s — no callback received, keeping lock for reconciliation",
                    track.leg,
                    track.symbol,
                    track.event_id,
                    (now - track.submitted_at).num_seconds(),
                );
                self.archived_live_orders.insert(client_id, track);
            }
        }

        actions
    }

    fn resolve_client_order_id(&self, update: &OrderUpdate) -> Option<String> {
        match &update.client_order_id {
            Some(id) => Some(id.clone()),
            None => self
                .live_orders
                .iter()
                .chain(self.archived_live_orders.iter())
                .find(|(_, track)| track.exchange_order_id.as_deref() == Some(&update.order_id))
                .map(|(id, _)| id.clone()),
        }
    }

    fn capture_exchange_order_id(&mut self, client_id: &str, update: &OrderUpdate) {
        if update.order_id.is_empty() {
            return;
        }

        if let Some(track) = self.live_orders.get_mut(client_id) {
            if track.exchange_order_id.is_none() {
                track.exchange_order_id = Some(update.order_id.clone());
            }
        } else if let Some(track) = self.archived_live_orders.get_mut(client_id) {
            if track.exchange_order_id.is_none() {
                track.exchange_order_id = Some(update.order_id.clone());
            }
        }
    }

    fn lookup_live_track(&self, client_id: &str) -> Option<LiveOrderTrack> {
        self.live_orders
            .get(client_id)
            .or_else(|| self.archived_live_orders.get(client_id))
            .cloned()
    }

    fn should_wait_for_cancel_poll(&self, update: &OrderUpdate, track: &LiveOrderTrack) -> bool {
        if update.status == OrderStatus::Cancelled
            && update.client_order_id.is_none()
            && update.filled_qty == 0
            && track.cancel_requested_at.is_some()
        {
            debug!(
                "[STAG-ARB] LEG{} cancel ack without fill details {} {} — waiting for poll reconciliation",
                track.leg, track.symbol, track.event_id
            );
            return true;
        }

        false
    }

    fn update_balance_failure_pause(&mut self, update: &OrderUpdate) {
        let is_balance_error = update
            .error
            .as_ref()
            .map(|error| error.contains("not enough balance") || error.contains("allowance"))
            .unwrap_or(false);

        if is_balance_error {
            self.consecutive_balance_failures += 1;
            if self.consecutive_balance_failures >= 3 && self.balance_pause_until.is_none() {
                let pause_secs = 90;
                self.balance_pause_until = Some(update.timestamp + Duration::seconds(pause_secs));
                info!(
                    "[STAG-ARB] Balance insufficient ({} consecutive failures), pausing entries for {}s to let claimer recycle funds",
                    self.consecutive_balance_failures, pause_secs
                );
            }
        } else {
            self.consecutive_balance_failures = 0;
        }
    }

    fn handle_filled_order_update(
        &mut self,
        client_id: &str,
        track: &LiveOrderTrack,
        ts: DateTime<Utc>,
        fill_price: Decimal,
        cumulative_filled: u64,
        filled_delta: u64,
        actions: &mut Vec<StrategyAction>,
    ) {
        if track.leg == 1 {
            let position_idx = if filled_delta > 0 {
                Some(self.record_leg1_fill(track, filled_delta, fill_price, ts, actions))
            } else {
                track.position_idx
            };
            self.update_order_fill_progress(client_id, cumulative_filled, position_idx);
        } else {
            self.handle_filled_leg2(
                client_id,
                track,
                ts,
                fill_price,
                cumulative_filled,
                filled_delta,
                actions,
            );
        }

        self.remove_order_tracking(client_id);
    }

    fn handle_filled_leg2(
        &mut self,
        client_id: &str,
        track: &LiveOrderTrack,
        ts: DateTime<Utc>,
        fill_price: Decimal,
        cumulative_filled: u64,
        filled_delta: u64,
        actions: &mut Vec<StrategyAction>,
    ) {
        let Some(idx) = track.position_idx else {
            return;
        };

        self.pending_leg2_positions.remove(&idx);

        if idx >= self.positions.len() {
            return;
        }

        if self.positions[idx].state != PaperPositionState::Leg1Filled {
            self.remove_order_tracking(client_id);
            return;
        }

        let close_reason = track.close_reason.as_deref().unwrap_or("merge").to_string();
        let total_filled = if filled_delta > 0 {
            self.record_leg2_fill(idx, filled_delta, fill_price, ts)
        } else {
            Self::leg2_filled_shares(&self.positions[idx])
        };
        self.update_order_fill_progress(client_id, cumulative_filled, Some(idx));
        let target = self.positions[idx].leg1_shares;

        if total_filled >= target {
            self.finalize_leg2_position(idx, close_reason.as_str(), ts, actions);
        } else {
            let symbol = self.positions[idx].symbol.clone();
            let avg = self.positions[idx].leg2_price.unwrap_or(fill_price);
            info!(
                "[STAG-ARB] LEG2 PARTIAL FILL {} {}/{} shares avg={:.2}¢",
                symbol,
                total_filled,
                target,
                avg * dec!(100)
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_terminal_order_update(
        &mut self,
        client_id: &str,
        track: &LiveOrderTrack,
        update: &OrderUpdate,
        ts: DateTime<Utc>,
        fill_price: Decimal,
        cumulative_filled: u64,
        filled_delta: u64,
        actions: &mut Vec<StrategyAction>,
    ) {
        let position_idx = track.position_idx;

        if track.leg == 1 {
            if filled_delta > 0 {
                warn!(
                    "[STAG-ARB] LEG1 {:?} but partially filled: {} {} shares={} avg={:.2}¢",
                    update.status,
                    track.symbol,
                    track.event_id,
                    filled_delta,
                    fill_price * dec!(100)
                );
                let idx = self.record_leg1_fill(track, filled_delta, fill_price, ts, actions);
                self.update_order_fill_progress(client_id, cumulative_filled, Some(idx));
            } else if position_idx.is_none() {
                self.pending_leg1_events.remove(&track.event_id);
                info!(
                    "[STAG-ARB] LEG1 {:?} {} {} — cleared for re-entry",
                    update.status, track.symbol, track.event_id,
                );
                actions.push(StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Error,
                        format!(
                            "[STAG-ARB] LEG1 {:?} {} {}",
                            update.status, track.symbol, track.event_id
                        ),
                    ),
                });
            }
        } else if let Some(idx) = position_idx {
            self.pending_leg2_positions.remove(&idx);
            if idx < self.positions.len()
                && self.positions[idx].state != PaperPositionState::Leg1Filled
            {
                self.remove_order_tracking(client_id);
                return;
            }
            if filled_delta > 0 {
                let total_filled = self.record_leg2_fill(idx, filled_delta, fill_price, ts);
                self.update_order_fill_progress(client_id, cumulative_filled, Some(idx));
                let target = self
                    .positions
                    .get(idx)
                    .map(|position| position.leg1_shares)
                    .unwrap_or(filled_delta);
                warn!(
                    "[STAG-ARB] LEG2 {:?} {} had partial fill shares={} total={}/{} before closure",
                    update.status, track.symbol, filled_delta, total_filled, target
                );
                if total_filled >= target {
                    let close_reason = track.close_reason.as_deref().unwrap_or("merge").to_string();
                    self.finalize_leg2_position(idx, close_reason.as_str(), ts, actions);
                } else {
                    info!(
                        "[STAG-ARB] LEG2 {:?} {} — will retry on next tick (filled {}/{})",
                        update.status, track.symbol, total_filled, target
                    );
                }
            } else {
                let (filled, target) = self
                    .positions
                    .get(idx)
                    .map(|position| (Self::leg2_filled_shares(position), position.leg1_shares))
                    .unwrap_or((0, 0));
                info!(
                    "[STAG-ARB] LEG2 {:?} {} — will retry on next tick (filled {}/{})",
                    update.status, track.symbol, filled, target
                );
            }
            actions.push(StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::Error,
                    format!("[STAG-ARB] LEG2 {:?} {}", update.status, track.symbol),
                ),
            });
        }

        self.remove_order_tracking(client_id);
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_partial_order_update(
        &mut self,
        client_id: &str,
        track: &LiveOrderTrack,
        update: &OrderUpdate,
        ts: DateTime<Utc>,
        fill_price: Decimal,
        cumulative_filled: u64,
        filled_delta: u64,
        actions: &mut Vec<StrategyAction>,
    ) {
        if track.leg == 1 {
            let position_idx = if filled_delta > 0 {
                Some(self.record_leg1_fill(track, filled_delta, fill_price, ts, actions))
            } else {
                track.position_idx
            };
            self.update_order_fill_progress(client_id, cumulative_filled, position_idx);

            if filled_delta > 0 {
                let cancel_id = if let Some(track_mut) = self.live_orders.get_mut(client_id) {
                    if track_mut.cancel_requested_at.is_none() {
                        track_mut.cancel_requested_at = Some(ts);
                        Some(
                            track_mut
                                .exchange_order_id
                                .clone()
                                .unwrap_or_else(|| client_id.to_string()),
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(order_id) = cancel_id {
                    info!(
                        "[STAG-ARB] LEG1 PARTIAL ACCEPT {} {} cumulative={}/{} — cancelling remainder",
                        track.symbol, track.event_id, cumulative_filled, track.shares,
                    );
                    actions.push(StrategyAction::CancelOrder { order_id });
                }
            }
        } else if let Some(idx) = track.position_idx {
            if idx < self.positions.len()
                && self.positions[idx].state != PaperPositionState::Leg1Filled
            {
                self.remove_order_tracking(client_id);
                return;
            }
            if filled_delta > 0 {
                let total_filled = self.record_leg2_fill(idx, filled_delta, fill_price, ts);
                self.update_order_fill_progress(client_id, cumulative_filled, Some(idx));
                let target = self
                    .positions
                    .get(idx)
                    .map(|position| position.leg1_shares)
                    .unwrap_or(0);
                info!(
                    "[STAG-ARB] LEG2 PARTIAL FILL {} {}/{} shares avg={:.2}¢",
                    track.symbol,
                    total_filled,
                    target,
                    self.positions[idx].leg2_price.unwrap_or(fill_price) * dec!(100)
                );
            }
        }

        actions.push(Self::order_update_log_event(
            track,
            update,
            StrategyEventType::StateChanged,
        ));
    }

    fn order_update_log_event(
        track: &LiveOrderTrack,
        update: &OrderUpdate,
        event_type: StrategyEventType,
    ) -> StrategyAction {
        StrategyAction::LogEvent {
            event: StrategyEvent::new(
                event_type,
                format!(
                    "[STAG-ARB] ORDER {:?} leg={} event={} symbol={} filled={} avg={:?}",
                    update.status,
                    track.leg,
                    track.event_id,
                    track.symbol,
                    update.filled_qty,
                    update.avg_fill_price
                ),
            ),
        }
    }
}
