use crate::config::PlatformConfig;
use crate::events::EventBroker;
use crate::http::publish_snapshot_events;
use chrono::{DateTime, Utc};
use ploy_connectivity::{
    CancellationOutcome, CancellationRequest, ClaimGateway, ClaimRequest, ExecutionError,
    ExecutionOutcome, ExecutionRequest, LiveExecutionGateway, PolymarketClaimGateway,
    PolymarketExecutionGateway, RedeemablePosition, ReplaceOutcome, ReplaceRequest,
    StaticClaimGateway, TrackedOrder,
};
use ploy_deployments::{WorkerLaunchSpec, WorkerSupervisor};
use ploy_operator_contracts::{
    AccountClaimActionResponse, AccountClaimActionState, AccountClaimDetailResponse,
    AccountClaimStatus, AlertRecord, AlertSeverity, ClaimExecutionOutcome, ClaimExecutionRecord,
    ClaimLoopState, ClaimPositionState, DeploymentApplyRequest, DeploymentControlRequest,
    DeploymentState, DesiredState, FillSnapshot, IntentPurpose, ObservedState,
    OrderControlResponse, OrderReplaceRequest, OrderSnapshot, PaperIntentResponse,
    PnlSnapshotResponse, PositionSnapshotResponse, RedeemablePositionSnapshot,
    RiskSnapshotResponse, SystemMetrics, TradingIntentSnapshot, TradingStateSnapshot,
};
use ploy_platform::{
    AccountClaimDetail, AccountClaimSnapshot, AlertSignal, ControlPlane, DeploymentRecord,
};
use ploy_trading::{OrderState, TradeSide, TradingIntent, TradingRuntime, TradingRuntimeSnapshot};
use rust_decimal::Decimal;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use ploy_trading::FillRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileStatus {
    Applied(usize),
    Noop,
    BackoffActive,
}

#[derive(Debug)]
pub struct PloyDaemon {
    pub config: PlatformConfig,
    pub control_plane: ControlPlane,
    pub supervisor: WorkerSupervisor,
    pub trading: BTreeMap<String, TradingRuntime>,
    live_execution: Box<dyn LiveExecutionGateway>,
    claim_gateway: Box<dyn ClaimGateway>,
    live_reconcile_failures: u32,
    next_live_reconcile_at: Option<DateTime<Utc>>,
    last_live_reconcile_error: Option<String>,
}

impl PloyDaemon {
    pub fn boot(config: &PlatformConfig) -> io::Result<Self> {
        Self::boot_with_gateways(
            config,
            Box::new(PolymarketExecutionGateway::from_env()),
            Box::new(PolymarketClaimGateway::from_env()),
        )
    }

    pub fn boot_with_live_execution(
        config: &PlatformConfig,
        live_execution: Box<dyn LiveExecutionGateway>,
    ) -> io::Result<Self> {
        Self::boot_with_gateways(
            config,
            live_execution,
            Box::new(StaticClaimGateway::default()),
        )
    }

    pub fn boot_with_gateways(
        config: &PlatformConfig,
        live_execution: Box<dyn LiveExecutionGateway>,
        claim_gateway: Box<dyn ClaimGateway>,
    ) -> io::Result<Self> {
        let config = config.clone().normalized();
        let mut control_plane = ControlPlane::default();
        control_plane
            .system
            .set_status(format!("starting@{}", config.listen_addr));

        let mut daemon = Self {
            config: config.clone(),
            control_plane,
            supervisor: WorkerSupervisor::default(),
            trading: BTreeMap::new(),
            live_execution,
            claim_gateway,
            live_reconcile_failures: 0,
            next_live_reconcile_at: None,
            last_live_reconcile_error: None,
        };
        daemon.load_registry()?;
        daemon.load_trading_snapshots()?;
        daemon.load_claim_snapshots()?;
        if daemon.config.trading_state_file.exists() {
            daemon
                .control_plane
                .system
                .mark_recovering(&daemon.config.listen_addr);
        }
        daemon.tick();
        daemon.mark_runtime_healthy();
        daemon.refresh_observability();

        Ok(daemon)
    }

    pub fn write_runtime_snapshots(&mut self) -> io::Result<()> {
        if let Err(err) = self.load_registry() {
            self.control_plane.system.set_database_connected(false);
            self.control_plane
                .system
                .mark_degraded(&self.config.listen_addr);
            return Err(err);
        }
        self.control_plane.system.set_database_connected(true);
        self.tick();
        self.sync_account_claims();
        if let Err(err) = self.run_auto_claim_loops() {
            self.mark_claim_runtime_degraded(err);
        }
        match self.reconcile_live_fills() {
            Ok(ReconcileStatus::Applied(_) | ReconcileStatus::Noop) => self.mark_runtime_healthy(),
            Ok(ReconcileStatus::BackoffActive) => {}
            Err(err) => self.mark_live_runtime_degraded(err),
        }
        if let Err(err) = self.persist_registry() {
            self.control_plane.system.set_database_connected(false);
            self.control_plane
                .system
                .mark_degraded(&self.config.listen_addr);
            return Err(err);
        }
        self.control_plane.system.set_database_connected(true);
        if let Err(err) = fs::create_dir_all(&self.config.runtime_root) {
            self.control_plane
                .system
                .mark_degraded(&self.config.listen_addr);
            return Err(err);
        }
        write_json(
            &self.config.status_file,
            &self.control_plane.system.status(),
        )?;
        write_json(
            &self.config.deployment_status_file,
            &self.control_plane.deployments.summaries(),
        )?;
        write_json(&self.config.trading_state_file, &self.trading_state())?;
        write_json(&self.config.claim_state_file, &self.claim_details())?;
        write_json(&self.config.metrics_state_file, &self.system_metrics())?;
        write_json(&self.config.alerts_state_file, &self.active_alerts())?;
        Ok(())
    }

    pub fn inspect_deployment(&self, deployment_id: &str) -> Option<DeploymentRecord> {
        self.control_plane.deployments.get(deployment_id).cloned()
    }

    pub fn trading_state(&self) -> Vec<TradingStateSnapshot> {
        self.control_plane
            .deployments
            .records()
            .into_iter()
            .map(|record| {
                let snapshot = self
                    .trading
                    .get(&record.deployment_id)
                    .map(|runtime| runtime.snapshot(&BTreeMap::new()))
                    .unwrap_or_default();
                build_trading_state_snapshot(record, snapshot)
            })
            .collect()
    }

    pub fn claim_statuses(&self) -> Vec<AccountClaimStatus> {
        self.control_plane.accounts.statuses()
    }

    pub fn active_alerts(&self) -> Vec<AlertRecord> {
        self.control_plane.alerts.active()
    }

    pub fn system_metrics(&self) -> SystemMetrics {
        let alerts = self.active_alerts();
        let deployments = self.control_plane.deployments.records();
        let claims = self.control_plane.accounts.statuses();
        let trading: Vec<_> = deployments
            .iter()
            .map(|record| {
                self.trading
                    .get(&record.deployment_id)
                    .map(|runtime| runtime.snapshot(&BTreeMap::new()))
                    .unwrap_or_default()
            })
            .collect();

        SystemMetrics {
            deployments_total: deployments.len(),
            deployments_running: deployments
                .iter()
                .filter(|record| record.observed_state == ObservedState::Running)
                .count(),
            deployments_degraded: deployments
                .iter()
                .filter(|record| record.observed_state == ObservedState::Degraded)
                .count(),
            deployments_failed: deployments
                .iter()
                .filter(|record| record.observed_state == ObservedState::Failed)
                .count(),
            live_deployments: deployments
                .iter()
                .filter(|record| record.runtime_mode == "live")
                .count(),
            paper_deployments: deployments
                .iter()
                .filter(|record| record.runtime_mode == "paper")
                .count(),
            claim_accounts_total: claims.len(),
            claim_accounts_degraded: claims
                .iter()
                .filter(|status| status.loop_state == ClaimLoopState::Degraded)
                .count(),
            pending_intents: trading
                .iter()
                .map(|snapshot| snapshot.risk.pending_intents)
                .sum(),
            active_orders: trading
                .iter()
                .map(|snapshot| snapshot.risk.active_orders)
                .sum(),
            open_positions: trading
                .iter()
                .map(|snapshot| snapshot.risk.open_positions)
                .sum(),
            gross_exposure: trading
                .iter()
                .map(|snapshot| snapshot.risk.gross_exposure)
                .sum(),
            reserved_order_exposure: trading
                .iter()
                .map(|snapshot| snapshot.risk.reserved_order_exposure)
                .sum(),
            total_gross_exposure: trading
                .iter()
                .map(|snapshot| snapshot.risk.total_gross_exposure)
                .sum(),
            active_alert_count: alerts.len(),
            warning_alert_count: alerts
                .iter()
                .filter(|alert| alert.severity == AlertSeverity::Warning)
                .count(),
            critical_alert_count: alerts
                .iter()
                .filter(|alert| alert.severity == AlertSeverity::Critical)
                .count(),
        }
    }

