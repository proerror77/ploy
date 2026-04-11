use crate::{build_order_control_response, io_error_from_execution_error, order_state_wire};
use ploy_connectivity::{
    CancellationOutcome, CancellationRequest, LiveExecutionGateway, ReplaceOutcome, ReplaceRequest,
};
use ploy_operator_contracts::{OrderControlResponse, OrderReplaceRequest};
use ploy_platform::DeploymentRecord;
use ploy_trading::{OrderState, TradingRuntime};
use std::io;

pub fn cancel_order(
    runtime: &mut TradingRuntime,
    gateway: &dyn LiveExecutionGateway,
    deployment: &DeploymentRecord,
    deployment_id: &str,
    order_id: &str,
) -> io::Result<OrderControlResponse> {
    let order = runtime
        .order(order_id)
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "order not found"))?;

    if !matches!(
        order.state,
        OrderState::Pending | OrderState::Acknowledged | OrderState::PartiallyFilled
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "order `{order_id}` is not cancelable from state `{}`",
                order_state_wire(order.state)
            ),
        ));
    }

    if deployment.runtime_mode == "live" {
        if let Some(venue_order_id) = order.venue_order_id.clone() {
            let cancel_result = gateway.cancel(&CancellationRequest {
                order_id: order_id.to_string(),
                venue_order_id,
            });
            match cancel_result {
                Ok(CancellationOutcome::Canceled) => {}
                Ok(CancellationOutcome::Rejected { reason }) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("live cancel rejected: {reason}"),
                    ));
                }
                Err(err) => return Err(io_error_from_execution_error(err)),
            }
        }
    }

    let updated = runtime
        .cancel_order(order_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "order not found"))?;
    Ok(build_order_control_response(
        deployment_id.to_string(),
        updated,
    ))
}

pub fn replace_order(
    runtime: &mut TradingRuntime,
    gateway: &dyn LiveExecutionGateway,
    deployment: &DeploymentRecord,
    deployment_id: &str,
    order_id: &str,
    request: OrderReplaceRequest,
    current_total_exposure: rust_decimal::Decimal,
) -> io::Result<OrderControlResponse> {
    let order = runtime
        .order(order_id)
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "order not found"))?;

    if !matches!(
        order.state,
        OrderState::Pending | OrderState::Acknowledged | OrderState::PartiallyFilled
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "order `{order_id}` is not replaceable from state `{}`",
                order_state_wire(order.state)
            ),
        ));
    }

    if request.quantity < order.filled_qty {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "replacement quantity {} cannot be below filled quantity {}",
                request.quantity, order.filled_qty
            ),
        ));
    }

    let Some(intent) = runtime.intent(&order.intent_id) else {
        return Ok(build_order_control_response(
            deployment_id.to_string(),
            &order,
        ));
    };

    crate::enforce_order_replacement_exposure(
        deployment,
        &order,
        &request,
        intent.purpose,
        current_total_exposure,
    )?;

    if deployment.runtime_mode == "live" {
        let venue_order_id = order.venue_order_id.clone().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("order `{order_id}` has no live venue order to replace"),
            )
        })?;
        let side = runtime
            .intent(&order.intent_id)
            .map(|intent| intent.side)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("intent `{}` for order `{order_id}` was not found", order.intent_id),
                )
            })?;

        match gateway.replace(&ReplaceRequest {
            order_id: order_id.to_string(),
            venue_order_id,
            token_id: order.token_id.clone(),
            side,
            quantity: request.quantity,
            limit_price: request.limit_price,
        }) {
            Ok(ReplaceOutcome::Replaced { venue_order_id }) => {
                let updated = runtime
                    .replace_order(order_id, request.quantity, request.limit_price, venue_order_id)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "order not found"))?;
                Ok(build_order_control_response(
                    deployment_id.to_string(),
                    updated,
                ))
            }
            Ok(ReplaceOutcome::Rejected { reason }) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("live replace rejected: {reason}"),
            )),
            Err(err) => {
                let _ = runtime.record_order_error(order_id, err.to_string());
                Err(io_error_from_execution_error(err))
            }
        }
    } else {
        let next_revision = order.revision + 1;
        let venue_order_id = format!("paper-{order_id}-r{next_revision}");
        let updated = runtime
            .replace_order(order_id, request.quantity, request.limit_price, venue_order_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "order not found"))?;
        Ok(build_order_control_response(
            deployment_id.to_string(),
            updated,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{cancel_order, replace_order};
    use ploy_connectivity::{CancellationOutcome, ExecutionError, StaticExecutionGateway};
    use ploy_operator_contracts::{DeploymentState, DesiredState, ObservedState, OrderReplaceRequest};
    use ploy_platform::DeploymentRecord;
    use ploy_trading::{IntentPurpose, TradeSide, TradingIntent, TradingRuntime};
    use rust_decimal_macros::dec;
    use std::io::ErrorKind;

    fn live_deployment() -> DeploymentRecord {
        DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: "live".to_string(),
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        }
    }

    fn seeded_runtime() -> TradingRuntime {
        let mut runtime = TradingRuntime::default();
        runtime.submit_intent(
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
        );
        runtime.acknowledge_order("order-1", "venue-1");
        runtime
    }

    #[test]
    fn cancel_live_order_updates_runtime() {
        let mut runtime = seeded_runtime();
        let gateway = StaticExecutionGateway::acknowledged("venue-1")
            .with_cancel_result(Ok(CancellationOutcome::Canceled));
        let response = cancel_order(&mut runtime, &gateway, &live_deployment(), "example.live", "order-1")
            .expect("cancel");
        assert_eq!(response.state, "canceled");
    }

    #[test]
    fn replace_rejects_invalid_quantity() {
        let mut runtime = seeded_runtime();
        runtime.record_fill(ploy_trading::FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1.5),
            price: dec!(0.45),
            fee: dec!(0.01),
            timestamp: chrono::Utc::now(),
        });
        let gateway = StaticExecutionGateway::failed(ExecutionError::Transport("offline".to_string()));
        let error = replace_order(
            &mut runtime,
            &gateway,
            &live_deployment(),
            "example.live",
            "order-1",
            OrderReplaceRequest {
                quantity: dec!(1),
                limit_price: Some(dec!(0.47)),
            },
            dec!(2),
        )
        .expect_err("invalid quantity");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }
}
