//! Dump & hedge engine entry, hedge completion, and stop-loss flow.

use super::{
    DumpHedgeEngine, HedgeResult, PendingHedge, ProgressiveHedgeSignal, StopLossReason,
    StopLossSignal,
};
use crate::domain::Side;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::{debug, info, warn};

impl DumpHedgeEngine {
    /// Check for dump and potential Leg 1 entry.
    pub async fn check_leg1_signal(
        &self,
        event_id: &str,
        symbol: &str,
        up_token_id: &str,
        down_token_id: &str,
        up_ask: Decimal,
        down_ask: Decimal,
        time_remaining_secs: i64,
    ) -> Option<crate::strategy::dump_hedge::EnhancedDumpSignal> {
        if time_remaining_secs < self.config.min_time_remaining_secs as i64 {
            return None;
        }

        {
            let pending = self.pending_hedges.read().await;
            if pending.contains_key(event_id) {
                return None;
            }
        }

        let tracker = self.price_tracker.read().await;

        if let Some(mut signal) = tracker.detect_dump(up_token_id, &self.config) {
            if up_ask <= self.config.max_leg1_price {
                let priority_bonus = (5 - self.config.get_priority(symbol).min(5)) as f64 * 0.05;
                signal.signal_strength = (signal.signal_strength + priority_bonus).min(1.0);
                return Some(signal);
            }
        }

        if let Some(mut signal) = tracker.detect_dump(down_token_id, &self.config) {
            if down_ask <= self.config.max_leg1_price {
                let priority_bonus = (5 - self.config.get_priority(symbol).min(5)) as f64 * 0.05;
                signal.signal_strength = (signal.signal_strength + priority_bonus).min(1.0);
                return Some(signal);
            }
        }

        None
    }

    /// Record a Leg 1 entry.
    pub async fn record_leg1(
        &self,
        event_id: &str,
        symbol: &str,
        leg1_token_id: &str,
        leg1_side: Side,
        leg1_price: Decimal,
        leg1_shares: u64,
        opposite_token_id: &str,
        time_remaining_secs: i64,
    ) {
        let pending = PendingHedge {
            event_id: event_id.to_string(),
            symbol: symbol.to_string(),
            leg1_token_id: leg1_token_id.to_string(),
            leg1_side,
            leg1_price,
            leg1_shares,
            leg1_time: Utc::now(),
            opposite_token_id: opposite_token_id.to_string(),
            hedged_shares: 0,
            avg_hedge_price: Decimal::ZERO,
            time_remaining_at_entry: time_remaining_secs,
        };

        let mut hedges = self.pending_hedges.write().await;
        hedges.insert(event_id.to_string(), pending);

        info!(
            "📝 Leg 1 recorded: {} {:?} @ {:.1}¢ x{} shares ({}s remaining)",
            symbol,
            leg1_side,
            leg1_price * dec!(100),
            leg1_shares,
            time_remaining_secs
        );
    }

    /// Check for hedge opportunity (supports progressive hedging).
    pub async fn check_hedge_signal(
        &self,
        event_id: &str,
        opposite_ask: Decimal,
        time_remaining_secs: i64,
    ) -> Option<ProgressiveHedgeSignal> {
        let pending = {
            let hedges = self.pending_hedges.read().await;
            hedges.get(event_id)?.clone()
        };

        if pending.is_fully_hedged() {
            return None;
        }

        let sum = pending.leg1_price + opposite_ask;
        let dynamic_target = self.config.dynamic_sum_target(time_remaining_secs);
        let is_urgent = time_remaining_secs < self.config.urgent_time_threshold_secs as i64;

        if sum <= dynamic_target {
            let remaining = pending.remaining_shares();
            let shares_to_hedge = if self.config.progressive_hedge && !is_urgent {
                remaining
                    .min(self.config.shares / 3)
                    .max(self.config.min_progressive_shares)
            } else {
                remaining
            };

            let locked_profit_pct = (dec!(1) - sum) / sum * dec!(100);

            info!(
                "✅ HEDGE {}: {} | Leg1={:.1}¢ + Leg2={:.1}¢ = {:.2} <= {:.2} | {} shares | Profit={:.1}%{}",
                if pending.hedged_shares > 0 { "PARTIAL" } else { "READY" },
                event_id,
                pending.leg1_price * dec!(100),
                opposite_ask * dec!(100),
                sum,
                dynamic_target,
                shares_to_hedge,
                locked_profit_pct,
                if is_urgent { " [URGENT]" } else { "" }
            );

            return Some(ProgressiveHedgeSignal {
                event_id: event_id.to_string(),
                pending,
                leg2_ask: opposite_ask,
                shares_to_hedge,
                sum,
                locked_profit_pct,
                is_urgent,
            });
        }

        debug!(
            "Hedge not ready: {} | {:.1}¢ + {:.1}¢ = {:.2} > {:.2}",
            event_id,
            pending.leg1_price * dec!(100),
            opposite_ask * dec!(100),
            sum,
            dynamic_target
        );

        None
    }

