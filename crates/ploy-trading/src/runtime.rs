use crate::fills::{FillLedger, FillRecord};
use crate::intents::TradingIntent;
use crate::orders::OrderLedger;
use crate::pnl::PnlSnapshot;
use crate::positions::{PositionLedger, PositionSnapshot};
use crate::risk::{snapshot_from_state, RiskSnapshot};
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

    pub fn reject_order(
        &mut self,
        order_id: &str,
        reason: impl Into<String>,
    ) -> Option<&crate::orders::OrderRecord> {
        self.orders.reject(order_id, reason)
    }

    pub fn cancel_order(&mut self, order_id: &str) -> Option<&crate::orders::OrderRecord> {
        self.orders.cancel(order_id)
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
