use crate::io_error_from_execution_error;
use ploy_connectivity::{
    ExecutionOutcome, ExecutionRequest, LiveExecutionGateway, OrderExecutionType,
};
use ploy_operator_contracts::PaperIntentResponse;
use ploy_platform::DeploymentRecord;
use ploy_trading::{TradingIntent, TradingRuntime};
use std::io;

pub fn submit_paper_intent(
    runtime: &mut TradingRuntime,
    deployment: &DeploymentRecord,
    intent: TradingIntent,
) -> io::Result<PaperIntentResponse> {
    if deployment.runtime_mode != "paper" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "only paper deployments are supported by the local trading runtime",
        ));
    }

    let order_id = format!("order-{}", intent.intent_id);
    let venue_order_id = format!("paper-{}", intent.intent_id);
    let deployment_id = intent.deployment_id.clone();
    let intent_id = intent.intent_id.clone();
    runtime.submit_intent(intent, order_id.clone());
    runtime.acknowledge_order(&order_id, venue_order_id.clone());
    Ok(PaperIntentResponse {
        deployment_id,
        intent_id,
        order_id,
        state: "acknowledged".to_string(),
        venue_order_id: Some(venue_order_id),
        rejection_reason: None,
        last_error: None,
    })
}

pub fn submit_live_intent(
    runtime: &mut TradingRuntime,
    gateway: &dyn LiveExecutionGateway,
    intent: TradingIntent,
) -> io::Result<PaperIntentResponse> {
    let order_id = format!("order-{}", intent.intent_id);
    runtime.submit_intent(intent.clone(), order_id.clone());

    let outcome = gateway.submit(&ExecutionRequest {
        order_id: order_id.clone(),
        token_id: intent.token_id.clone(),
        side: intent.side,
        quantity: intent.quantity,
        limit_price: intent.limit_price,
        order_type: OrderExecutionType::GTC,
        aggressive_ticks: 0,
    });

    match outcome {
        Ok(ExecutionOutcome::Acknowledged { venue_order_id }) => {
            runtime.acknowledge_order(&order_id, venue_order_id.clone());
            Ok(PaperIntentResponse {
                deployment_id: intent.deployment_id,
                intent_id: intent.intent_id,
                order_id,
                state: "acknowledged".to_string(),
                venue_order_id: Some(venue_order_id),
                rejection_reason: None,
                last_error: None,
            })
        }
        Ok(ExecutionOutcome::Rejected { reason }) => {
            runtime.reject_order(&order_id, reason.clone());
            Ok(PaperIntentResponse {
                deployment_id: intent.deployment_id,
                intent_id: intent.intent_id,
                order_id,
                state: "rejected".to_string(),
                venue_order_id: None,
                rejection_reason: Some(reason.clone()),
                last_error: Some(reason),
            })
        }
        Err(err) => {
            runtime.record_order_error(&order_id, err.to_string());
            Err(io_error_from_execution_error(err))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{submit_live_intent, submit_paper_intent};
    use ploy_connectivity::{ExecutionError, StaticExecutionGateway};
    use ploy_operator_contracts::{DeploymentState, DesiredState, ObservedState};
    use ploy_platform::DeploymentRecord;
    use ploy_trading::{IntentPurpose, TradeSide, TradingIntent, TradingRuntime};
    use rust_decimal_macros::dec;
    use std::io;

    fn paper_deployment() -> DeploymentRecord {
        DeploymentRecord {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: "paper".to_string(),
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        }
    }

    fn intent() -> TradingIntent {
        TradingIntent {
            intent_id: "intent-1".to_string(),
            deployment_id: "example.paper".to_string(),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            limit_price: Some(dec!(0.45)),
            purpose: IntentPurpose::Entry,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn paper_submit_acknowledges() {
        let mut runtime = TradingRuntime::default();
        let response = submit_paper_intent(&mut runtime, &paper_deployment(), intent()).expect("submit");
        assert_eq!(response.state, "acknowledged");
        assert!(runtime.order(&response.order_id).is_some());
    }

    #[test]
    fn live_submit_rejection_is_recorded() {
        let mut runtime = TradingRuntime::default();
        let gateway = StaticExecutionGateway::failed(ExecutionError::Transport("offline".to_string()));
        let error = submit_live_intent(&mut runtime, &gateway, intent()).expect_err("error");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    }
}
