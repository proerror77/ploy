use crate::fills::FillRecord;
use crate::intents::TradingIntent;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    Pending,
    Acknowledged,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderRecord {
    pub order_id: String,
    pub intent_id: String,
    pub deployment_id: String,
    pub token_id: String,
    pub requested_qty: Decimal,
    pub limit_price: Option<Decimal>,
    pub venue_order_id: Option<String>,
    pub state: OrderState,
    pub filled_qty: Decimal,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Default)]
pub struct OrderLedger {
    orders: BTreeMap<String, OrderRecord>,
}

impl OrderLedger {
    pub fn restore(records: Vec<OrderRecord>) -> Self {
        let orders = records
            .into_iter()
            .map(|record| (record.order_id.clone(), record))
            .collect();
        Self { orders }
    }

    pub fn insert_from_intent(
        &mut self,
        order_id: impl Into<String>,
        intent: &TradingIntent,
    ) -> &OrderRecord {
        let order_id = order_id.into();
        let record = OrderRecord {
            order_id: order_id.clone(),
            intent_id: intent.intent_id.clone(),
            deployment_id: intent.deployment_id.clone(),
            token_id: intent.token_id.clone(),
            requested_qty: intent.quantity,
            limit_price: intent.limit_price,
            venue_order_id: None,
            state: OrderState::Pending,
            filled_qty: Decimal::ZERO,
            rejection_reason: None,
        };
        self.orders.insert(order_id.clone(), record);
        self.orders.get(&order_id).expect("order inserted")
    }

    pub fn acknowledge(
        &mut self,
        order_id: &str,
        venue_order_id: impl Into<String>,
    ) -> Option<&OrderRecord> {
        let record = self.orders.get_mut(order_id)?;
        record.venue_order_id = Some(venue_order_id.into());
        record.state = OrderState::Acknowledged;
        Some(record)
    }

    pub fn reject(&mut self, order_id: &str, reason: impl Into<String>) -> Option<&OrderRecord> {
        let record = self.orders.get_mut(order_id)?;
        record.state = OrderState::Rejected;
        record.rejection_reason = Some(reason.into());
        Some(record)
    }

    pub fn cancel(&mut self, order_id: &str) -> Option<&OrderRecord> {
        let record = self.orders.get_mut(order_id)?;
        record.state = OrderState::Canceled;
        Some(record)
    }

    pub fn apply_fill(&mut self, fill: &FillRecord) -> Option<&OrderRecord> {
        let record = self.orders.get_mut(&fill.order_id)?;
        record.filled_qty += fill.quantity;
        record.state = if record.filled_qty >= record.requested_qty {
            OrderState::Filled
        } else {
            OrderState::PartiallyFilled
        };
        Some(record)
    }

    pub fn active_orders(&self) -> usize {
        self.orders
            .values()
            .filter(|record| {
                matches!(
                    record.state,
                    OrderState::Pending | OrderState::Acknowledged | OrderState::PartiallyFilled
                )
            })
            .count()
    }

    pub fn orders(&self) -> impl Iterator<Item = &OrderRecord> {
        self.orders.values()
    }
}
