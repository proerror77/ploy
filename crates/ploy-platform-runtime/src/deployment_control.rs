use ploy_operator_contracts::{
    DeploymentApplyRequest, DeploymentControlRequest, DeploymentState, DesiredState,
};
use ploy_platform::{DeploymentRecord, DeploymentRegistry};
use ploy_trading::{IntentPurpose, OrderRecord, TradingIntent};
use rust_decimal::Decimal;
use std::io;

use crate::{intent_allowed_while_draining, intent_counts_toward_exposure, observed_state_for_desired};

pub fn build_deployment_record(request: DeploymentApplyRequest) -> DeploymentRecord {
    DeploymentRecord {
        deployment_id: request.deployment_id,
        bundle_id: request.bundle_id,
        runtime_mode: request.runtime_mode,
        account_id: request.account_id,
        max_gross_exposure: request.max_gross_exposure,
        deployment_state: request.deployment_state,
        desired_state: request.desired_state,
        observed_state: observed_state_for_desired(request.desired_state),
    }
}

pub fn apply_deployment(
    registry: &mut DeploymentRegistry,
    request: DeploymentApplyRequest,
) -> DeploymentRecord {
    let record = build_deployment_record(request);
    registry.upsert(record.clone());
    registry
        .get(&record.deployment_id)
        .cloned()
        .expect("deployment persisted")
}

pub fn control_deployment(
    registry: &mut DeploymentRegistry,
    deployment_id: &str,
    request: DeploymentControlRequest,
) -> io::Result<Option<DeploymentRecord>> {
    let Some(existing) = registry.get(deployment_id).cloned() else {
        return Ok(None);
    };

    if let Some(deployment_state) = request.deployment_state {
        registry.set_deployment_state(deployment_id, deployment_state);
    }
    if let Some(desired_state) = request.desired_state {
        registry.set_desired_state(deployment_id, desired_state);
        registry.set_observed_state(deployment_id, observed_state_for_desired(desired_state));
    }

    if request.deployment_state.is_none() && request.desired_state.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("deployment `{deployment_id}` control request was empty"),
        ));
    }

    Ok(registry.get(deployment_id).cloned().or(Some(existing)))
}

pub fn ensure_intent_allowed(
    deployment: &DeploymentRecord,
    intent: &TradingIntent,
) -> io::Result<()> {
    if deployment.deployment_state == DeploymentState::Disabled
        || deployment.deployment_state == DeploymentState::Archived
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "deployment is {} and cannot accept intents",
                crate::deployment_state_wire(deployment.deployment_state)
            ),
        ));
    }

    if deployment.deployment_state == DeploymentState::Draining
        && !intent_allowed_while_draining(intent.purpose)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "deployment is draining and only exit/reduce/hedge/cancel intents are allowed",
        ));
    }

    if deployment.desired_state != DesiredState::Running {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "deployment must be running before it can accept intents",
        ));
    }

    Ok(())
}

pub fn enforce_exposure_limit(
    deployment: &DeploymentRecord,
    intent: &TradingIntent,
    current_total_exposure: Decimal,
) -> io::Result<()> {
    let Some(max_gross_exposure) = deployment.max_gross_exposure else {
        return Ok(());
    };
    if !intent_counts_toward_exposure(intent.purpose) {
        return Ok(());
    }

    let requested_exposure = intent.quantity * intent.limit_price.unwrap_or(Decimal::ONE);
    if current_total_exposure + requested_exposure > max_gross_exposure {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "deployment `{}` would exceed max_gross_exposure {} on account `{}` (current_total={} requested={})",
                deployment.deployment_id,
                max_gross_exposure,
                deployment.account_id,
                current_total_exposure,
                requested_exposure
            ),
        ));
    }

    Ok(())
}

