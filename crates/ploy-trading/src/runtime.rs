use crate::fills::{FillLedger, FillRecord};
use crate::intents::{TradeSide, TradingIntent};
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TradeCashflowSummary {
    pub buy_shares: Decimal,
    pub sell_shares: Decimal,
    pub gross_buy_cost: Decimal,
    pub gross_sell_proceeds: Decimal,
    pub total_fees: Decimal,
}

impl TradeCashflowSummary {
    pub fn deployed_capital(&self) -> Decimal {
        self.gross_buy_cost
    }

    pub fn net_pnl(&self) -> Decimal {
        self.gross_sell_proceeds - self.gross_buy_cost - self.total_fees
    }

    pub fn roi_on_deployed_capital(&self) -> Option<Decimal> {
        let deployed = self.deployed_capital();
        if deployed.is_zero() {
            None
        } else {
            Some(self.net_pnl() / deployed)
        }
    }
}

impl TradingRuntimeSnapshot {
    pub fn fill_cashflow_summary(&self) -> TradeCashflowSummary {
        let mut summary = TradeCashflowSummary::default();

        for fill in &self.fills {
            let notional = fill.quantity * fill.price;
            summary.total_fees += fill.fee;

            match fill.side {
                TradeSide::Buy => {
                    summary.buy_shares += fill.quantity;
                    summary.gross_buy_cost += notional;
                }
                TradeSide::Sell => {
                    summary.sell_shares += fill.quantity;
                    summary.gross_sell_proceeds += notional;
                }
            }
        }

        summary
    }
}

#[derive(Debug, Default)]
pub struct TradingRuntime {
    intents: Vec<TradingIntent>,
    intent_by_id: BTreeMap<String, usize>,
    orders: OrderLedger,
    fills: FillLedger,
    positions: PositionLedger,
}

