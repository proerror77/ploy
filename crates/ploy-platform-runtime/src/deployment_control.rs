use ploy_operator_contracts::{
    DeploymentApplyRequest, DeploymentControlRequest, DeploymentRuntimeMode, DeploymentState,
    DesiredState, ObservedState,
};
use ploy_platform::{DeploymentRecord, DeploymentRegistry};
use ploy_trading::{IntentPurpose, OrderRecord, TradingIntent};
use rust_decimal::Decimal;
use std::io;

use crate::{
    intent_counts_toward_exposure, observed_state_for_desired,
    runtime_support::{IntentAdmissionSource, IntentRiskEffect},
};

pub fn build_deployment_record(request: DeploymentApplyRequest) -> DeploymentRecord {
    let desired_state = if request.deployment_state == DeploymentState::Archived {
        DesiredState::Stopped
    } else {
        request.desired_state
    };
    DeploymentRecord {
        deployment_id: request.deployment_id,
        bundle_id: request.bundle_id,
        runtime_mode: request.runtime_mode,
        account_id: request.account_id,
        max_gross_exposure: request.max_gross_exposure,
        deployment_state: request.deployment_state,
        desired_state,
        observed_state: observed_state_for_desired(desired_state),
    }
}

pub fn apply_deployment(
    registry: &mut DeploymentRegistry,
    request: DeploymentApplyRequest,
) -> io::Result<DeploymentRecord> {
    if let Some(existing) = registry.get(&request.deployment_id) {
        let execution_spec_changed = existing.bundle_id != request.bundle_id
            || existing.runtime_mode != request.runtime_mode
            || existing.account_id != request.account_id;
        if existing.desired_state == DesiredState::Running && execution_spec_changed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "deployment `{}` must be paused or stopped before changing bundle_id, runtime_mode, or account_id",
                    request.deployment_id
                ),
            ));
        }
        let cap_increased_or_removed =
            match (existing.max_gross_exposure, request.max_gross_exposure) {
                (Some(_), None) => true,
                (Some(current), Some(proposed)) => proposed > current,
                (None, None | Some(_)) => false,
            };
        if existing.desired_state == DesiredState::Running && cap_increased_or_removed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "deployment `{}` must be paused or stopped before increasing or removing max_gross_exposure",
                    request.deployment_id
                ),
            ));
        }
    }
    let record = build_deployment_record(request);
    registry.upsert(record.clone());
    Ok(registry
        .get(&record.deployment_id)
        .cloned()
        .expect("deployment persisted"))
}