    /// Record partial hedge execution.
    pub async fn record_partial_hedge(
        &self,
        event_id: &str,
        shares_hedged: u64,
        hedge_price: Decimal,
    ) -> Option<HedgeResult> {
        let mut hedges = self.pending_hedges.write().await;

        let pending = hedges.get_mut(event_id)?;
        let total_hedged_before = pending.hedged_shares;
        let new_total = total_hedged_before + shares_hedged;

        pending.avg_hedge_price = if total_hedged_before == 0 {
            hedge_price
        } else {
            (pending.avg_hedge_price * Decimal::from(total_hedged_before)
                + hedge_price * Decimal::from(shares_hedged))
                / Decimal::from(new_total)
        };
        pending.hedged_shares = new_total;

        info!(
            "📊 Partial hedge: {} hedged {}/{} shares @ {:.1}¢ (avg {:.1}¢)",
            event_id,
            new_total,
            pending.leg1_shares,
            hedge_price * dec!(100),
            pending.avg_hedge_price * dec!(100)
        );

        if pending.is_fully_hedged() {
            let result = self.finalize_hedge(pending);
            hedges.remove(event_id);
            return Some(result);
        }

        None
    }

    /// Finalize a complete hedge.
    fn finalize_hedge(&self, pending: &PendingHedge) -> HedgeResult {
        let total_leg1_cost = pending.leg1_price * Decimal::from(pending.leg1_shares);
        let total_leg2_cost = pending.avg_hedge_price * Decimal::from(pending.leg1_shares);
        let total_cost = total_leg1_cost + total_leg2_cost;
        let payout = Decimal::from(pending.leg1_shares);
        let locked_profit = payout - total_cost;
        let locked_profit_pct = locked_profit / total_cost * dec!(100);

        info!(
            "🎉 HEDGE COMPLETE: {} | Cost=${:.2} | Payout=${:.2} | Profit=${:.2} ({:.1}%)",
            pending.event_id, total_cost, payout, locked_profit, locked_profit_pct
        );

        HedgeResult {
            event_id: pending.event_id.clone(),
            total_leg1_cost,
            total_leg2_cost,
            total_shares: pending.leg1_shares,
            locked_profit,
            locked_profit_pct,
        }
    }

    /// Check for positions that need stop-loss (failed hedge).
    pub async fn check_stop_loss(
        &self,
        current_prices: &HashMap<String, Decimal>,
    ) -> Vec<StopLossSignal> {
        let hedges = self.pending_hedges.read().await;
        let mut signals = Vec::new();

        for pending in hedges.values() {
            let elapsed = pending.elapsed_secs();
            let current_price = match current_prices.get(&pending.leg1_token_id) {
                Some(p) => *p,
                None => continue,
            };

            let loss_pct = if current_price < pending.leg1_price {
                (pending.leg1_price - current_price) / pending.leg1_price
            } else {
                Decimal::ZERO
            };

            if elapsed > self.config.max_hedge_wait_secs as i64 && !pending.is_fully_hedged() {
                warn!(
                    "⚠️ Hedge timeout: {} waited {}s, only {}/{} hedged",
                    pending.event_id, elapsed, pending.hedged_shares, pending.leg1_shares
                );
                signals.push(StopLossSignal {
                    event_id: pending.event_id.clone(),
                    pending: pending.clone(),
                    reason: StopLossReason::HedgeTimeout,
                    current_price,
                    loss_pct,
                });
                continue;
            }

            if loss_pct >= self.config.hedge_fail_stop_loss_pct {
                warn!(
                    "⚠️ Price crash: {} dropped {:.1}% from entry",
                    pending.event_id,
                    loss_pct * dec!(100)
                );
                signals.push(StopLossSignal {
                    event_id: pending.event_id.clone(),
                    pending: pending.clone(),
                    reason: StopLossReason::PriceCrash,
                    current_price,
                    loss_pct,
                });
            }
        }

        signals
    }
}