pub fn enforce_order_replacement_exposure(
    deployment: &DeploymentRecord,
    order: &OrderRecord,
    request: &ploy_operator_contracts::OrderReplaceRequest,
    intent_purpose: IntentPurpose,
    current_total_exposure: Decimal,
) -> io::Result<()> {
    let Some(max_gross_exposure) = deployment.max_gross_exposure else {
        return Ok(());
    };
    if !intent_counts_toward_exposure(intent_purpose) {
        return Ok(());
    }

    let current_reservation =
        (order.requested_qty - order.filled_qty).max(Decimal::ZERO) * order.limit_price.unwrap_or(Decimal::ONE);
    let replacement_reservation =
        (request.quantity - order.filled_qty).max(Decimal::ZERO)
            * request.limit_price.unwrap_or(order.limit_price.unwrap_or(Decimal::ONE));
    let next_total_exposure = current_total_exposure - current_reservation + replacement_reservation;

    if next_total_exposure > max_gross_exposure {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "replacement would exceed max_gross_exposure {} on account `{}` (current_total={} next_total={})",
                max_gross_exposure,
                deployment.account_id,
                current_total_exposure,
                next_total_exposure
            ),
        ));
    }

    Ok(())
}

pub fn set_deployment_max_gross_exposure(
    registry: &mut DeploymentRegistry,
    deployment_id: &str,
    max_gross_exposure: Option<Decimal>,
    current_exposure: Option<Decimal>,
) -> io::Result<DeploymentRecord> {
    if let (Some(limit), Some(current)) = (max_gross_exposure, current_exposure) {
        if current > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "current exposure {} exceeds proposed limit {} for `{}`",
                    current, limit, deployment_id
                ),
            ));
        }
    }

    registry
        .set_max_gross_exposure(deployment_id, max_gross_exposure)
        .cloned()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("deployment `{deployment_id}` was not found"),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{
        apply_deployment, control_deployment, enforce_exposure_limit, ensure_intent_allowed,
        set_deployment_max_gross_exposure,
    };
    use ploy_operator_contracts::{
        DeploymentApplyRequest, DeploymentControlRequest, DeploymentState, DesiredState,
        ObservedState,
    };
    use ploy_platform::DeploymentRegistry;
    use ploy_trading::{IntentPurpose, TradeSide, TradingIntent};
    use rust_decimal_macros::dec;

    #[test]
    fn apply_and_control_deployment_mutates_registry() {
        let mut registry = DeploymentRegistry::default();
        let record = apply_deployment(
            &mut registry,
            DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: "paper".to_string(),
                account_id: "acct-paper".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            },
        );
        assert_eq!(record.observed_state, ObservedState::Starting);

        let updated = control_deployment(
            &mut registry,
            "example.paper",
            DeploymentControlRequest {
                desired_state: Some(DesiredState::Paused),
                deployment_state: None,
            },
        )
        .expect("control")
        .expect("deployment");
        assert_eq!(updated.desired_state, DesiredState::Paused);
    }

    #[test]
    fn intent_checks_and_exposure_limits_hold() {
        let deployment = ploy_platform::DeploymentRecord {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: "paper".to_string(),
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        };
        let intent = TradingIntent {
            intent_id: "intent-1".to_string(),
            deployment_id: "example.paper".to_string(),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            limit_price: Some(dec!(1)),
            purpose: IntentPurpose::Entry,
            created_at: chrono::Utc::now(),
        };

        ensure_intent_allowed(&deployment, &intent).expect("allowed");
        enforce_exposure_limit(&deployment, &intent, dec!(2)).expect("fits");
        assert!(enforce_exposure_limit(&deployment, &intent, dec!(4)).is_err());
    }

    #[test]
    fn max_exposure_update_rejects_too_low_limits() {
        let mut registry = DeploymentRegistry::default();
        apply_deployment(
            &mut registry,
            DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: "paper".to_string(),
                account_id: "acct-paper".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            },
        );

        assert!(set_deployment_max_gross_exposure(
            &mut registry,
            "example.paper",
            Some(dec!(2)),
            Some(dec!(3)),
        )
        .is_err());
    }
}