    pub fn claim_details(&self) -> Vec<AccountClaimDetailResponse> {
        self.control_plane.accounts.details()
    }

    pub fn inspect_account_claims(&self, account_id: &str) -> Option<AccountClaimDetailResponse> {
        self.control_plane
            .accounts
            .detail(account_id)
            .map(AccountClaimDetail::response)
    }

    pub fn run_account_claims(
        &mut self,
        account_id: &str,
    ) -> io::Result<AccountClaimActionResponse> {
        self.refresh_account_claims(account_id, true)
    }

    pub fn rescan_account_claims(
        &mut self,
        account_id: &str,
    ) -> io::Result<AccountClaimActionResponse> {
        self.refresh_account_claims(account_id, false)
    }

    pub fn set_account_claim_enabled(
        &mut self,
        account_id: &str,
        enabled: bool,
    ) -> io::Result<AccountClaimActionResponse> {
        let Some(current) = self.control_plane.accounts.get(account_id).cloned() else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("account `{account_id}` was not found"),
            ));
        };
        if current.runtime_mode != "live" {
            return Ok(AccountClaimActionResponse {
                account_id: account_id.to_string(),
                state: AccountClaimActionState::NotSupported,
                message: format!("account `{account_id}` is not a live account"),
            });
        }
        self.control_plane.accounts.set_enabled(account_id, enabled);
        self.refresh_claim_metrics();
        self.refresh_observability();
        Ok(AccountClaimActionResponse {
            account_id: account_id.to_string(),
            state: AccountClaimActionState::Accepted,
            message: if enabled {
                "automatic claim loop resumed".to_string()
            } else {
                "automatic claim loop paused".to_string()
            },
        })
    }

    pub fn apply_deployment(
        &mut self,
        request: DeploymentApplyRequest,
    ) -> io::Result<DeploymentRecord> {
        let record = DeploymentRecord {
            deployment_id: request.deployment_id,
            bundle_id: request.bundle_id,
            runtime_mode: request.runtime_mode,
            account_id: request.account_id,
            max_gross_exposure: request.max_gross_exposure,
            deployment_state: request.deployment_state,
            desired_state: request.desired_state,
            observed_state: observed_state_for_desired(request.desired_state),
        };
        self.control_plane.deployments.upsert(record.clone());
        self.persist_registry()?;
        self.write_runtime_snapshots()?;
        Ok(self
            .control_plane
            .deployments
            .get(&record.deployment_id)
            .cloned()
            .expect("deployment persisted"))
    }

    pub fn control_deployment(
        &mut self,
        deployment_id: &str,
        request: DeploymentControlRequest,
    ) -> io::Result<Option<DeploymentRecord>> {
        let Some(existing) = self.control_plane.deployments.get(deployment_id).cloned() else {
            return Ok(None);
        };

        if let Some(deployment_state) = request.deployment_state {
            self.control_plane
                .deployments
                .set_deployment_state(deployment_id, deployment_state);
        }
        if let Some(desired_state) = request.desired_state {
            self.control_plane
                .deployments
                .set_desired_state(deployment_id, desired_state);
            self.control_plane
                .deployments
                .set_observed_state(deployment_id, observed_state_for_desired(desired_state));
        }

        if request.deployment_state.is_none() && request.desired_state.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("deployment `{deployment_id}` control request was empty"),
            ));
        }

        self.persist_registry()?;
        self.write_runtime_snapshots()?;
        Ok(self
            .control_plane
            .deployments
            .get(deployment_id)
            .cloned()
            .or(Some(existing)))
    }

    pub fn submit_intent(&mut self, intent: TradingIntent) -> io::Result<PaperIntentResponse> {
        let deployment = self
            .control_plane
            .deployments
            .get(&intent.deployment_id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "deployment not found"))?;

        if deployment.deployment_state == DeploymentState::Disabled
            || deployment.deployment_state == DeploymentState::Archived
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "deployment is {} and cannot accept intents",
                    deployment_state_wire(deployment.deployment_state)
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

        self.enforce_exposure_limit(&deployment, &intent)?;

        match deployment.runtime_mode.as_str() {
            "paper" => self.submit_paper_intent(intent),
            "live" => self.submit_live_intent(intent),
            runtime_mode => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported runtime mode: {runtime_mode}"),
            )),
        }
    }

    pub fn submit_paper_intent(
        &mut self,
        intent: TradingIntent,
    ) -> io::Result<PaperIntentResponse> {
        let deployment = self
            .control_plane
            .deployments
            .get(&intent.deployment_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "deployment not found"))?;

        if deployment.runtime_mode != "paper" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only paper deployments are supported by the local trading runtime",
            ));
        }
        if deployment.desired_state != DesiredState::Running {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "paper intents require a running deployment",
            ));
        }

        let order_id = format!("order-{}", intent.intent_id);
        let venue_order_id = format!("paper-{}", intent.intent_id);
        let runtime = self
            .trading
            .entry(intent.deployment_id.clone())
            .or_default();
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

    pub fn cancel_order(
        &mut self,
        deployment_id: &str,
        order_id: &str,
    ) -> io::Result<OrderControlResponse> {
        let deployment = self
            .control_plane
            .deployments
            .get(deployment_id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "deployment not found"))?;
        let runtime = self.trading.get_mut(deployment_id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "deployment has no trading state")
        })?;
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
                let cancel_result = self.live_execution.cancel(&CancellationRequest {
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
                    Err(err) => {
                        return Err(io_error_from_execution_error(err));
                    }
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
        &mut self,
        deployment_id: &str,
        order_id: &str,
        request: OrderReplaceRequest,
    ) -> io::Result<OrderControlResponse> {
        let deployment = self
            .control_plane
            .deployments
            .get(deployment_id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "deployment not found"))?;
        let runtime = self.trading.get(deployment_id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "deployment has no trading state")
        })?;
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

        self.enforce_order_replacement_exposure(&deployment, &order, &request)?;

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
                        format!(
                            "intent `{}` for order `{order_id}` was not found",
                            order.intent_id
                        ),
                    )
                })?;

            match self.live_execution.replace(&ReplaceRequest {
                order_id: order_id.to_string(),
                venue_order_id,
                token_id: order.token_id.clone(),
                side,
                quantity: request.quantity,
                limit_price: request.limit_price,
            }) {
                Ok(ReplaceOutcome::Replaced { venue_order_id }) => {
                    let runtime = self.trading.get_mut(deployment_id).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotFound, "deployment has no trading state")
                    })?;
                    let updated = runtime
                        .replace_order(
                            order_id,
                            request.quantity,
                            request.limit_price,
                            venue_order_id,
                        )
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::NotFound, "order not found")
                        })?;
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
                    if let Some(runtime) = self.trading.get_mut(deployment_id) {
                        let _ = runtime.record_order_error(order_id, err.to_string());
                    }
                    Err(io_error_from_execution_error(err))
                }
            }
        } else {
            let next_revision = order.revision + 1;
            let venue_order_id = format!("paper-{order_id}-r{next_revision}");
            let runtime = self.trading.get_mut(deployment_id).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "deployment has no trading state")
            })?;
            let updated = runtime
                .replace_order(
                    order_id,
                    request.quantity,
                    request.limit_price,
                    venue_order_id,
                )
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "order not found"))?;
            Ok(build_order_control_response(
                deployment_id.to_string(),
                updated,
            ))
        }
    }

    fn submit_live_intent(&mut self, intent: TradingIntent) -> io::Result<PaperIntentResponse> {
        let order_id = format!("order-{}", intent.intent_id);
        self.trading
            .entry(intent.deployment_id.clone())
            .or_default()
            .submit_intent(intent.clone(), order_id.clone());

        let outcome = self.live_execution.submit(&ExecutionRequest {
            order_id: order_id.clone(),
            token_id: intent.token_id.clone(),
            side: intent.side,
            quantity: intent.quantity,
            limit_price: intent.limit_price,
        });

        match outcome {
            Ok(ExecutionOutcome::Acknowledged { venue_order_id }) => {
                self.trading
                    .entry(intent.deployment_id.clone())
                    .or_default()
                    .acknowledge_order(&order_id, venue_order_id.clone());
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
                self.trading
                    .entry(intent.deployment_id.clone())
                    .or_default()
                    .reject_order(&order_id, reason.clone());
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
                let reason = err.to_string();
                self.trading
                    .entry(intent.deployment_id.clone())
                    .or_default()
                    .record_order_error(&order_id, reason);
                Err(io_error_from_execution_error(err))
            }
        }
    }

    pub fn reconcile_live_fills(&mut self) -> io::Result<ReconcileStatus> {
        if let Some(next_attempt_at) = self.next_live_reconcile_at {
            if Utc::now() < next_attempt_at {
                return Ok(ReconcileStatus::BackoffActive);
            }
        }

        let mut tracked_orders = Vec::new();
        let mut order_deployments = BTreeMap::new();

        for record in self.control_plane.deployments.records() {
            if record.runtime_mode != "live" || record.deployment_state == DeploymentState::Archived
            {
                continue;
            }

            let Some(runtime) = self.trading.get(&record.deployment_id) else {
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
                            OrderState::Acknowledged | OrderState::PartiallyFilled
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
            self.live_reconcile_failures = 0;
            self.next_live_reconcile_at = None;
            self.last_live_reconcile_error = None;
            self.control_plane.system.note_live_reconcile_healthy();
            return Ok(ReconcileStatus::Noop);
        }

        let fills = self
            .live_execution
            .reconcile_fills(&tracked_orders)
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;

        let mut recorded = 0;
        for fill in fills {
            let Some(deployment_id) = order_deployments.get(&fill.order_id) else {
                continue;
            };
            let Some(runtime) = self.trading.get_mut(deployment_id) else {
                continue;
            };
            if runtime.record_fill(fill) {
                recorded += 1;
            }
        }

        self.live_reconcile_failures = 0;
        self.next_live_reconcile_at = None;
        self.last_live_reconcile_error = None;
        self.control_plane.system.note_live_reconcile_healthy();

        Ok(ReconcileStatus::Applied(recorded))
    }

    fn latest_trade_time(&self) -> Option<DateTime<Utc>> {
        self.trading
            .values()
            .filter_map(TradingRuntime::last_fill_time)
            .max()
    }

    fn mark_runtime_healthy(&mut self) {
        self.refresh_claim_metrics();
        if self.control_plane.system.status().degraded_claim_accounts > 0 {
            self.control_plane
                .system
                .mark_degraded(&self.config.listen_addr);
            return;
        }
        self.control_plane.system.note_live_reconcile_healthy();
        self.control_plane
            .system
            .note_trade(self.latest_trade_time());
        if self.control_plane.system.is_degraded() {
            self.control_plane
                .system
                .mark_recovering(&self.config.listen_addr);
        } else if self
            .control_plane
            .system
            .status()
            .status
            .starts_with("recovering")
        {
            self.control_plane
                .system
                .mark_running(&self.config.listen_addr);
        } else {
            self.control_plane
                .system
                .mark_running(&self.config.listen_addr);
        }

        for record in self.control_plane.deployments.records() {
            if record.runtime_mode == "live"
                && record.desired_state == DesiredState::Running
                && record.observed_state == ObservedState::Degraded
            {
                self.control_plane
                    .deployments
                    .set_observed_state(&record.deployment_id, ObservedState::Running);
            }
        }
        self.refresh_observability();
    }

    fn mark_live_runtime_degraded(&mut self, err: io::Error) {
        self.control_plane
            .system
            .mark_degraded(&self.config.listen_addr);
        self.record_live_reconcile_failure(&err);

        for record in self.control_plane.deployments.records() {
            if record.runtime_mode != "live"
                || record.deployment_state == DeploymentState::Archived
                || record.desired_state != DesiredState::Running
            {
                continue;
            }

            self.control_plane
                .deployments
                .set_observed_state(&record.deployment_id, ObservedState::Degraded);
        }
        self.refresh_observability();
    }

    fn mark_claim_runtime_degraded(&mut self, err: io::Error) {
        self.control_plane
            .system
            .mark_degraded(&self.config.listen_addr);
        let degraded_accounts: BTreeSet<String> = self
            .control_plane
            .accounts
            .statuses()
            .into_iter()
            .filter(|status| status.loop_state == ClaimLoopState::Degraded)
            .map(|status| status.account_id)
            .collect();

        for record in self.control_plane.deployments.records() {
            if record.runtime_mode != "live" || !degraded_accounts.contains(&record.account_id) {
                continue;
            }
            self.control_plane
                .deployments
                .set_observed_state(&record.deployment_id, ObservedState::Degraded);
        }

        self.control_plane.audit.append(
            "claim_loop_degraded",
            format!(
                "error={} degraded_accounts={}",
                err,
                degraded_accounts.into_iter().collect::<Vec<_>>().join(",")
            ),
        );
        self.refresh_observability();
    }

    fn record_live_reconcile_failure(&mut self, err: &io::Error) {
        let failures = self.live_reconcile_failures.saturating_add(1);
        self.live_reconcile_failures = failures;
        let next_attempt_at = self.next_live_reconcile_at(failures);
        let reason = err.to_string();
        self.next_live_reconcile_at = Some(next_attempt_at);
        self.last_live_reconcile_error = Some(reason.clone());
        self.control_plane
            .system
            .note_live_reconcile_failure(failures, next_attempt_at, reason);
    }

    fn next_live_reconcile_at(&self, failures: u32) -> DateTime<Utc> {
        let backoff_ms = live_reconcile_backoff_ms(
            failures,
            self.config.live_reconcile_backoff_base_ms,
            self.config.live_reconcile_backoff_max_ms,
        );
        Utc::now() + chrono::Duration::milliseconds(backoff_ms as i64)
    }

    fn next_claim_retry_at(&self, failures: u32) -> DateTime<Utc> {
        let backoff_ms = live_reconcile_backoff_ms(
            failures,
            self.config.claim_backoff_base_ms,
            self.config.claim_backoff_max_ms,
        );
        Utc::now() + chrono::Duration::milliseconds(backoff_ms as i64)
    }

    #[cfg(test)]
    pub fn record_fill(&mut self, deployment_id: &str, fill: FillRecord) {
        if let Some(runtime) = self.trading.get_mut(deployment_id) {
            runtime.record_fill(fill);
        }
    }

    fn enforce_exposure_limit(
        &self,
        deployment: &DeploymentRecord,
        intent: &TradingIntent,
    ) -> io::Result<()> {
        let Some(max_gross_exposure) = deployment.max_gross_exposure else {
            return Ok(());
        };
        if !intent_counts_toward_exposure(intent.purpose) {
            return Ok(());
        }

        let current_total_exposure = self.account_total_exposure(&deployment.account_id);
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

    fn enforce_order_replacement_exposure(
        &self,
        deployment: &DeploymentRecord,
        order: &ploy_trading::OrderRecord,
        request: &OrderReplaceRequest,
    ) -> io::Result<()> {
        let Some(max_gross_exposure) = deployment.max_gross_exposure else {
            return Ok(());
        };
        let Some(runtime) = self.trading.get(&deployment.deployment_id) else {
            return Ok(());
        };
        let Some(intent) = runtime.intent(&order.intent_id) else {
            return Ok(());
        };
        if !intent_counts_toward_exposure(intent.purpose) {
            return Ok(());
        }

        let current_total_exposure = self.account_total_exposure(&deployment.account_id);
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

    fn account_total_exposure(&self, account_id: &str) -> Decimal {
        self.control_plane
            .deployments
            .records()
            .into_iter()
            .filter(|deployment| deployment.account_id == account_id)
            .map(|deployment| {
                self.trading
                    .get(&deployment.deployment_id)
                    .map(|runtime| runtime.snapshot(&BTreeMap::new()).risk.total_gross_exposure)
                    .unwrap_or_default()
            })
            .sum()
    }

    fn load_registry(&mut self) -> io::Result<()> {
        if !self.config.registry_file.exists() {
            return Ok(());
        }

        let raw = fs::read_to_string(&self.config.registry_file)?;
        if raw.trim().is_empty() {
            return Ok(());
        }

        let records: Vec<DeploymentRecord> = serde_json::from_str(&raw)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

        for record in records {
            let deployment_id = record.deployment_id.clone();
            let desired_state = record.desired_state;
            let bundle_id = record.bundle_id.clone();
            let runtime_mode = record.runtime_mode.clone();
            self.control_plane.deployments.upsert(record);
            self.trading.entry(deployment_id.clone()).or_default();

            if desired_state == DesiredState::Running {
                self.supervisor.start(WorkerLaunchSpec {
                    deployment_id: deployment_id.clone(),
                    bundle_id,
                    runtime_mode,
                    desired_state,
                });
                if let Some(status) = self.supervisor.heartbeat(&deployment_id) {
                    self.control_plane
                        .deployments
                        .set_observed_state(&deployment_id, status.observed_state);
                }
            }
        }

        Ok(())
    }

    fn load_trading_snapshots(&mut self) -> io::Result<()> {
        if !self.config.trading_state_file.exists() {
            return Ok(());
        }

        let raw = fs::read_to_string(&self.config.trading_state_file)?;
        if raw.trim().is_empty() {
            return Ok(());
        }

        let snapshots: Vec<TradingStateSnapshot> = serde_json::from_str(&raw)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

        for snapshot in snapshots {
            if self
                .control_plane
                .deployments
                .get(&snapshot.deployment_id)
                .is_none()
            {
                continue;
            }

            let deployment_id = snapshot.deployment_id.clone();
            self.trading
                .insert(deployment_id, restore_trading_runtime(snapshot)?);
        }

        Ok(())
    }

    fn load_claim_snapshots(&mut self) -> io::Result<()> {
        if !self.config.claim_state_file.exists() {
            return Ok(());
        }

        let raw = fs::read_to_string(&self.config.claim_state_file)?;
        if raw.trim().is_empty() {
            return Ok(());
        }

        let records: Vec<AccountClaimDetail> = serde_json::from_str(&raw)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        self.control_plane.accounts.restore(records);
        Ok(())
    }

    fn sync_account_claims(&mut self) {
        let live_accounts: BTreeSet<String> = self
            .control_plane
            .deployments
            .records()
            .into_iter()
            .filter(|record| {
                record.runtime_mode == "live"
                    && record.deployment_state != DeploymentState::Archived
            })
            .map(|record| record.account_id)
            .collect();

        self.control_plane.accounts.retain_accounts(&live_accounts);
        for account_id in &live_accounts {
            if self.control_plane.accounts.get(account_id).is_none() {
                self.control_plane
                    .accounts
                    .upsert(AccountClaimSnapshot::for_runtime_mode(account_id, "live"));
            }
        }
        self.refresh_claim_metrics();
    }

    fn refresh_account_claims(
        &mut self,
        account_id: &str,
        execute_claims: bool,
    ) -> io::Result<AccountClaimActionResponse> {
        let Some(status) = self.control_plane.accounts.get(account_id).cloned() else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("account `{account_id}` was not found"),
            ));
        };
        if status.runtime_mode != "live" {
            return Ok(AccountClaimActionResponse {
                account_id: account_id.to_string(),
                state: AccountClaimActionState::NotSupported,
                message: format!("account `{account_id}` is not a live account"),
            });
        }

        let scan_time = Utc::now();
        let positions = self
            .claim_gateway
            .discover_redeemable_positions(account_id)
            .map_err(io_error_from_claim_error)?;
        self.control_plane
            .accounts
            .mark_scan_complete(account_id, scan_time);

        let detected_count = positions.len();
        let remaining = if execute_claims {
            let (remaining, claim_error) =
                self.execute_account_claims(account_id, positions, scan_time);
            if let Some(err) = claim_error {
                let failures = self
                    .control_plane
                    .accounts
                    .get(account_id)
                    .map(|current| current.consecutive_failures.saturating_add(1))
                    .unwrap_or(1);
                let next_retry_at = Some(self.next_claim_retry_at(failures));
                self.control_plane.accounts.mark_degraded(
                    account_id,
                    err.to_string(),
                    next_retry_at,
                );
                self.mark_claim_runtime_degraded(err);
            } else {
                self.control_plane.accounts.mark_running(account_id);
            }
            remaining
        } else {
            positions
                .into_iter()
                .map(|position| {
                    redeemable_position_snapshot(
                        account_id,
                        scan_time,
                        position,
                        ClaimPositionState::Detected,
                    )
                })
                .collect()
        };

        let claimed_count = detected_count.saturating_sub(remaining.len());
        self.control_plane
            .accounts
            .set_redeemable_positions(account_id, remaining);
        self.refresh_claim_metrics();
        self.refresh_observability();

        let message = if execute_claims {
            format!("claim run completed: detected={detected_count} claimed={claimed_count}")
        } else {
            format!("claim rescan completed: detected={detected_count}")
        };

        Ok(AccountClaimActionResponse {
            account_id: account_id.to_string(),
            state: AccountClaimActionState::Accepted,
            message,
        })
    }

    fn run_auto_claim_loops(&mut self) -> io::Result<()> {
        let account_ids: Vec<String> = self
            .control_plane
            .accounts
            .statuses()
            .into_iter()
            .filter(|status| {
                status.enabled
                    && status.runtime_mode == "live"
                    && status.loop_state != ClaimLoopState::Paused
                    && status
                        .next_retry_at
                        .map(|next| next <= Utc::now())
                        .unwrap_or(true)
            })
            .map(|status| status.account_id)
            .collect();

        for account_id in account_ids {
            let scan_time = Utc::now();
            let positions = self
                .claim_gateway
                .discover_redeemable_positions(&account_id)
                .map_err(io_error_from_claim_error)?;
            self.control_plane
                .accounts
                .mark_scan_complete(&account_id, scan_time);

            let (remaining, claim_error) =
                self.execute_account_claims(&account_id, positions, scan_time);
            self.control_plane
                .accounts
                .set_redeemable_positions(&account_id, remaining);
            if let Some(err) = claim_error {
                let failures = self
                    .control_plane
                    .accounts
                    .get(&account_id)
                    .map(|status| status.consecutive_failures.saturating_add(1))
                    .unwrap_or(1);
                let next_retry_at = Some(self.next_claim_retry_at(failures));
                self.control_plane.accounts.mark_degraded(
                    &account_id,
                    err.to_string(),
                    next_retry_at,
                );
                self.mark_claim_runtime_degraded(err);
            } else {
                self.control_plane.accounts.mark_running(&account_id);
            }
        }

        self.refresh_claim_metrics();
        self.refresh_observability();
        Ok(())
    }

    fn execute_account_claims(
        &mut self,
        account_id: &str,
        positions: Vec<RedeemablePosition>,
        detected_at: DateTime<Utc>,
    ) -> (Vec<RedeemablePositionSnapshot>, Option<io::Error>) {
        let mut remaining = Vec::new();
        let mut last_error = None;

        for position in positions {
            let request = ClaimRequest {
                account_id: position.account_id.clone(),
                wallet_address: position.wallet_address.clone(),
                condition_id: position.condition_id.clone(),
                outcome_indexes: position.outcome_indexes.clone(),
                outcome_amounts: position.outcome_amounts.clone(),
                negative_risk: position.negative_risk,
            };
            let amount_claimed = position
                .outcome_amounts
                .iter()
                .copied()
                .fold(Decimal::ZERO, |acc, value| acc + value);

            match self.claim_gateway.claim(&request) {
                Ok(result) => {
                    self.control_plane.accounts.append_claim_record(
                        account_id,
                        ClaimExecutionRecord {
                            claim_id: next_claim_id(account_id, &position.condition_id),
                            account_id: account_id.to_string(),
                            condition_id: position.condition_id.clone(),
                            submitted_at: detected_at,
                            completed_at: Some(Utc::now()),
                            tx_hash: Some(result.tx_hash),
                            amount_claimed: result.amount_claimed,
                            outcome: ClaimExecutionOutcome::Confirmed,
                            error_message: None,
                        },
                    );
                }
                Err(err) => {
                    let error_message = err.to_string();
                    last_error = Some(io_error_from_claim_error(err));
                    self.control_plane.accounts.append_claim_record(
                        account_id,
                        ClaimExecutionRecord {
                            claim_id: next_claim_id(account_id, &position.condition_id),
                            account_id: account_id.to_string(),
                            condition_id: position.condition_id.clone(),
                            submitted_at: detected_at,
                            completed_at: Some(Utc::now()),
                            tx_hash: None,
                            amount_claimed,
                            outcome: ClaimExecutionOutcome::Failed,
                            error_message: Some(error_message),
                        },
                    );
                    remaining.push(redeemable_position_snapshot(
                        account_id,
                        detected_at,
                        position,
                        ClaimPositionState::Failed,
                    ));
                }
            }
        }

        (remaining, last_error)
    }

    fn tick(&mut self) {
        let records = self.control_plane.deployments.records();

        for record in records {
            match record.desired_state {
                DesiredState::Running => {
                    if self.supervisor.status(&record.deployment_id).is_none() {
                        self.supervisor.start(WorkerLaunchSpec {
                            deployment_id: record.deployment_id.clone(),
                            bundle_id: record.bundle_id.clone(),
                            runtime_mode: record.runtime_mode.clone(),
                            desired_state: record.desired_state,
                        });
                    }
                    if let Some(status) = self.supervisor.heartbeat(&record.deployment_id) {
                        self.control_plane
                            .deployments
                            .set_observed_state(&record.deployment_id, status.observed_state);
                    }
                }
                DesiredState::Paused => {
                    if let Some(status) = self.supervisor.pause(&record.deployment_id) {
                        self.control_plane
                            .deployments
                            .set_observed_state(&record.deployment_id, status.observed_state);
                    } else {
                        self.control_plane
                            .deployments
                            .set_observed_state(&record.deployment_id, ObservedState::Paused);
                    }
                }
                DesiredState::Stopped => {
                    if let Some(status) = self.supervisor.stop(&record.deployment_id) {
                        self.control_plane
                            .deployments
                            .set_observed_state(&record.deployment_id, status.observed_state);
                    } else {
                        self.control_plane
                            .deployments
                            .set_observed_state(&record.deployment_id, ObservedState::Stopped);
                    }
                }
            }
        }
    }

    fn persist_registry(&self) -> io::Result<()> {
        write_json(
            &self.config.registry_file,
            &self.control_plane.deployments.records(),
        )
    }

    fn refresh_claim_metrics(&mut self) {
        let details = self.control_plane.accounts.records();
        let last_claim_time = details
            .iter()
            .filter_map(|detail| detail.status.last_claim_at)
            .max();
        let degraded_claim_accounts = details
            .iter()
            .filter(|detail| detail.status.loop_state == ClaimLoopState::Degraded)
            .count();
        let pending_redeemable_count = details
            .iter()
            .map(|detail| detail.status.pending_redeemable_count)
            .sum();
        let pending_redeemable_notional = details
            .iter()
            .map(|detail| detail.status.pending_redeemable_notional)
            .fold(Decimal::ZERO, |acc, value| acc + value);

        self.control_plane.system.note_claims(
            last_claim_time,
            degraded_claim_accounts,
            pending_redeemable_count,
            pending_redeemable_notional,
        );
    }

    fn refresh_observability(&mut self) {
        self.control_plane
            .alerts
            .reconcile(self.derive_active_alerts());
    }

    fn derive_active_alerts(&self) -> Vec<AlertSignal> {
        let mut alerts = Vec::new();
        let system = self.control_plane.system.status();

        if system.status.starts_with("degraded") {
            alerts.push(AlertSignal {
                alert_id: "system_degraded".to_string(),
                severity: AlertSeverity::Critical,
                kind: "system_degraded".to_string(),
                source: "ployd".to_string(),
                resource_type: "system".to_string(),
                resource_id: None,
                message: format!("platform runtime is degraded ({})", system.status),
            });
        } else if system.status.starts_with("recovering") {
            alerts.push(AlertSignal {
                alert_id: "system_recovering".to_string(),
                severity: AlertSeverity::Warning,
                kind: "system_recovering".to_string(),
                source: "ployd".to_string(),
                resource_type: "system".to_string(),
                resource_id: None,
                message: format!("platform runtime is recovering ({})", system.status),
            });
        }

        if system.live_reconcile_failures > 0 {
            alerts.push(AlertSignal {
                alert_id: "live_reconcile_degraded".to_string(),
                severity: AlertSeverity::Critical,
                kind: "live_reconcile_degraded".to_string(),
                source: "ployd".to_string(),
                resource_type: "system".to_string(),
                resource_id: None,
                message: format!(
                    "live reconcile failures={} next_retry_at={}",
                    system.live_reconcile_failures,
                    system
                        .next_live_reconcile_at
                        .map(|value| value.to_rfc3339())
                        .unwrap_or_else(|| "-".to_string())
                ),
            });
        }

        if system.degraded_claim_accounts > 0 {
            alerts.push(AlertSignal {
                alert_id: "claim_loop_degraded".to_string(),
                severity: AlertSeverity::Warning,
                kind: "claim_loop_degraded".to_string(),
                source: "ployd".to_string(),
                resource_type: "claims".to_string(),
                resource_id: None,
                message: format!("degraded claim accounts={}", system.degraded_claim_accounts),
            });
        }

        if system.pending_redeemable_count > 0 {
            alerts.push(AlertSignal {
                alert_id: "pending_redeemables_present".to_string(),
                severity: AlertSeverity::Info,
                kind: "pending_redeemables_present".to_string(),
                source: "ployd".to_string(),
                resource_type: "claims".to_string(),
                resource_id: None,
                message: format!(
                    "pending redeemables count={} notional={}",
                    system.pending_redeemable_count, system.pending_redeemable_notional
                ),
            });
        }

        for record in self.control_plane.deployments.records() {
            match record.observed_state {
                ObservedState::Degraded => alerts.push(AlertSignal {
                    alert_id: format!("deployment_degraded:{}", record.deployment_id),
                    severity: AlertSeverity::Warning,
                    kind: "deployment_degraded".to_string(),
                    source: "ployd".to_string(),
                    resource_type: "deployment".to_string(),
                    resource_id: Some(record.deployment_id.clone()),
                    message: format!(
                        "deployment {} is degraded (mode={} account={})",
                        record.deployment_id, record.runtime_mode, record.account_id
                    ),
                }),
                ObservedState::Failed => alerts.push(AlertSignal {
                    alert_id: format!("deployment_failed:{}", record.deployment_id),
                    severity: AlertSeverity::Critical,
                    kind: "deployment_failed".to_string(),
                    source: "ployd".to_string(),
                    resource_type: "deployment".to_string(),
                    resource_id: Some(record.deployment_id.clone()),
                    message: format!(
                        "deployment {} failed (mode={} account={})",
                        record.deployment_id, record.runtime_mode, record.account_id
                    ),
                }),
                _ => {}
            }
        }

        alerts
    }
}

