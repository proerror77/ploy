use super::{
    Direction, LiveWindow, StaggeredArbAdapter, StrategyAction, StrategyEvent, StrategyEventType,
};
use crate::domain::OrderStatus;
use crate::strategy::traits::OrderUpdate;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{info, warn};

/// Paper position state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PaperPositionState {
    Leg1Filled,
    Merged,
    Settled,
    ForcedComplete,
}

/// A paper position tracking the two-leg arb lifecycle.
#[derive(Debug, Clone)]
pub(super) struct PaperPosition {
    pub(super) symbol: String,
    pub(super) event_id: String,
    pub(super) condition_id: Option<String>,
    pub(super) up_token: String,
    pub(super) down_token: String,
    pub(super) leg1_direction: Direction,
    pub(super) leg1_price: Decimal,
    pub(super) leg1_shares: u64,
    pub(super) leg1_fee: Decimal,
    pub(super) leg1_time: DateTime<Utc>,
    pub(super) entry_obi: Option<f64>,
    pub(super) protective_stop_armed_at: Option<DateTime<Utc>>,
    pub(super) wait_deadline: DateTime<Utc>,
    pub(super) leg2_price: Option<Decimal>,
    pub(super) leg2_shares: Option<u64>,
    pub(super) leg2_fee: Option<Decimal>,
    pub(super) leg2_time: Option<DateTime<Utc>>,
    pub(super) state: PaperPositionState,
}

/// A closed paper trade for summary reporting.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(super) struct PaperTrade {
    pub(super) symbol: String,
    pub(super) event_id: String,
    pub(super) direction: Direction,
    pub(super) leg1_price: Decimal,
    pub(super) leg2_price: Decimal,
    pub(super) total_cost: Decimal,
    pub(super) payout: Decimal,
    pub(super) pnl: Decimal,
    pub(super) exit_reason: String,
    pub(super) duration_secs: i64,
    pub(super) opened_at: DateTime<Utc>,
    pub(super) closed_at: DateTime<Utc>,
}

/// Tracks an in-flight order for the live (non-paper) path.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(super) struct LiveOrderTrack {
    pub(super) event_id: String,
    pub(super) condition_id: Option<String>,
    pub(super) symbol: String,
    pub(super) up_token: String,
    pub(super) down_token: String,
    pub(super) direction: Direction,
    pub(super) token_id: String,
    pub(super) leg: u8,
    pub(super) price: Decimal,
    pub(super) shares: u64,
    pub(super) position_idx: Option<usize>,
    pub(super) close_reason: Option<String>,
    pub(super) submitted_at: DateTime<Utc>,
    pub(super) cancel_requested_at: Option<DateTime<Utc>>,
    pub(super) exchange_order_id: Option<String>,
    pub(super) acknowledged_filled_qty: u64,
    pub(super) entry_obi: Option<f64>,
}

impl StaggeredArbAdapter {
    pub(super) fn effective_cumulative_filled_qty(
        track: &LiveOrderTrack,
        update: &OrderUpdate,
    ) -> u64 {
        if update.filled_qty > 0 {
            update.filled_qty.min(track.shares)
        } else if update.status == OrderStatus::Filled && track.acknowledged_filled_qty == 0 {
            track.shares
        } else {
            track.acknowledged_filled_qty.min(track.shares)
        }
    }

    pub(super) fn incremental_filled_shares(track: &LiveOrderTrack, update: &OrderUpdate) -> u64 {
        let cumulative = Self::effective_cumulative_filled_qty(track, update);
        cumulative.saturating_sub(track.acknowledged_filled_qty)
    }

    pub(super) fn update_order_fill_progress(
        &mut self,
        client_id: &str,
        cumulative_filled_qty: u64,
        position_idx: Option<usize>,
    ) {
        if let Some(track) = self.live_orders.get_mut(client_id) {
            track.acknowledged_filled_qty = cumulative_filled_qty
                .min(track.shares)
                .max(track.acknowledged_filled_qty);
            if let Some(idx) = position_idx {
                track.position_idx = Some(idx);
            }
        } else if let Some(track) = self.archived_live_orders.get_mut(client_id) {
            track.acknowledged_filled_qty = cumulative_filled_qty
                .min(track.shares)
                .max(track.acknowledged_filled_qty);
            if let Some(idx) = position_idx {
                track.position_idx = Some(idx);
            }
        }
    }

    pub(super) fn remove_order_tracking(&mut self, client_id: &str) -> Option<LiveOrderTrack> {
        self.live_orders
            .remove(client_id)
            .or_else(|| self.archived_live_orders.remove(client_id))
    }

