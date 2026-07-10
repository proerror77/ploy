use crate::ReconcileStatus;
use ploy_connectivity::{LiveExecutionGateway, TrackedOrder};
use ploy_operator_contracts::DeploymentRuntimeMode;
use ploy_platform::DeploymentRecord;
use ploy_trading::TradingRuntime;
use std::collections::{BTreeMap, HashMap};
use std::io;

pub fn reconcile_live_fills(
    live_execution: &dyn LiveExecutionGateway,
    deployments: &[DeploymentRecord],
    trading: &mut BTreeMap<String, TradingRuntime>,
) -> io::Result<ReconcileStatus> {
    let mut tracked_orders = Vec::new();
    let mut order_deployments = HashMap::new();

    for record in deployments {
        if record.runtime_mode != DeploymentRuntimeMode::Live {
            continue;
        }

        let Some(runtime) = trading.get(&record.deployment_id) else {
            continue;
        };

        for order in runtime
            .snapshot(&BTreeMap::new())
            .orders
            .into_iter()
            .filter(|order| {
                order.venue_order_id.is_some()
                    && matches!(
                        order.state,
                        ploy_trading::OrderState::Unknown
                            | ploy_trading::OrderState::Acknowledged
                            | ploy_trading::OrderState::PartiallyFilled
                    )
            })
        {
            let Some(venue_order_id) = order.venue_order_id.clone() else {
                continue;
            };
            order_deployments.insert(order.order_id.clone(), record.deployment_id.clone());
            tracked_orders.push(TrackedOrder {
                order_id: order.order_id,
                venue_order_id,
                token_id: order.token_id,
            });
        }
    }

    if tracked_orders.is_empty() {
        return Ok(ReconcileStatus::Noop);
    }

    let fills = live_execution
        .reconcile_fills(&tracked_orders)
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;

    let mut recorded = 0;
    for fill in fills {
        let Some(deployment_id) = order_deployments.get(&fill.order_id) else {
            continue;
        };
        let Some(runtime) = trading.get_mut(deployment_id) else {
            continue;
        };
        if runtime.record_fill(fill) {
            recorded += 1;
        }
    }

    Ok(ReconcileStatus::Applied(recorded))
}

#[cfg(test)]
mod tests {
    use super::reconcile_live_fills;
    use ploy_connectivity::StaticExecutionGateway;
    use ploy_operator_contracts::{DeploymentState, DesiredState, ObservedState};
    use ploy_platform::DeploymentRecord;
    use ploy_trading::{FillRecord, IntentPurpose, TradeSide, TradingIntent, TradingRuntime};
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;

    #[test]
    fn reconcile_records_fills_into_trading_runtime() {
        let deployment = DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        };
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-1".to_string(),
                    deployment_id: "example.live".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.45)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-1",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-1", "venue-1");
        runtime.mark_order_unknown("order-1", "final persistence lost");

        let fill = FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            price: dec!(0.45),
            fee: dec!(0.01),
            timestamp: chrono::Utc::now(),
        };
        let gateway =
            StaticExecutionGateway::acknowledged("venue-1").with_reconciled_fills(vec![fill]);
        let mut trading = BTreeMap::from([(deployment.deployment_id.clone(), runtime)]);

        let result =
            reconcile_live_fills(&gateway, &[deployment], &mut trading).expect("reconcile");
        assert_eq!(result, crate::ReconcileStatus::Applied(1));
        assert_eq!(
            trading
                .get("example.live")
                .expect("runtime")
                .snapshot(&BTreeMap::new())
                .fills
                .len(),
            1
        );
    }

    #[test]
    fn unknown_without_venue_order_id_is_not_reconciled() {
        let deployment = DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: None,
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Paused,
            observed_state: ObservedState::Degraded,
        };
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-unknown".to_string(),
                    deployment_id: "example.live".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-unknown",
                None,
            )
            .unwrap();
        runtime.mark_order_unknown("order-unknown", "transport lost");
        let mut runtimes = BTreeMap::from([("example.live".to_string(), runtime)]);

        let result = reconcile_live_fills(
            &StaticExecutionGateway::acknowledged("unused"),
            &[deployment],
            &mut runtimes,
        )
        .unwrap();
        assert_eq!(result, crate::ReconcileStatus::Noop);
    }

    #[test]
    fn archived_orders_remain_reconcilable_until_flat() {
        let deployment = DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Archived,
            desired_state: DesiredState::Stopped,
            observed_state: ObservedState::Stopped,
        };
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-archived".to_string(),
                    deployment_id: "example.live".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.45)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-archived",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-archived", "venue-archived");
        let gateway =
            StaticExecutionGateway::acknowledged("venue-archived").with_reconciled_fills(vec![
                FillRecord {
                    fill_id: "fill-archived".to_string(),
                    order_id: "order-archived".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    price: dec!(0.45),
                    fee: dec!(0.01),
                    timestamp: chrono::Utc::now(),
                },
            ]);
        let mut trading = BTreeMap::from([(deployment.deployment_id.clone(), runtime)]);

        let result = reconcile_live_fills(&gateway, &[deployment], &mut trading)
            .expect("archived order reconciliation");

        assert_eq!(result, crate::ReconcileStatus::Applied(1));
    }
}