fn build_trading_state_snapshot(
    record: DeploymentRecord,
    snapshot: TradingRuntimeSnapshot,
) -> TradingStateSnapshot {
    TradingStateSnapshot {
        deployment_id: record.deployment_id,
        runtime_mode: record.runtime_mode,
        intents: snapshot
            .intents
            .into_iter()
            .map(|intent| TradingIntentSnapshot {
                intent_id: intent.intent_id,
                market_id: intent.market_id,
                token_id: intent.token_id,
                side: trade_side_wire(intent.side),
                quantity: intent.quantity,
                limit_price: intent.limit_price,
                purpose: intent_purpose_wire(intent.purpose),
                created_at: intent.created_at,
            })
            .collect(),
        orders: snapshot
            .orders
            .into_iter()
            .map(|order| OrderSnapshot {
                order_id: order.order_id,
                intent_id: order.intent_id,
                token_id: order.token_id,
                requested_qty: order.requested_qty,
                limit_price: order.limit_price,
                venue_order_id: order.venue_order_id,
                venue_order_history: order.venue_order_history,
                revision: order.revision,
                state: order_state_wire(order.state),
                filled_qty: order.filled_qty,
                rejection_reason: order.rejection_reason,
                last_error: order.last_error,
            })
            .collect(),
        fills: snapshot
            .fills
            .into_iter()
            .map(|fill| FillSnapshot {
                fill_id: fill.fill_id,
                order_id: fill.order_id,
                token_id: fill.token_id,
                side: trade_side_wire(fill.side),
                quantity: fill.quantity,
                price: fill.price,
                fee: fill.fee,
                timestamp: fill.timestamp,
            })
            .collect(),
        positions: snapshot
            .positions
            .into_iter()
            .map(|position| PositionSnapshotResponse {
                token_id: position.token_id,
                net_qty: position.net_qty,
                avg_entry_price: position.avg_entry_price,
                realized_pnl: position.realized_pnl,
            })
            .collect(),
        pnl: PnlSnapshotResponse {
            realized_pnl: snapshot.pnl.realized_pnl,
            unrealized_pnl: snapshot.pnl.unrealized_pnl,
            total_fees: snapshot.pnl.total_fees,
            net_pnl: snapshot.pnl.net_pnl(),
        },
        risk: RiskSnapshotResponse {
            pending_intents: snapshot.risk.pending_intents,
            active_orders: snapshot.risk.active_orders,
            open_positions: snapshot.risk.open_positions,
            gross_exposure: snapshot.risk.gross_exposure,
            reserved_order_exposure: snapshot.risk.reserved_order_exposure,
            total_gross_exposure: snapshot.risk.total_gross_exposure,
        },
    }
}