pub fn control_deployment(
    registry: &mut DeploymentRegistry,
    deployment_id: &str,
    request: DeploymentControlRequest,
) -> io::Result<Option<DeploymentRecord>> {
    let Some(existing) = registry.get(deployment_id).cloned() else {
        return Ok(None);
    };

    if let Some(desired_state) = request.desired_state {
        registry.set_desired_state(deployment_id, desired_state);
        registry.set_observed_state(deployment_id, observed_state_for_desired(desired_state));
    }
    if let Some(deployment_state) = request.deployment_state {
        registry.set_deployment_state(deployment_id, deployment_state);
        if deployment_state == DeploymentState::Archived {
            registry.set_desired_state(deployment_id, DesiredState::Stopped);
            registry.set_observed_state(deployment_id, ObservedState::Stopped);
        }
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
    risk_effect: IntentRiskEffect,
    venue_health_fresh: bool,
    source: IntentAdmissionSource,
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

    if deployment.runtime_mode == DeploymentRuntimeMode::Paper {
        if deployment.deployment_state == DeploymentState::Draining
            && intent.purpose == IntentPurpose::Entry
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
        return Ok(());
    }

    if risk_effect == IntentRiskEffect::Increase {
        if deployment.deployment_state != DeploymentState::Enabled
            || deployment.desired_state != DesiredState::Running
            || deployment.observed_state != ObservedState::Running
            || !venue_health_fresh
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "live risk increase requires enabled, desired running, observed running, and fresh venue health",
            ));
        }
        return Ok(());
    }

    let paused_or_failed = deployment.desired_state != DesiredState::Running
        || matches!(
            deployment.observed_state,
            ObservedState::Paused | ObservedState::Stopped | ObservedState::Failed
        );
    if paused_or_failed {
        if risk_effect == IntentRiskEffect::Reduce
            && matches!(
                source,
                IntentAdmissionSource::AuthenticatedOperator | IntentAdmissionSource::Emergency
            )
        {
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "paused, stopped, or failed live deployment accepts reduction only from an authenticated operator or emergency source",
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

    let current_reservation = (order.requested_qty - order.filled_qty).max(Decimal::ZERO)
        * order.limit_price.unwrap_or(Decimal::ONE);
    let replacement_reservation = (request.quantity - order.filled_qty).max(Decimal::ZERO)
        * request
            .limit_price
            .unwrap_or(order.limit_price.unwrap_or(Decimal::ONE));
    let next_total_exposure =
        current_total_exposure - current_reservation + replacement_reservation;

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

    use crate::runtime_support::{IntentAdmissionSource, IntentRiskEffect};

    fn live_deployment(
        deployment_state: DeploymentState,
        desired_state: DesiredState,
        observed_state: ObservedState,
    ) -> ploy_platform::DeploymentRecord {
        ploy_platform::DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state,
            desired_state,
            observed_state,
        }
    }

    fn live_intent(purpose: IntentPurpose) -> TradingIntent {
        TradingIntent {
            intent_id: format!("intent-{purpose:?}"),
            deployment_id: "example.live".to_string(),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            limit_price: Some(dec!(1)),
            purpose,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn apply_and_control_deployment_mutates_registry() {
        let mut registry = DeploymentRegistry::default();
        let record = apply_deployment(
            &mut registry,
            DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                account_id: "acct-paper".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            },
        );
        assert_eq!(
            record.expect("apply").observed_state,
            ObservedState::Starting
        );

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
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
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

        ensure_intent_allowed(
            &deployment,
            &intent,
            IntentRiskEffect::Increase,
            false,
            IntentAdmissionSource::Worker,
        )
        .expect("paper admission keeps existing lifecycle behavior");
        enforce_exposure_limit(&deployment, &intent, dec!(2)).expect("fits");
        assert!(enforce_exposure_limit(&deployment, &intent, dec!(4)).is_err());
    }

    #[test]
    fn degraded_live_rejects_entry_and_increasing_hedge_but_allows_reduction() {
        let deployment = live_deployment(
            DeploymentState::Enabled,
            DesiredState::Running,
            ObservedState::Degraded,
        );

        for intent in [
            live_intent(IntentPurpose::Entry),
            live_intent(IntentPurpose::Hedge),
        ] {
            assert!(ensure_intent_allowed(
                &deployment,
                &intent,
                IntentRiskEffect::Increase,
                false,
                IntentAdmissionSource::Worker,
            )
            .is_err());
        }
        ensure_intent_allowed(
            &deployment,
            &live_intent(IntentPurpose::Reduce),
            IntentRiskEffect::Reduce,
            false,
            IntentAdmissionSource::Worker,
        )
        .expect("degraded live deployment must retain a reduction path");
    }

    #[test]
    fn starting_live_rejects_risk_increase() {
        let deployment = live_deployment(
            DeploymentState::Enabled,
            DesiredState::Running,
            ObservedState::Starting,
        );

        assert!(ensure_intent_allowed(
            &deployment,
            &live_intent(IntentPurpose::Entry),
            IntentRiskEffect::Increase,
            true,
            IntentAdmissionSource::Worker,
        )
        .is_err());
    }

    #[test]
    fn draining_live_rejects_increasing_hedge() {
        let deployment = live_deployment(
            DeploymentState::Draining,
            DesiredState::Running,
            ObservedState::Running,
        );

        assert!(ensure_intent_allowed(
            &deployment,
            &live_intent(IntentPurpose::Hedge),
            IntentRiskEffect::Increase,
            true,
            IntentAdmissionSource::Worker,
        )
        .is_err());
    }

    #[test]
    fn paused_live_reduction_rejects_worker_but_accepts_operator() {
        let deployment = live_deployment(
            DeploymentState::Enabled,
            DesiredState::Paused,
            ObservedState::Paused,
        );
        let reduction = live_intent(IntentPurpose::Reduce);

        assert!(ensure_intent_allowed(
            &deployment,
            &reduction,
            IntentRiskEffect::Reduce,
            false,
            IntentAdmissionSource::Worker,
        )
        .is_err());
        ensure_intent_allowed(
            &deployment,
            &reduction,
            IntentRiskEffect::Reduce,
            false,
            IntentAdmissionSource::AuthenticatedOperator,
        )
        .expect("authenticated operator must retain a paused reduction path");
    }

    #[test]
    fn max_exposure_update_rejects_too_low_limits() {
        let mut registry = DeploymentRegistry::default();
        let _ = apply_deployment(
            &mut registry,
            DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
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

    #[test]
    fn running_deployment_rejects_execution_spec_change() {
        let mut registry = DeploymentRegistry::default();
        apply_deployment(
            &mut registry,
            DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                account_id: "acct-paper".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            },
        )
        .expect("initial apply");

        let error = apply_deployment(
            &mut registry,
            DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "replacement".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
                account_id: "acct-live".to_string(),
                max_gross_exposure: Some(dec!(4)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            },
        )
        .expect_err("running execution spec drift must be rejected");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        let persisted = registry.get("example.paper").expect("persisted deployment");
        assert_eq!(persisted.bundle_id, "example");
        assert_eq!(
            persisted.runtime_mode,
            ploy_operator_contracts::DeploymentRuntimeMode::Paper
        );
        assert_eq!(persisted.account_id, "acct-paper");
    }

    #[test]
    fn running_deployment_permits_identical_reapply_and_cap_reduction() {
        let mut registry = DeploymentRegistry::default();
        let request = DeploymentApplyRequest {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
        };
        apply_deployment(&mut registry, request.clone()).expect("initial apply");
        apply_deployment(&mut registry, request.clone()).expect("identical reapply");

        let reduced = apply_deployment(
            &mut registry,
            DeploymentApplyRequest {
                max_gross_exposure: Some(dec!(4)),
                ..request
            },
        )
        .expect("safe cap reduction");

        assert_eq!(reduced.max_gross_exposure, Some(dec!(4)));
    }

    #[test]
    fn running_deployment_rejects_cap_increase_and_removal() {
        let mut registry = DeploymentRegistry::default();
        let request = DeploymentApplyRequest {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
        };
        apply_deployment(&mut registry, request.clone()).expect("initial apply");

        let increase = apply_deployment(
            &mut registry,
            DeploymentApplyRequest {
                max_gross_exposure: Some(dec!(6)),
                ..request.clone()
            },
        )
        .expect_err("running cap increase must be rejected");
        assert_eq!(increase.kind(), std::io::ErrorKind::InvalidInput);

        let removal = apply_deployment(
            &mut registry,
            DeploymentApplyRequest {
                max_gross_exposure: None,
                ..request
            },
        )
        .expect_err("running cap removal must be rejected");
        assert_eq!(removal.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            registry
                .get("example.paper")
                .expect("persisted deployment")
                .max_gross_exposure,
            Some(dec!(5))
        );
    }

    #[test]
    fn archive_forces_stopped_desired_state() {
        let mut registry = DeploymentRegistry::default();
        apply_deployment(
            &mut registry,
            DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                account_id: "acct-paper".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            },
        )
        .expect("initial apply");

        let archived = control_deployment(
            &mut registry,
            "example.paper",
            DeploymentControlRequest {
                desired_state: Some(DesiredState::Running),
                deployment_state: Some(DeploymentState::Archived),
            },
        )
        .expect("archive")
        .expect("deployment");

        assert_eq!(archived.desired_state, DesiredState::Stopped);
        assert_eq!(archived.observed_state, ObservedState::Stopped);
    }
}
