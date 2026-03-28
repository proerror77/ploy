use crate::fills::{FillLedger, FillRecord};
use crate::intents::TradingIntent;
use crate::orders::OrderLedger;
use crate::pnl::PnlSnapshot;
use crate::positions::{PositionLedger, PositionSnapshot};
use crate::risk::{snapshot_from_state, RiskSnapshot};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TradingRuntimeSnapshot {
    pub intents: Vec<TradingIntent>,
    pub orders: Vec<crate::orders::OrderRecord>,
    pub fills: Vec<FillRecord>,
    pub positions: Vec<PositionSnapshot>,
    pub pnl: PnlSnapshot,
    pub risk: RiskSnapshot,
}

#[derive(Debug, Default)]
pub struct TradingRuntime {
    intents: Vec<TradingIntent>,
    orders: OrderLedger,
    fills: FillLedger,
    positions: PositionLedger,
}

impl TradingRuntime {
    pub fn restore(snapshot: TradingRuntimeSnapshot) -> Self {
        let mut positions = PositionLedger::default();
        for fill in &snapshot.fills {
            positions.apply_fill(fill);
        }

        Self {
            intents: snapshot.intents,
            orders: OrderLedger::restore(snapshot.orders),
            fills: FillLedger::restore(snapshot.fills),
            positions,
        }
    }

    pub fn submit_intent(
        &mut self,
        intent: TradingIntent,
        order_id: impl Into<String>,
    ) -> &crate::orders::OrderRecord {
        self.intents.push(intent.clone());
        self.orders.insert_from_intent(order_id, &intent)
    }

    pub fn acknowledge_order(
        &mut self,
        order_id: &str,
        venue_order_id: impl Into<String>,
    ) -> Option<&crate::orders::OrderRecord> {
        self.orders.acknowledge(order_id, venue_order_id)
    }

    pub fn replace_order(
        &mut self,
        order_id: &str,
        requested_qty: Decimal,
        limit_price: Option<Decimal>,
        venue_order_id: impl Into<String>,
    ) -> Option<&crate::orders::OrderRecord> {
        self.orders
            .replace(order_id, requested_qty, limit_price, venue_order_id)
    }

    pub fn reject_order(
        &mut self,
        order_id: &str,
        reason: impl Into<String>,
    ) -> Option<&crate::orders::OrderRecord> {
        self.orders.reject(order_id, reason)
    }

    pub fn record_order_error(
        &mut self,
        order_id: &str,
        error: impl Into<String>,
    ) -> Option<&crate::orders::OrderRecord> {
        self.orders.record_error(order_id, error)
    }

    pub fn cancel_order(&mut self, order_id: &str) -> Option<&crate::orders::OrderRecord> {
        self.orders.cancel(order_id)
    }

    pub fn order(&self, order_id: &str) -> Option<&crate::orders::OrderRecord> {
        self.orders.order(order_id)
    }

    pub fn intent(&self, intent_id: &str) -> Option<&TradingIntent> {
        self.intents
            .iter()
            .find(|intent| intent.intent_id == intent_id)
    }

    pub fn record_fill(&mut self, fill: FillRecord) -> bool {
        if self.fills.contains(&fill.fill_id) {
            return false;
        }
        self.orders.apply_fill(&fill);
        self.positions.apply_fill(&fill);
        self.fills.record(fill);
        true
    }

    pub fn last_fill_time(&self) -> Option<DateTime<Utc>> {
        self.fills.all().iter().map(|fill| fill.timestamp).max()
    }

    /// Read-only access to the position ledger.
    pub fn positions(&self) -> &PositionLedger {
        &self.positions
    }

    /// Read-only access to the order ledger.
    pub fn orders(&self) -> &OrderLedger {
        &self.orders
    }

    pub fn snapshot(&self, mark_prices: &BTreeMap<String, Decimal>) -> TradingRuntimeSnapshot {
        let orders = self.orders.orders().cloned().collect::<Vec<_>>();
        let active_intents = self
            .intents
            .iter()
            .filter(|intent| {
                orders.iter().any(|order| {
                    order.intent_id == intent.intent_id
                        && matches!(
                            order.state,
                            crate::orders::OrderState::Pending
                                | crate::orders::OrderState::Acknowledged
                                | crate::orders::OrderState::PartiallyFilled
                        )
                })
            })
            .cloned()
            .collect::<Vec<_>>();

        TradingRuntimeSnapshot {
            intents: self.intents.clone(),
            orders,
            fills: self.fills.all().to_vec(),
            positions: self.positions.positions().cloned().collect(),
            pnl: self.positions.pnl_snapshot(mark_prices),
            risk: snapshot_from_state(&active_intents, &self.orders, &self.positions),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TradingRuntime;
    use crate::{FillRecord, IntentPurpose, OrderRecord, OrderState, TradeSide, TradingIntent};
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;

    #[test]
    fn restore_rebuilds_positions_and_active_risk_from_snapshot() {
        let snapshot = super::TradingRuntimeSnapshot {
            intents: vec![TradingIntent {
                intent_id: "intent-1".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "token-1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                limit_price: Some(dec!(0.45)),
                purpose: IntentPurpose::Entry,
                created_at: Utc::now(),
            }],
            orders: vec![OrderRecord {
                order_id: "order-1".to_string(),
                intent_id: "intent-1".to_string(),
                deployment_id: "example.live".to_string(),
                token_id: "token-1".to_string(),
                requested_qty: dec!(2),
                limit_price: Some(dec!(0.45)),
                venue_order_id: Some("venue-1".to_string()),
                venue_order_history: vec!["venue-0".to_string()],
                revision: 1,
                state: OrderState::PartiallyFilled,
                filled_qty: dec!(1),
                rejection_reason: None,
                last_error: None,
            }],
            fills: vec![FillRecord {
                fill_id: "fill-1".to_string(),
                order_id: "order-1".to_string(),
                token_id: "token-1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                price: dec!(0.45),
                fee: dec!(0.02),
                timestamp: Utc::now(),
            }],
            positions: Vec::new(),
            pnl: Default::default(),
            risk: Default::default(),
        };

        let runtime = TradingRuntime::restore(snapshot);
        let restored = runtime.snapshot(&BTreeMap::new());
        assert_eq!(restored.orders.len(), 1);
        assert_eq!(restored.fills.len(), 1);
        assert_eq!(restored.positions.len(), 1);
        assert_eq!(restored.positions[0].net_qty, dec!(1));
        assert_eq!(restored.risk.active_orders, 1);
    }
}