    pub(super) fn clear_order_tracking_for_event(&mut self, event_id: &str) {
        let live_ids: Vec<String> = self
            .live_orders
            .iter()
            .filter(|(_, track)| track.event_id == event_id)
            .map(|(id, _)| id.clone())
            .collect();
        for client_id in live_ids {
            if let Some(track) = self.live_orders.remove(&client_id) {
                if track.leg == 1 {
                    self.pending_leg1_events.remove(&track.event_id);
                } else if let Some(idx) = track.position_idx {
                    self.pending_leg2_positions.remove(&idx);
                }
            }
        }

        let archived_ids: Vec<String> = self
            .archived_live_orders
            .iter()
            .filter(|(_, track)| track.event_id == event_id)
            .map(|(id, _)| id.clone())
            .collect();
        for client_id in archived_ids {
            if let Some(track) = self.archived_live_orders.remove(&client_id) {
                if track.leg == 1 {
                    self.pending_leg1_events.remove(&track.event_id);
                } else if let Some(idx) = track.position_idx {
                    self.pending_leg2_positions.remove(&idx);
                }
            }
        }
    }

    pub(super) fn leg2_filled_shares(pos: &PaperPosition) -> u64 {
        pos.leg2_shares.unwrap_or(0)
    }

    pub(super) fn leg2_remaining_shares(pos: &PaperPosition) -> u64 {
        pos.leg1_shares
            .saturating_sub(Self::leg2_filled_shares(pos))
    }

    pub(super) fn record_leg2_fill(
        &mut self,
        idx: usize,
        filled_shares: u64,
        fill_price: Decimal,
        ts: DateTime<Utc>,
    ) -> u64 {
        if filled_shares == 0 || idx >= self.positions.len() {
            return 0;
        }

        let pos = &mut self.positions[idx];
        let prev_shares = pos.leg2_shares.unwrap_or(0);
        let prev_avg_price = pos.leg2_price.unwrap_or(Decimal::ZERO);
        let prev_fee = pos.leg2_fee.unwrap_or(Decimal::ZERO);

        let add_shares = filled_shares.min(pos.leg1_shares.saturating_sub(prev_shares));
        if add_shares == 0 {
            return prev_shares;
        }

        let prev_notional = prev_avg_price * Decimal::from(prev_shares);
        let add_notional = fill_price * Decimal::from(add_shares);
        let total_shares = prev_shares + add_shares;
        let total_notional = prev_notional + add_notional;
        let avg_price = total_notional / Decimal::from(total_shares);
        let total_fee = prev_fee + fill_price * Decimal::from(add_shares) * self.config.fee_rate;

        pos.leg2_shares = Some(total_shares);
        pos.leg2_price = Some(avg_price);
        pos.leg2_fee = Some(total_fee);
        pos.leg2_time = Some(ts);

        total_shares
    }