impl TradingRuntime {
    pub fn restore(snapshot: TradingRuntimeSnapshot) -> Self {
        let positions = if snapshot.positions.is_empty() {
            let mut positions = PositionLedger::default();
            for fill in &snapshot.fills {
                positions.apply_fill(fill);
            }
            positions
        } else {
            PositionLedger::restore(snapshot.positions, snapshot.pnl.total_fees)
        };

        let intent_by_id = snapshot
            .intents
            .iter()
            .enumerate()
            .map(|(index, intent)| (intent.intent_id.clone(), index))
            .collect();

        Self {
            intents: snapshot.intents,
            intent_by_id,
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
        self.prune_inactive_intents();
        let index = self.intents.len();
        self.intent_by_id.insert(intent.intent_id.clone(), index);
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
        self.intent_by_id
            .get(intent_id)
            .and_then(|index| self.intents.get(*index))
    }

    pub fn record_fill(&mut self, fill: FillRecord) -> bool {
        if self.fills.contains(&fill.fill_id) {
            return false;
        }
        if self.orders.apply_fill(&fill).is_none() {
            return false;
        }
        self.positions.apply_fill(&fill);
        self.fills.record(fill);
        self.prune_inactive_intents();
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

    fn prune_inactive_intents(&mut self) {
        let retained_intent_ids = self
            .orders
            .orders()
            .filter(|order| {
                matches!(
                    order.state,
                    crate::orders::OrderState::Pending
                        | crate::orders::OrderState::Acknowledged
                        | crate::orders::OrderState::PartiallyFilled
                ) || self.positions.net_qty(&order.token_id) != Decimal::ZERO
            })
            .map(|order| order.intent_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();

        if retained_intent_ids.len() == self.intents.len() {
            return;
        }

        self.intents
            .retain(|intent| retained_intent_ids.contains(intent.intent_id.as_str()));
        self.intent_by_id = self
            .intents
            .iter()
            .enumerate()
            .map(|(index, intent)| (intent.intent_id.clone(), index))
            .collect();
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
    use crate::{
        FillRecord, IntentPurpose, OrderRecord, OrderState, PnlSnapshot, PositionSnapshot,
        TradeSide, TradingIntent, TradingRuntimeSnapshot,
    };
    use chrono::Utc;
    use rust_decimal::Decimal;
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

    #[test]
    fn restore_preserves_persisted_positions_when_fills_are_absent() {
        let snapshot = TradingRuntimeSnapshot {
            positions: vec![PositionSnapshot {
                token_id: "token-1".to_string(),
                net_qty: dec!(3),
                avg_entry_price: dec!(0.42),
                realized_pnl: dec!(0.7),
            }],
            pnl: PnlSnapshot {
                realized_pnl: dec!(0.7),
                unrealized_pnl: Decimal::ZERO,
                total_fees: dec!(0.03),
            },
            ..TradingRuntimeSnapshot::default()
        };

        let runtime = TradingRuntime::restore(snapshot);
        let restored = runtime.snapshot(&BTreeMap::new());

        assert_eq!(restored.positions.len(), 1);
        assert_eq!(restored.positions[0].net_qty, dec!(3));
        assert_eq!(restored.pnl.realized_pnl, dec!(0.7));
        assert_eq!(restored.pnl.total_fees, dec!(0.03));
        assert_eq!(restored.risk.open_positions, 1);
        assert_eq!(restored.risk.gross_exposure, dec!(1.26));
    }

    #[test]
    fn closed_position_intents_are_pruned_from_lookup() {
        let mut runtime = TradingRuntime::default();
        runtime.submit_intent(
            TradingIntent {
                intent_id: "intent-1".to_string(),
                deployment_id: "dep-1".to_string(),
                market_id: "market-1".to_string(),
                token_id: "token-1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.40)),
                purpose: IntentPurpose::Entry,
                created_at: Utc::now(),
            },
            "order-1",
        );
        runtime.acknowledge_order("order-1", "venue-1");
        assert!(runtime.intent("intent-1").is_some());

        runtime.record_fill(FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        });
        assert!(runtime.intent("intent-1").is_some());

        runtime.submit_intent(
            TradingIntent {
                intent_id: "intent-exit".to_string(),
                deployment_id: "dep-1".to_string(),
                market_id: "market-1".to_string(),
                token_id: "token-1".to_string(),
                side: TradeSide::Sell,
                quantity: dec!(1),
                limit_price: Some(dec!(0.60)),
                purpose: IntentPurpose::Exit,
                created_at: Utc::now(),
            },
            "order-exit",
        );
        runtime.acknowledge_order("order-exit", "venue-exit");
        runtime.record_fill(FillRecord {
            fill_id: "fill-2".to_string(),
            order_id: "order-exit".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Sell,
            quantity: dec!(1),
            price: dec!(0.60),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        });
        assert!(runtime.intent("intent-1").is_none());
        assert!(runtime.snapshot(&BTreeMap::new()).intents.is_empty());
    }

    #[test]
    fn cashflow_summary_treats_quantity_as_shares_not_dollars() {
        let now = Utc::now();
        let snapshot = TradingRuntimeSnapshot {
            fills: vec![
                FillRecord {
                    fill_id: "fill-buy".to_string(),
                    order_id: "order-buy".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(25),
                    price: dec!(0.40),
                    fee: dec!(0.05),
                    timestamp: now,
                },
                FillRecord {
                    fill_id: "fill-sell".to_string(),
                    order_id: "order-sell".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(25),
                    price: dec!(1.00),
                    fee: Decimal::ZERO,
                    timestamp: now,
                },
            ],
            ..Default::default()
        };

        let summary = snapshot.fill_cashflow_summary();
        assert_eq!(summary.buy_shares, dec!(25));
        assert_eq!(summary.sell_shares, dec!(25));
        assert_eq!(summary.gross_buy_cost, dec!(10.00));
        assert_eq!(summary.gross_sell_proceeds, dec!(25.00));
        assert_eq!(summary.deployed_capital(), dec!(10.00));
        assert_eq!(summary.net_pnl(), dec!(14.95));
        assert_eq!(
            summary.roi_on_deployed_capital().expect("roi").round_dp(4),
            dec!(1.4950)
        );
    }
}