fn redeemable_position_snapshot(
    account_id: &str,
    detected_at: DateTime<Utc>,
    position: RedeemablePosition,
    claim_state: ClaimPositionState,
) -> RedeemablePositionSnapshot {
    RedeemablePositionSnapshot {
        account_id: account_id.to_string(),
        condition_id: position.condition_id,
        market_id: position.market_id,
        token_ids: position.token_ids,
        outcome_labels: position.outcome_labels,
        redeemable_size: position.redeemable_size,
        estimated_payout: position.estimated_payout,
        detected_at,
        claim_state,
    }
}

fn next_claim_id(account_id: &str, condition_id: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("duration since epoch")
        .as_nanos();
    format!("claim-{account_id}-{condition_id}-{nanos}")
}

fn restore_trading_runtime(snapshot: TradingStateSnapshot) -> io::Result<TradingRuntime> {
    let deployment_id = snapshot.deployment_id.clone();
    let intents = snapshot
        .intents
        .into_iter()
        .map(|intent| {
            Ok(TradingIntent {
                intent_id: intent.intent_id,
                deployment_id: deployment_id.clone(),
                market_id: intent.market_id,
                token_id: intent.token_id,
                side: trade_side_from_wire(&intent.side)?,
                quantity: intent.quantity,
                limit_price: intent.limit_price,
                purpose: intent_purpose_from_contract(intent.purpose),
                created_at: intent.created_at,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let orders = snapshot
        .orders
        .into_iter()
        .map(|order| {
            Ok(ploy_trading::OrderRecord {
                order_id: order.order_id,
                intent_id: order.intent_id,
                deployment_id: deployment_id.clone(),
                token_id: order.token_id,
                requested_qty: order.requested_qty,
                limit_price: order.limit_price,
                venue_order_id: order.venue_order_id,
                venue_order_history: order.venue_order_history,
                revision: order.revision,
                state: order_state_from_wire(&order.state)?,
                filled_qty: order.filled_qty,
                rejection_reason: order.rejection_reason,
                last_error: order.last_error,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let fills = snapshot
        .fills
        .into_iter()
        .map(|fill| {
            Ok(ploy_trading::FillRecord {
                fill_id: fill.fill_id,
                order_id: fill.order_id,
                token_id: fill.token_id,
                side: trade_side_from_wire(&fill.side)?,
                quantity: fill.quantity,
                price: fill.price,
                fee: fill.fee,
                timestamp: fill.timestamp,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;

    Ok(TradingRuntime::restore(TradingRuntimeSnapshot {
        intents,
        orders,
        fills,
        positions: Vec::new(),
        pnl: Default::default(),
        risk: Default::default(),
    }))
}

fn build_order_control_response(
    deployment_id: String,
    order: &ploy_trading::OrderRecord,
) -> OrderControlResponse {
    OrderControlResponse {
        deployment_id,
        order_id: order.order_id.clone(),
        state: order_state_wire(order.state),
        venue_order_id: order.venue_order_id.clone(),
        venue_order_history: order.venue_order_history.clone(),
        revision: order.revision,
        requested_qty: order.requested_qty,
        limit_price: order.limit_price,
        rejection_reason: order.rejection_reason.clone(),
        last_error: order.last_error.clone(),
        filled_qty: order.filled_qty,
    }
}

fn trade_side_wire(side: TradeSide) -> String {
    match side {
        TradeSide::Buy => "buy".to_string(),
        TradeSide::Sell => "sell".to_string(),
    }
}

fn trade_side_from_wire(side: &str) -> io::Result<TradeSide> {
    match side {
        "buy" => Ok(TradeSide::Buy),
        "sell" => Ok(TradeSide::Sell),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported trade side `{other}`"),
        )),
    }
}

fn order_state_wire(state: OrderState) -> String {
    match state {
        OrderState::Pending => "pending".to_string(),
        OrderState::Acknowledged => "acknowledged".to_string(),
        OrderState::PartiallyFilled => "partially_filled".to_string(),
        OrderState::Filled => "filled".to_string(),
        OrderState::Canceled => "canceled".to_string(),
        OrderState::Rejected => "rejected".to_string(),
    }
}

fn order_state_from_wire(state: &str) -> io::Result<OrderState> {
    match state {
        "pending" => Ok(OrderState::Pending),
        "acknowledged" => Ok(OrderState::Acknowledged),
        "partially_filled" => Ok(OrderState::PartiallyFilled),
        "filled" => Ok(OrderState::Filled),
        "canceled" => Ok(OrderState::Canceled),
        "rejected" => Ok(OrderState::Rejected),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported order state `{other}`"),
        )),
    }
}

fn intent_purpose_wire(purpose: ploy_trading::IntentPurpose) -> IntentPurpose {
    match purpose {
        ploy_trading::IntentPurpose::Entry => IntentPurpose::Entry,
        ploy_trading::IntentPurpose::Exit => IntentPurpose::Exit,
        ploy_trading::IntentPurpose::Reduce => IntentPurpose::Reduce,
        ploy_trading::IntentPurpose::Hedge => IntentPurpose::Hedge,
        ploy_trading::IntentPurpose::Cancel => IntentPurpose::Cancel,
    }
}

fn intent_purpose_from_contract(purpose: IntentPurpose) -> ploy_trading::IntentPurpose {
    match purpose {
        IntentPurpose::Entry => ploy_trading::IntentPurpose::Entry,
        IntentPurpose::Exit => ploy_trading::IntentPurpose::Exit,
        IntentPurpose::Reduce => ploy_trading::IntentPurpose::Reduce,
        IntentPurpose::Hedge => ploy_trading::IntentPurpose::Hedge,
        IntentPurpose::Cancel => ploy_trading::IntentPurpose::Cancel,
    }
}

fn deployment_state_wire(state: DeploymentState) -> &'static str {
    match state {
        DeploymentState::Enabled => "enabled",
        DeploymentState::Draining => "draining",
        DeploymentState::Disabled => "disabled",
        DeploymentState::Archived => "archived",
    }
}

fn intent_counts_toward_exposure(purpose: ploy_trading::IntentPurpose) -> bool {
    matches!(
        purpose,
        ploy_trading::IntentPurpose::Entry | ploy_trading::IntentPurpose::Hedge
    )
}

fn intent_allowed_while_draining(purpose: ploy_trading::IntentPurpose) -> bool {
    !matches!(purpose, ploy_trading::IntentPurpose::Entry)
}

fn observed_state_for_desired(desired_state: DesiredState) -> ObservedState {
    match desired_state {
        DesiredState::Running => ObservedState::Starting,
        DesiredState::Paused => ObservedState::Paused,
        DesiredState::Stopped => ObservedState::Stopped,
    }
}

fn live_reconcile_backoff_ms(failures: u32, base_ms: u64, max_ms: u64) -> u64 {
    if failures == 0 {
        return 0;
    }
    let exponent = failures.saturating_sub(1).min(10);
    let scaled = base_ms.saturating_mul(2_u64.saturating_pow(exponent));
    scaled.min(max_ms.max(base_ms))
}

fn io_error_from_execution_error(err: ExecutionError) -> io::Error {
    match err {
        ExecutionError::Validation(message) => io::Error::new(io::ErrorKind::InvalidInput, message),
        ExecutionError::Configuration(message) => {
            io::Error::new(io::ErrorKind::InvalidData, message)
        }
        ExecutionError::Transport(message) => {
            io::Error::new(io::ErrorKind::ConnectionAborted, message)
        }
    }
}

fn io_error_from_claim_error(err: ploy_connectivity::ClaimError) -> io::Error {
    match err {
        ploy_connectivity::ClaimError::Validation(message) => {
            io::Error::new(io::ErrorKind::InvalidInput, message)
        }
        ploy_connectivity::ClaimError::Configuration(message) => {
            io::Error::new(io::ErrorKind::InvalidData, message)
        }
        ploy_connectivity::ClaimError::Transport(message) => {
            io::Error::new(io::ErrorKind::ConnectionAborted, message)
        }
    }
}

pub fn next_paper_intent_id(deployment_id: &str) -> String {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_millis();
    format!("{deployment_id}-{unique}")
}

pub fn run_shared_forever(
    daemon: Arc<Mutex<PloyDaemon>>,
    events: Arc<EventBroker>,
) -> io::Result<()> {
    loop {
        let tick_interval_ms = {
            let mut daemon = daemon
                .lock()
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "daemon lock poisoned"))?;
            if let Err(err) = daemon.write_runtime_snapshots() {
                eprintln!("ployd tick degraded: {err}");
            }
            publish_snapshot_events(&daemon, &events);
            daemon.config.tick_interval_ms
        };
        thread::sleep(Duration::from_millis(tick_interval_ms));
    }
}

fn write_json<T>(path: &Path, value: &T) -> io::Result<()>
where
    T: Serialize,
{
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    let body = serde_json::to_vec_pretty(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    fs::write(path, body)
}

#[cfg(test)]
mod tests {
    use super::{live_reconcile_backoff_ms, PloyDaemon, ReconcileStatus};
    use crate::config::PlatformConfig;
    use ploy_connectivity::{
        CancellationOutcome, CancellationRequest, ExecutionError, ExecutionOutcome,
        ExecutionRequest, LiveExecutionGateway, ReplaceOutcome, ReplaceRequest,
        StaticExecutionGateway,
    };
    use ploy_operator_contracts::{
        DeploymentApplyRequest, DeploymentState, DesiredState, ObservedState,
    };
    use ploy_trading::{FillRecord, IntentPurpose, TradeSide, TradingIntent};
    use rust_decimal_macros::dec;
    use std::fs;
    use std::io::ErrorKind;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployd-{label}-{unique}"))
    }

    #[derive(Debug, Default, Clone)]
    struct FlakyReconcileGateway {
        attempts: Arc<Mutex<usize>>,
    }

    impl LiveExecutionGateway for FlakyReconcileGateway {
        fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
            Ok(ExecutionOutcome::Acknowledged {
                venue_order_id: "venue-live-health-1".to_string(),
            })
        }

        fn cancel(
            &self,
            _request: &CancellationRequest,
        ) -> Result<CancellationOutcome, ExecutionError> {
            Ok(CancellationOutcome::Canceled)
        }

        fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
            Ok(ReplaceOutcome::Replaced {
                venue_order_id: "venue-live-health-2".to_string(),
            })
        }

        fn reconcile_fills(
            &self,
            _tracked_orders: &[ploy_connectivity::TrackedOrder],
        ) -> Result<Vec<FillRecord>, ExecutionError> {
            let mut attempts = self.attempts.lock().expect("attempts lock");
            *attempts += 1;
            if *attempts == 1 {
                Err(ExecutionError::Transport("gateway offline".to_string()))
            } else {
                Ok(Vec::new())
            }
        }
    }

    #[test]
    fn daemon_loads_platform_config() {
        let config = PlatformConfig {
            listen_addr: "127.0.0.1:9090".to_string(),
            ..PlatformConfig::default()
        };

        let daemon = PloyDaemon::boot(&config).expect("boot");
        let status = daemon.control_plane.system.status();
        assert_eq!(status.status, "running@127.0.0.1:9090");
    }

    #[test]
    fn daemon_writes_runtime_snapshots_for_operator_clients() {
        let root = temp_dir("snapshots");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "bundle_id": "example",
                    "runtime_mode": "paper",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file: registry_file.clone(),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon.write_runtime_snapshots().expect("write snapshots");

        let status: ploy_operator_contracts::SystemStatus =
            serde_json::from_str(&fs::read_to_string(&config.status_file).expect("status file"))
                .expect("status json");
        assert!(status.status.starts_with("running@"));
        assert!(status.database_connected);

        let deployments: Vec<ploy_operator_contracts::DeploymentSummary> = serde_json::from_str(
            &fs::read_to_string(&config.deployment_status_file).expect("deployment file"),
        )
        .expect("deployment json");
        assert_eq!(deployments.len(), 1);
        assert_eq!(deployments[0].deployment_id, "example.paper");
        assert_eq!(deployments[0].desired_state, DesiredState::Running);
        assert_eq!(deployments[0].observed_state, ObservedState::Running);
    }

    #[test]
    fn daemon_records_paper_trade_into_trading_state_snapshot() {
        let root = temp_dir("trading-state");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "bundle_id": "example",
                    "runtime_mode": "paper",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file: registry_file.clone(),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon
            .submit_paper_intent(TradingIntent {
                intent_id: "intent-1".to_string(),
                deployment_id: "example.paper".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(5),
                limit_price: Some(dec!(0.40)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit intent");
        daemon.record_fill(
            "example.paper",
            FillRecord {
                fill_id: "fill-1".to_string(),
                order_id: "order-intent-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(5),
                price: dec!(0.40),
                fee: dec!(0.05),
                timestamp: chrono::Utc::now(),
            },
        );
        daemon.write_runtime_snapshots().expect("write snapshots");

        let trading_state: Vec<ploy_operator_contracts::TradingStateSnapshot> =
            serde_json::from_str(
                &fs::read_to_string(&config.trading_state_file).expect("trading state file"),
            )
            .expect("trading state json");
        assert_eq!(trading_state.len(), 1);
        assert_eq!(trading_state[0].deployment_id, "example.paper");
        assert_eq!(trading_state[0].orders.len(), 1);
        assert_eq!(trading_state[0].fills.len(), 1);
        assert_eq!(trading_state[0].positions.len(), 1);
        assert_eq!(trading_state[0].risk.open_positions, 1);
    }

    #[test]
    fn daemon_rejects_paper_intent_when_deployment_is_not_running() {
        let root = temp_dir("paper-intent-gate");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "bundle_id": "example",
                    "runtime_mode": "paper",
                    "desired_state": "paused",
                    "observed_state": "paused"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        let err = daemon
            .submit_paper_intent(TradingIntent {
                intent_id: "intent-1".to_string(),
                deployment_id: "example.paper".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(5),
                limit_price: Some(dec!(0.40)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect_err("paused deployment should reject intents");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn daemon_routes_live_intent_into_acknowledged_order_snapshot() {
        let root = temp_dir("live-intent-ack");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("venue-live-1")),
        )
        .expect("boot");
        let response = daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-1".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(3),
                limit_price: Some(dec!(0.44)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        assert_eq!(response.state, "acknowledged");

        let trading_state = daemon.trading_state();
        assert_eq!(trading_state.len(), 1);
        assert_eq!(trading_state[0].deployment_id, "example.live");
        assert_eq!(trading_state[0].orders.len(), 1);
        assert_eq!(trading_state[0].orders[0].state, "acknowledged");
        assert_eq!(
            trading_state[0].orders[0].venue_order_id.as_deref(),
            Some("venue-live-1")
        );
    }

    #[test]
    fn daemon_records_live_rejection_in_canonical_ledger() {
        let root = temp_dir("live-intent-reject");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::rejected("market closed")),
        )
        .expect("boot");
        let response = daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-2".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Sell,
                quantity: dec!(2),
                limit_price: Some(dec!(0.55)),
                purpose: IntentPurpose::Reduce,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        assert_eq!(response.state, "rejected");

        let trading_state = daemon.trading_state();
        assert_eq!(trading_state[0].orders.len(), 1);
        assert_eq!(trading_state[0].orders[0].state, "rejected");
        assert_eq!(
            trading_state[0].orders[0].rejection_reason.as_deref(),
            Some("market closed")
        );
    }

    #[test]
    fn daemon_surfaces_live_gateway_transport_failure_as_error_without_finalizing_order() {
        let root = temp_dir("live-intent-transport-error");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::failed(
                ploy_connectivity::ExecutionError::Transport("gateway offline".to_string()),
            )),
        )
        .expect("boot");
        let err = daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-transport".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(3),
                limit_price: Some(dec!(0.44)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect_err("transport failure should surface");

        assert_eq!(err.kind(), std::io::ErrorKind::ConnectionAborted);
        let trading_state = daemon.trading_state();
        assert_eq!(trading_state[0].orders.len(), 1);
        assert_eq!(trading_state[0].orders[0].state, "pending");
        assert!(trading_state[0].orders[0].rejection_reason.is_none());
        assert!(trading_state[0].orders[0]
            .last_error
            .as_deref()
            .expect("last_error")
            .contains("gateway offline"));
    }

    #[test]
    fn daemon_cancels_live_order_through_gateway_and_updates_ledger() {
        let root = temp_dir("live-order-cancel");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let gateway = StaticExecutionGateway::acknowledged("venue-live-cancel-1")
            .with_cancel_result(Ok(CancellationOutcome::Canceled));
        let mut daemon =
            PloyDaemon::boot_with_live_execution(&config, Box::new(gateway)).expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-cancel".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                limit_price: Some(dec!(0.55)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        let response = daemon
            .cancel_order("example.live", "order-intent-live-cancel")
            .expect("cancel live order");

        assert_eq!(response.state, "canceled");
        let trading_state = daemon.trading_state();
        assert_eq!(trading_state[0].orders[0].state, "canceled");
        assert_eq!(trading_state[0].risk.pending_intents, 0);
        assert_eq!(trading_state[0].risk.active_orders, 0);
    }

    #[test]
    fn daemon_replaces_live_order_through_gateway_and_preserves_logical_order() {
        let root = temp_dir("live-order-replace");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let gateway = StaticExecutionGateway::acknowledged("venue-live-replace-1")
            .with_replace_result(Ok(ReplaceOutcome::Replaced {
                venue_order_id: "venue-live-replace-2".to_string(),
            }));
        let mut daemon =
            PloyDaemon::boot_with_live_execution(&config, Box::new(gateway)).expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-replace".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                limit_price: Some(dec!(0.55)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        let response = daemon
            .replace_order(
                "example.live",
                "order-intent-live-replace",
                ploy_operator_contracts::OrderReplaceRequest {
                    quantity: dec!(3),
                    limit_price: Some(dec!(0.57)),
                },
            )
            .expect("replace live order");

        assert_eq!(response.state, "acknowledged");
        assert_eq!(response.order_id, "order-intent-live-replace");
        assert_eq!(response.revision, 1);
        assert_eq!(
            response.venue_order_id.as_deref(),
            Some("venue-live-replace-2")
        );
        assert_eq!(
            response.venue_order_history,
            vec!["venue-live-replace-1".to_string()]
        );
        assert_eq!(response.requested_qty, dec!(3));
        assert_eq!(response.limit_price, Some(dec!(0.57)));

        let trading_state = daemon.trading_state();
        assert_eq!(
            trading_state[0].orders[0].order_id,
            "order-intent-live-replace"
        );
        assert_eq!(trading_state[0].orders[0].revision, 1);
        assert_eq!(
            trading_state[0].orders[0].venue_order_history,
            vec!["venue-live-replace-1".to_string()]
        );
        assert_eq!(
            trading_state[0].orders[0].venue_order_id.as_deref(),
            Some("venue-live-replace-2")
        );
    }

    #[test]
    fn daemon_rejects_replace_when_requested_qty_is_below_filled_qty() {
        let root = temp_dir("live-order-replace-invalid");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let fill = FillRecord {
            fill_id: "fill-live-replace-1".to_string(),
            order_id: "order-intent-live-replace-invalid".to_string(),
            token_id: "yes-token".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            price: dec!(0.55),
            fee: dec!(0.01),
            timestamp: chrono::Utc::now(),
        };
        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(
                StaticExecutionGateway::acknowledged("venue-live-replace-invalid-1")
                    .with_reconciled_fills(vec![fill]),
            ),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-replace-invalid".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(3),
                limit_price: Some(dec!(0.55)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");
        assert!(matches!(
            daemon.reconcile_live_fills().expect("reconcile fills"),
            ReconcileStatus::Applied(1)
        ));

        let error = daemon
            .replace_order(
                "example.live",
                "order-intent-live-replace-invalid",
                ploy_operator_contracts::OrderReplaceRequest {
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.57)),
                },
            )
            .expect_err("replace should fail");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error
            .to_string()
            .contains("cannot be below filled quantity"));
    }

    #[test]
    fn daemon_enforces_account_exposure_limits_across_deployments() {
        let root = temp_dir("account-exposure-limit");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        for deployment_id in ["acct-a.paper", "acct-b.paper"] {
            daemon
                .apply_deployment(DeploymentApplyRequest {
                    deployment_id: deployment_id.to_string(),
                    bundle_id: "example".to_string(),
                    runtime_mode: "paper".to_string(),
                    account_id: "acct-shared".to_string(),
                    max_gross_exposure: Some(dec!(5)),
                    deployment_state: DeploymentState::Enabled,
                    desired_state: DesiredState::Running,
                })
                .expect("apply deployment");
        }

        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-account-a".to_string(),
                deployment_id: "acct-a.paper".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(4),
                limit_price: Some(dec!(1)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit first intent");

        let error = daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-account-b".to_string(),
                deployment_id: "acct-b.paper".to_string(),
                market_id: "market-2".to_string(),
                token_id: "no-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                limit_price: Some(dec!(1)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect_err("second intent should exceed shared account exposure");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("acct-shared"));
        assert!(error.to_string().contains("max_gross_exposure"));
    }

    #[test]
    fn daemon_rejects_replacement_when_it_would_exceed_account_exposure_limit() {
        let root = temp_dir("account-exposure-replace");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon
            .apply_deployment(DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: "paper".to_string(),
                account_id: "acct-paper".to_string(),
                max_gross_exposure: Some(dec!(1.5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            })
            .expect("apply deployment");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-paper-replace".to_string(),
                deployment_id: "example.paper".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                limit_price: Some(dec!(0.5)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit intent");

        let error = daemon
            .replace_order(
                "example.paper",
                "order-intent-paper-replace",
                ploy_operator_contracts::OrderReplaceRequest {
                    quantity: dec!(4),
                    limit_price: Some(dec!(0.5)),
                },
            )
            .expect_err("replacement should exceed exposure limit");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("acct-paper"));
        assert!(error.to_string().contains("next_total=2.0"));
    }

    #[test]
    fn daemon_reconciles_live_fill_into_canonical_ledger() {
        let root = temp_dir("live-fill-reconcile");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let fill = FillRecord {
            fill_id: "fill-live-1".to_string(),
            order_id: "order-intent-live-3".to_string(),
            token_id: "yes-token".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(3),
            price: dec!(0.44),
            fee: dec!(0.02),
            timestamp: chrono::Utc::now(),
        };

        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(
                StaticExecutionGateway::acknowledged("venue-live-3")
                    .with_reconciled_fills(vec![fill]),
            ),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-3".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(3),
                limit_price: Some(dec!(0.44)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        let reconciled = daemon.reconcile_live_fills().expect("reconcile fills");
        assert_eq!(reconciled, ReconcileStatus::Applied(1));

        let trading_state = daemon.trading_state();
        assert_eq!(trading_state[0].fills.len(), 1);
        assert_eq!(trading_state[0].orders[0].state, "filled");
        assert_eq!(trading_state[0].positions[0].net_qty, dec!(3));
    }

    #[test]
    fn daemon_restores_live_orders_and_reconciles_after_restart() {
        let root = temp_dir("live-restart-recovery");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("venue-live-restart-1")),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-restart".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(4),
                limit_price: Some(dec!(0.41)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");
        daemon.write_runtime_snapshots().expect("write snapshots");

        let mut restored = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(
                StaticExecutionGateway::acknowledged("unused").with_reconciled_fills(vec![
                    FillRecord {
                        fill_id: "fill-live-restart".to_string(),
                        order_id: "order-intent-live-restart".to_string(),
                        token_id: "yes-token".to_string(),
                        side: TradeSide::Buy,
                        quantity: dec!(4),
                        price: dec!(0.41),
                        fee: dec!(0.04),
                        timestamp: chrono::Utc::now(),
                    },
                ]),
            ),
        )
        .expect("restore daemon");

        let restored_state = restored.trading_state();
        assert_eq!(restored_state[0].orders.len(), 1);
        assert_eq!(restored_state[0].orders[0].state, "acknowledged");
        assert_eq!(
            restored_state[0].orders[0].venue_order_id.as_deref(),
            Some("venue-live-restart-1")
        );

        let recorded = restored
            .reconcile_live_fills()
            .expect("reconcile restored fills");
        assert_eq!(recorded, ReconcileStatus::Applied(1));

        let reconciled_state = restored.trading_state();
        assert_eq!(reconciled_state[0].fills.len(), 1);
        assert_eq!(reconciled_state[0].orders[0].state, "filled");
        assert_eq!(reconciled_state[0].positions.len(), 1);
        assert_eq!(reconciled_state[0].positions[0].net_qty, dec!(4));
    }

    #[test]
    fn daemon_reconcile_is_idempotent_for_duplicate_fill_ids() {
        let root = temp_dir("live-fill-idempotent");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let fill = FillRecord {
            fill_id: "fill-live-dup".to_string(),
            order_id: "order-intent-live-4".to_string(),
            token_id: "yes-token".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.41),
            fee: dec!(0.01),
            timestamp: chrono::Utc::now(),
        };

        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(
                StaticExecutionGateway::acknowledged("venue-live-4")
                    .with_reconciled_fills(vec![fill]),
            ),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-4".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.41)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        assert_eq!(
            daemon.reconcile_live_fills().expect("reconcile fills"),
            ReconcileStatus::Applied(1)
        );
        assert_eq!(
            daemon.reconcile_live_fills().expect("reconcile fills"),
            ReconcileStatus::Noop
        );

        let trading_state = daemon.trading_state();
        assert_eq!(trading_state[0].fills.len(), 1);
        assert_eq!(trading_state[0].orders[0].filled_qty, dec!(1));
    }

    #[test]
    fn daemon_surfaces_transient_reconcile_failures_as_degraded_then_recovering() {
        let root = temp_dir("live-health-recovery");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            live_reconcile_backoff_base_ms: 0,
            live_reconcile_backoff_max_ms: 0,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(FlakyReconcileGateway::default()),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-health".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.41)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        daemon.write_runtime_snapshots().expect("degraded snapshot");
        assert!(daemon
            .control_plane
            .system
            .status()
            .status
            .starts_with("degraded@"));
        assert!(daemon.control_plane.system.status().error_count_1h >= 1);
        assert_eq!(
            daemon
                .inspect_deployment("example.live")
                .expect("deployment")
                .observed_state,
            ObservedState::Degraded
        );

        daemon
            .write_runtime_snapshots()
            .expect("recovering snapshot");
        assert!(daemon
            .control_plane
            .system
            .status()
            .status
            .starts_with("recovering@"));
        assert_eq!(
            daemon
                .inspect_deployment("example.live")
                .expect("deployment")
                .observed_state,
            ObservedState::Running
        );

        daemon.write_runtime_snapshots().expect("running snapshot");
        assert!(daemon
            .control_plane
            .system
            .status()
            .status
            .starts_with("running@"));
    }

    #[test]
    fn live_reconcile_backoff_doubles_until_maximum() {
        assert_eq!(live_reconcile_backoff_ms(1, 1_000, 30_000), 1_000);
        assert_eq!(live_reconcile_backoff_ms(2, 1_000, 30_000), 2_000);
        assert_eq!(live_reconcile_backoff_ms(3, 1_000, 30_000), 4_000);
        assert_eq!(live_reconcile_backoff_ms(10, 1_000, 30_000), 30_000);
    }

    #[test]
    fn daemon_skips_live_reconcile_while_backoff_is_active() {
        let root = temp_dir("live-health-backoff");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            tick_interval_ms: 5,
            live_reconcile_backoff_base_ms: 60_000,
            live_reconcile_backoff_max_ms: 60_000,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(FlakyReconcileGateway::default()),
        )
        .expect("boot");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-backoff".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.41)),
                purpose: IntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit live intent");

        daemon.write_runtime_snapshots().expect("degraded snapshot");
        let status = daemon.control_plane.system.status();
        assert_eq!(status.live_reconcile_failures, 1);
        assert!(status.next_live_reconcile_at.is_some());
        assert_eq!(
            daemon.reconcile_live_fills().expect("backoff reconcile"),
            ReconcileStatus::BackoffActive
        );
    }
}