    pub(super) fn finalize_leg2_position(
        &mut self,
        idx: usize,
        close_reason: &str,
        ts: DateTime<Utc>,
        actions: &mut Vec<StrategyAction>,
    ) {
        if idx >= self.positions.len() {
            return;
        }

        let (
            symbol,
            event_id,
            direction,
            leg1_price,
            leg1_shares,
            leg1_fee,
            leg1_time,
            leg2_avg_price,
            leg2_fee_total,
            leg2_filled,
        ) = {
            let pos = &self.positions[idx];
            (
                pos.symbol.clone(),
                pos.event_id.clone(),
                pos.leg1_direction.clone(),
                pos.leg1_price,
                pos.leg1_shares,
                pos.leg1_fee,
                pos.leg1_time,
                pos.leg2_price.unwrap_or(Decimal::ZERO),
                pos.leg2_fee.unwrap_or(Decimal::ZERO),
                pos.leg2_shares.unwrap_or(0),
            )
        };

        if leg2_filled < leg1_shares {
            return;
        }

        let total_cost = Decimal::from(leg1_shares) * leg1_price
            + leg1_fee
            + Decimal::from(leg1_shares) * leg2_avg_price
            + leg2_fee_total;
        let payout = Decimal::from(leg1_shares);
        let pnl = payout - total_cost;
        let duration_secs = (ts - leg1_time).num_seconds();
        let exit_reason = if close_reason == "merge" {
            "live_leg2_complete".to_string()
        } else {
            "live_forced".to_string()
        };

        let pos = &mut self.positions[idx];
        pos.state = if close_reason == "merge" {
            PaperPositionState::Merged
        } else {
            PaperPositionState::ForcedComplete
        };
        pos.leg2_time = Some(ts);

        self.closed_trades.push(PaperTrade {
            symbol: symbol.clone(),
            event_id,
            direction,
            leg1_price,
            leg2_price: leg2_avg_price,
            total_cost,
            payout,
            pnl,
            exit_reason,
            duration_secs,
            opened_at: leg1_time,
            closed_at: ts,
        });

        let tag = if close_reason == "merge" {
            "COMPLETE"
        } else {
            "FORCED"
        };
        info!(
            "[STAG-ARB] LEG2 {} FILLED {} cost=${:.4} pnl={}{:.4} wait={}s",
            tag,
            symbol,
            total_cost,
            if pnl >= Decimal::ZERO { "+" } else { "" },
            pnl,
            duration_secs,
        );
        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::CycleCompleted,
                format!(
                    "[STAG-ARB] LEG2 {} FILLED {} pnl={}{:.4} wait={}s",
                    tag,
                    symbol,
                    if pnl >= Decimal::ZERO { "+" } else { "" },
                    pnl,
                    duration_secs
                ),
            ),
        });
    }

    pub(super) fn settle_expired_event(
        &mut self,
        window: &LiveWindow,
        ts: DateTime<Utc>,
        actions: &mut Vec<StrategyAction>,
    ) {
        let strike = match window.open_price {
            Some(price) if price > Decimal::ZERO => price,
            _ => {
                warn!(
                    "[STAG-ARB] EVENT EXPIRED {} {} without open anchor; keeping position open",
                    window.symbol, window.event_id
                );
                return;
            }
        };
        let settle_spot = match self.spot_prices.get(&window.symbol) {
            Some(spot) => spot.price,
            None => {
                warn!(
                    "[STAG-ARB] EVENT EXPIRED {} {} without final spot; keeping position open",
                    window.symbol, window.event_id
                );
                return;
            }
        };

        let up_won = settle_spot > strike;
        let mut to_settle: Vec<usize> = self
            .positions
            .iter()
            .enumerate()
            .filter(|(_, pos)| {
                pos.event_id == window.event_id && pos.state == PaperPositionState::Leg1Filled
            })
            .map(|(idx, _)| idx)
            .collect();
        to_settle.sort_by(|a, b| b.cmp(a));

        for idx in to_settle {
            self.settle_expired_position(idx, up_won, settle_spot, ts, actions);
        }

        self.clear_order_tracking_for_event(&window.event_id);
    }

    pub(super) fn settle_expired_position(
        &mut self,
        idx: usize,
        up_won: bool,
        settle_spot: Decimal,
        ts: DateTime<Utc>,
        actions: &mut Vec<StrategyAction>,
    ) {
        if idx >= self.positions.len() {
            return;
        }

        let (
            symbol,
            event_id,
            direction,
            leg1_price,
            leg1_shares,
            leg1_fee,
            leg1_time,
            leg2_price,
            leg2_shares,
            leg2_fee,
            winner_matches_leg1,
        ) = {
            let pos = &self.positions[idx];
            let winner_matches_leg1 = matches!(pos.leg1_direction, Direction::Up) == up_won;
            (
                pos.symbol.clone(),
                pos.event_id.clone(),
                pos.leg1_direction.clone(),
                pos.leg1_price,
                pos.leg1_shares,
                pos.leg1_fee,
                pos.leg1_time,
                pos.leg2_price.unwrap_or(Decimal::ZERO),
                pos.leg2_shares.unwrap_or(0),
                pos.leg2_fee.unwrap_or(Decimal::ZERO),
                winner_matches_leg1,
            )
        };

        let payout = if winner_matches_leg1 {
            Decimal::from(leg1_shares)
        } else {
            Decimal::from(leg2_shares)
        };
        let total_cost = Decimal::from(leg1_shares) * leg1_price
            + leg1_fee
            + Decimal::from(leg2_shares) * leg2_price
            + leg2_fee;
        let pnl = payout - total_cost;
        let duration_secs = (ts - leg1_time).num_seconds();

        self.pending_leg2_positions.remove(&idx);
        let pos = &mut self.positions[idx];
        pos.state = PaperPositionState::Settled;
        pos.leg2_time = Some(pos.leg2_time.unwrap_or(ts));

        self.closed_trades.push(PaperTrade {
            symbol: symbol.clone(),
            event_id,
            direction,
            leg1_price,
            leg2_price,
            total_cost,
            payout,
            pnl,
            exit_reason: "live_settlement".to_string(),
            duration_secs,
            opened_at: leg1_time,
            closed_at: ts,
        });

        info!(
            "[STAG-ARB] SETTLED {} spot={} payout=${:.4} pnl={}{:.4} wait={}s hedge={}/{}",
            symbol,
            settle_spot,
            payout,
            if pnl >= Decimal::ZERO { "+" } else { "" },
            pnl,
            duration_secs,
            leg2_shares,
            leg1_shares,
        );
        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::CycleCompleted,
                format!(
                    "[STAG-ARB] SETTLED {} payout=${:.4} pnl={}{:.4} wait={}s hedge={}/{}",
                    symbol,
                    payout,
                    if pnl >= Decimal::ZERO { "+" } else { "" },
                    pnl,
                    duration_secs,
                    leg2_shares,
                    leg1_shares,
                ),
            ),
        });
    }

    pub(super) fn append_leg1_fill_to_position(
        &mut self,
        idx: usize,
        filled_shares: u64,
        fill_price: Decimal,
        _ts: DateTime<Utc>,
    ) -> u64 {
        if filled_shares == 0 || idx >= self.positions.len() {
            return 0;
        }

        let pos = &mut self.positions[idx];
        let prev_shares = pos.leg1_shares;
        let prev_avg_price = pos.leg1_price;
        let prev_fee = pos.leg1_fee;

        let prev_notional = prev_avg_price * Decimal::from(prev_shares);
        let add_notional = fill_price * Decimal::from(filled_shares);
        let total_shares = prev_shares + filled_shares;
        let total_notional = prev_notional + add_notional;
        let avg_price = total_notional / Decimal::from(total_shares);
        let total_fee = prev_fee + fill_price * Decimal::from(filled_shares) * self.config.fee_rate;

        pos.leg1_shares = total_shares;
        pos.leg1_price = avg_price;
        pos.leg1_fee = total_fee;

        total_shares
    }

    pub(super) fn record_leg1_fill(
        &mut self,
        track: &LiveOrderTrack,
        filled_shares: u64,
        fill_price: Decimal,
        ts: DateTime<Utc>,
        actions: &mut Vec<StrategyAction>,
    ) -> usize {
        if filled_shares == 0 {
            return track.position_idx.unwrap_or(usize::MAX);
        }

        if let Some(idx) = track.position_idx {
            let total_shares =
                self.append_leg1_fill_to_position(idx, filled_shares, fill_price, ts);
            info!(
                "[STAG-ARB] LEG1 FILL ADD {} {} @ {:.2}¢ total_shares={}",
                track.symbol,
                track.direction,
                fill_price * dec!(100),
                total_shares,
            );
            actions.push(StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::OrderFilled,
                    format!(
                        "[STAG-ARB] LEG1 FILL ADD {} {} @ {:.2}¢ shares={} total={}",
                        track.symbol,
                        track.direction,
                        fill_price * dec!(100),
                        filled_shares,
                        total_shares
                    ),
                ),
            });
            return idx;
        }

        self.pending_leg1_events.remove(&track.event_id);
        *self
            .event_trade_counts
            .entry(track.event_id.clone())
            .or_default() += 1;

        let bc = &self.config.backtest_config;
        let window_end = self
            .active_windows
            .get(&track.symbol)
            .and_then(|ws| ws.iter().find(|w| w.event_id == track.event_id))
            .map(|w| w.end_time)
            .unwrap_or(ts + chrono::Duration::seconds(300));

        let window_duration = (window_end - ts).num_seconds() as f64;
        let max_wait_by_pct = (window_duration * bc.max_wait_pct) as i64;
        let max_wait = (bc.max_wait_secs as i64).min(max_wait_by_pct);
        let wait_deadline = ts + chrono::Duration::seconds(max_wait);

        let leg1_fee = fill_price * Decimal::from(filled_shares) * self.config.fee_rate;

        self.positions.push(PaperPosition {
            symbol: track.symbol.clone(),
            event_id: track.event_id.clone(),
            condition_id: track.condition_id.clone(),
            up_token: track.up_token.clone(),
            down_token: track.down_token.clone(),
            leg1_direction: track.direction.clone(),
            leg1_price: fill_price,
            leg1_shares: filled_shares,
            leg1_fee,
            leg1_time: ts,
            entry_obi: track.entry_obi,
            protective_stop_armed_at: None,
            wait_deadline,
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        if filled_shares < track.shares {
            warn!(
                "[STAG-ARB] LEG1 PARTIAL FILL {} {} @ {:.2}¢ ({}/{} shares)",
                track.symbol,
                track.direction,
                fill_price * dec!(100),
                filled_shares,
                track.shares,
            );
        } else {
            info!(
                "[STAG-ARB] LEG1 FILLED {} {} @ {:.2}¢ ({} shares)",
                track.symbol,
                track.direction,
                fill_price * dec!(100),
                filled_shares,
            );
        }

        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::OrderFilled,
                format!(
                    "[STAG-ARB] LEG1 FILLED {} {} @ {:.2}¢ shares={}",
                    track.symbol,
                    track.direction,
                    fill_price * dec!(100),
                    filled_shares
                ),
            ),
        });

        self.positions.len() - 1
    }
}
