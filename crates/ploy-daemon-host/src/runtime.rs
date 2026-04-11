use crate::config::PlatformConfig;
use crate::events::EventBroker;
use crate::http::publish_snapshot_events;
use chrono::{DateTime, Utc};
use ploy_platform_runtime::{
    apply_loaded_registry_state,
    apply_deployment as apply_deployment_record, control_deployment as control_deployment_record,
    LiveHealthConfig, mark_live_runtime_degraded as mark_runtime_degraded_state,
    mark_runtime_healthy as mark_runtime_healthy_state,
    WorkerTickConfig, refresh_source_health as refresh_platform_source_health,
    tick_workers as tick_platform_workers,
    reconcile_live_fills as reconcile_runtime_live_fills,
    cancel_order as cancel_runtime_order, replace_order as replace_runtime_order,
    enforce_exposure_limit as enforce_intent_exposure_limit,
    ensure_intent_allowed, set_deployment_max_gross_exposure as set_record_max_gross_exposure,
    load_proposal_store, load_registry_records, load_trading_runtimes,
    ProposalStore,
    submit_live_intent as submit_live_runtime_intent,
    ReconcileStatus, build_trading_state_snapshot, submit_paper_intent as submit_paper_runtime_intent,
    write_json,
};
use ploy_connectivity::{
    LiveExecutionGateway, PolymarketExecutionGateway,
};
use ploy_deployments::WorkerSupervisor;
use ploy_operator_contracts::{
    ActiveAlert, DeploymentApplyRequest, DeploymentControlRequest, DeploymentState, DesiredState,
    ObservedState, OrderControlResponse, OrderReplaceRequest, PaperIntentResponse,
    PlatformMetrics, ProposalActionKind, ProposalCreateRequest, ProposalDecisionRequest,
    SafetyProposal, TradingStateSnapshot,
};
use ploy_platform::{ControlPlane, DeploymentRecord};
use ploy_trading::{TradingIntent, TradingRuntime};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

pub use ploy_platform_runtime::next_paper_intent_id;

#[allow(dead_code)]
pub fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}

#[cfg(test)]
use ploy_trading::FillRecord;

#[derive(Debug)]
pub struct PloyDaemon {
    pub config: PlatformConfig,
    pub control_plane: ControlPlane,
    pub supervisor: WorkerSupervisor,
    pub trading: BTreeMap<String, TradingRuntime>,
    proposals: ProposalStore,
    live_execution: Box<dyn LiveExecutionGateway>,
    live_reconcile_failures: u32,
    next_live_reconcile_at: Option<DateTime<Utc>>,
    last_live_reconcile_error: Option<String>,
}

impl PloyDaemon {
    pub fn boot(config: &PlatformConfig) -> io::Result<Self> {
        Self::boot_with_live_execution(config, Box::new(PolymarketExecutionGateway::from_env()))
    }

    pub fn boot_with_live_execution(
        config: &PlatformConfig,
        live_execution: Box<dyn LiveExecutionGateway>,
    ) -> io::Result<Self> {
        let mut normalized_config = config.clone();
        normalized_config.normalize_derived_paths();
        let mut control_plane = ControlPlane::default();
        control_plane
            .system
            .set_status(format!("starting@{}", normalized_config.listen_addr));

        let mut daemon = Self {
            config: normalized_config,
            control_plane,
            supervisor: WorkerSupervisor::default(),
            trading: BTreeMap::new(),
            proposals: ProposalStore::default(),
            live_execution,
            live_reconcile_failures: 0,
            next_live_reconcile_at: None,
            last_live_reconcile_error: None,
        };
        daemon.load_registry()?;
        daemon.load_trading_snapshots()?;
        daemon.load_proposals()?;
        if daemon.config.trading_state_file.exists() {
            daemon
                .control_plane
                .system
                .mark_recovering(&daemon.config.listen_addr);
        }
        daemon.tick();
        daemon.mark_runtime_healthy();

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
        match self.reconcile_live_fills() {
            Ok(ReconcileStatus::Applied(_) | ReconcileStatus::Noop) => self.mark_runtime_healthy(),
            Ok(ReconcileStatus::BackoffActive) => {}
            Err(err) => self.mark_live_runtime_degraded(err),
        }
        self.refresh_source_health();
        if self.config.circuit_breaker_enabled {
            self.evaluate_circuit_breakers();
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
        write_json(&self.config.proposals_file, &self.proposals.all())?;
        Ok(())
    }

    pub fn inspect_deployment(&self, deployment_id: &str) -> Option<DeploymentRecord> {
        self.control_plane.deployments.get(deployment_id).cloned()
    }

    pub fn platform_metrics(&self) -> PlatformMetrics {
        let records = self.control_plane.deployments.records();
        let total_deployments = records.len();
        let live_deployments = records
            .iter()
            .filter(|record| record.runtime_mode == "live")
            .count();
        let degraded_deployments = records
            .iter()
            .filter(|record| record.observed_state == ObservedState::Degraded)
            .count();
        self.control_plane
            .system
            .metrics(total_deployments, live_deployments, degraded_deployments)
    }

    pub fn active_alerts(&self) -> Vec<ActiveAlert> {
        self.control_plane.system.active_alerts()
    }

    pub fn proposals(&self) -> Vec<SafetyProposal> {
        self.proposals.all()
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

    pub fn apply_deployment(
        &mut self,
        request: DeploymentApplyRequest,
    ) -> io::Result<DeploymentRecord> {
        let record = apply_deployment_record(&mut self.control_plane.deployments, request);
        self.persist_registry()?;
        self.write_runtime_snapshots()?;
        Ok(record)
    }

    pub fn create_proposal(
        &mut self,
        request: ProposalCreateRequest,
    ) -> io::Result<SafetyProposal> {
        if self
            .control_plane
            .deployments
            .get(&request.target_deployment_id)
            .is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "deployment `{}` was not found",
                    request.target_deployment_id
                ),
            ));
        }

        self.proposals.create(request)
    }

    pub fn approve_proposal(
        &mut self,
        proposal_id: &str,
        request: ProposalDecisionRequest,
    ) -> io::Result<Option<SafetyProposal>> {
        let Some(plan) = self.proposals.prepare_approval(proposal_id, request)? else {
            return Ok(None);
        };

        let action_result = match plan.action_kind {
            ProposalActionKind::PauseDeployment => self.control_deployment(
                &plan.target_deployment_id,
                DeploymentControlRequest {
                    desired_state: Some(DesiredState::Paused),
                    deployment_state: None,
                },
            ),
            ProposalActionKind::DrainDeployment => self.control_deployment(
                &plan.target_deployment_id,
                DeploymentControlRequest {
                    desired_state: None,
                    deployment_state: Some(DeploymentState::Draining),
                },
            ),
            ProposalActionKind::ReduceMaxExposure => {
                let max_gross_exposure = plan.proposed_max_gross_exposure.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "proposal missing proposed_max_gross_exposure",
                    )
                })?;
                self.set_deployment_max_gross_exposure(
                    &plan.target_deployment_id,
                    Some(max_gross_exposure),
                )?;
                Ok(self.inspect_deployment(&plan.target_deployment_id))
            }
        };

        match action_result {
            Ok(_) => Ok(Some(
                self.proposals
                    .mark_approved(proposal_id, plan.decision_note)?,
            )),
            Err(err) => {
                self.proposals.mark_failed(proposal_id, &err)?;
                Err(err)
            }
        }
    }

    pub fn reject_proposal(
        &mut self,
        proposal_id: &str,
        request: ProposalDecisionRequest,
    ) -> io::Result<Option<SafetyProposal>> {
        self.proposals.reject(proposal_id, request)
    }

    pub fn control_deployment(
        &mut self,
        deployment_id: &str,
        request: DeploymentControlRequest,
    ) -> io::Result<Option<DeploymentRecord>> {
        let record =
            control_deployment_record(&mut self.control_plane.deployments, deployment_id, request)?;
        self.persist_registry()?;
        self.write_runtime_snapshots()?;
        Ok(record)
    }

    pub fn submit_intent(&mut self, intent: TradingIntent) -> io::Result<PaperIntentResponse> {
        let deployment = self
            .control_plane
            .deployments
            .get(&intent.deployment_id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "deployment not found"))?;
        ensure_intent_allowed(&deployment, &intent)?;
        enforce_intent_exposure_limit(
            &deployment,
            &intent,
            self.account_total_exposure(&deployment.account_id),
        )?;

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

        let runtime = self
            .trading
            .entry(intent.deployment_id.clone())
            .or_default();
        submit_paper_runtime_intent(runtime, deployment, intent)
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
        cancel_runtime_order(
            runtime,
            self.live_execution.as_ref(),
            &deployment,
            deployment_id,
            order_id,
        )
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
        let current_total_exposure = self.account_total_exposure(&deployment.account_id);
        let runtime = self.trading.get_mut(deployment_id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "deployment has no trading state")
        })?;
        replace_runtime_order(
            runtime,
            self.live_execution.as_ref(),
            &deployment,
            deployment_id,
            order_id,
            request,
            current_total_exposure,
        )
    }

    fn submit_live_intent(&mut self, intent: TradingIntent) -> io::Result<PaperIntentResponse> {
        let runtime = self
            .trading
            .entry(intent.deployment_id.clone())
            .or_default();
        submit_live_runtime_intent(runtime, self.live_execution.as_ref(), intent)
    }

    pub fn reconcile_live_fills(&mut self) -> io::Result<ReconcileStatus> {
        if let Some(next_attempt_at) = self.next_live_reconcile_at {
            if Utc::now() < next_attempt_at {
                return Ok(ReconcileStatus::BackoffActive);
            }
        }

        let result = reconcile_runtime_live_fills(
            self.live_execution.as_ref(),
            &self.control_plane.deployments.records(),
            &mut self.trading,
        )?;

        if matches!(result, ReconcileStatus::Noop) {
            self.live_reconcile_failures = 0;
            self.next_live_reconcile_at = None;
            self.last_live_reconcile_error = None;
            self.control_plane.system.note_live_reconcile_healthy();
            return Ok(ReconcileStatus::Noop);
        }

        self.live_reconcile_failures = 0;
        self.next_live_reconcile_at = None;
        self.last_live_reconcile_error = None;
        self.control_plane.system.note_live_reconcile_healthy();

        Ok(result)
    }

    fn latest_trade_time(&self) -> Option<DateTime<Utc>> {
        self.trading
            .values()
            .filter_map(TradingRuntime::last_fill_time)
            .max()
    }

    fn mark_runtime_healthy(&mut self) {
        let health_config = self.live_health_config();
        let latest_trade_time = self.latest_trade_time();
        mark_runtime_healthy_state(
            &mut self.control_plane,
            &health_config,
            latest_trade_time,
        );
    }

    fn mark_live_runtime_degraded(&mut self, err: io::Error) {
        let health_config = self.live_health_config();
        mark_runtime_degraded_state(
            &mut self.control_plane,
            &health_config,
            &mut self.live_reconcile_failures,
            &mut self.next_live_reconcile_at,
            &mut self.last_live_reconcile_error,
            &err,
        );
    }

    fn evaluate_circuit_breakers(&mut self) {
        // Placeholder until circuit-breaker policy is fully reintroduced on the
        // new platform-runtime path. Keeping this as a no-op preserves the
        // current compile contract without inventing behavior.
    }

    fn live_health_config(&self) -> LiveHealthConfig {
        LiveHealthConfig {
            listen_addr: self.config.listen_addr.clone(),
            live_reconcile_stale_after_ms: self.config.live_reconcile_stale_after_ms,
            venue_stale_after_ms: self.config.venue_stale_after_ms,
            live_reconcile_backoff_base_ms: self.config.live_reconcile_backoff_base_ms,
            live_reconcile_backoff_max_ms: self.config.live_reconcile_backoff_max_ms,
        }
    }

    #[cfg(test)]
    pub fn record_fill(&mut self, deployment_id: &str, fill: FillRecord) {
        if let Some(runtime) = self.trading.get_mut(deployment_id) {
            runtime.record_fill(fill);
        }
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
        let records = load_registry_records(&self.config.registry_file)?;
        apply_loaded_registry_state(
            records,
            &mut self.control_plane.deployments,
            &mut self.supervisor,
            &mut self.trading,
        );

        Ok(())
    }

    fn load_trading_snapshots(&mut self) -> io::Result<()> {
        let runtimes = load_trading_runtimes(&self.config.trading_state_file, |deployment_id| {
            self.control_plane.deployments.get(deployment_id).is_some()
        })?;
        self.trading.extend(runtimes);
        Ok(())
    }

    fn load_proposals(&mut self) -> io::Result<()> {
        self.proposals = load_proposal_store(&self.config.proposals_file)?;
        Ok(())
    }

    fn tick(&mut self) {
        let tick_config = WorkerTickConfig {
            listen_addr: self.config.listen_addr.clone(),
            worker_heartbeat_stale_after_ms: self.config.worker_heartbeat_stale_after_ms,
        };
        tick_platform_workers(&mut self.control_plane, &mut self.supervisor, &tick_config);
    }

    fn refresh_source_health(&mut self) {
        refresh_platform_source_health(&mut self.control_plane, &self.config.listen_addr);
    }

    fn persist_registry(&self) -> io::Result<()> {
        write_json(
            &self.config.registry_file,
            &self.control_plane.deployments.records(),
        )
    }

    fn set_deployment_max_gross_exposure(
        &mut self,
        deployment_id: &str,
        max_gross_exposure: Option<Decimal>,
    ) -> io::Result<()> {
        // Validate against current exposure BEFORE mutating the registry so that
        // a failed check never leaves a partially-applied limit in memory.
        if let Some(limit) = max_gross_exposure {
            if let Some(runtime) = self.trading.get(deployment_id) {
                let current = runtime.snapshot(&BTreeMap::new()).risk.total_gross_exposure;
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
        }

        let record = set_record_max_gross_exposure(
            &mut self.control_plane.deployments,
            deployment_id,
            max_gross_exposure,
            self.trading
                .get(deployment_id)
                .map(|runtime| runtime.snapshot(&BTreeMap::new()).risk.total_gross_exposure),
        )?;

        self.persist_registry()?;

        if self.trading.contains_key(&record.deployment_id) {
            let _ = self.enforce_current_exposure_limit(&record);
        }
        Ok(())
    }

    fn enforce_current_exposure_limit(&self, record: &DeploymentRecord) -> io::Result<()> {
        let Some(limit) = record.max_gross_exposure else {
            return Ok(());
        };
        let Some(runtime) = self.trading.get(&record.deployment_id) else {
            return Ok(());
        };
        let current = runtime.snapshot(&BTreeMap::new()).risk.total_gross_exposure;
        if current > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "current exposure {} exceeds proposed limit {} for `{}`",
                    current, limit, record.deployment_id
                ),
            ));
        }
        Ok(())
    }
}

pub fn run_shared_forever(
    daemon: Arc<Mutex<PloyDaemon>>,
    events: Arc<EventBroker>,
) -> io::Result<()> {
    loop {
        if shutdown_requested() {
            eprintln!("ployd: shutdown signal received, writing final snapshots");
            let mut daemon = daemon
                .lock()
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "daemon lock poisoned"))?;
            if let Err(err) = daemon.write_runtime_snapshots() {
                eprintln!("ployd: final snapshot write failed: {err}");
            }
            eprintln!("ployd: shutdown complete");
            return Ok(());
        }
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

#[cfg(test)]
mod tests {
    use super::{PloyDaemon, ReconcileStatus};
    use crate::config::PlatformConfig;
    use ploy_connectivity::{
        CancellationOutcome, CancellationRequest, ExecutionError, ExecutionOutcome,
        ExecutionRequest, LiveExecutionGateway, ReplaceOutcome, ReplaceRequest,
        StaticExecutionGateway,
    };
    use ploy_platform_runtime::live_reconcile_backoff_ms;
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
        assert_eq!(daemon.control_plane.system.status().error_count_1h, 1);
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

    #[test]
    fn daemon_surfaces_live_source_failures_in_metrics_and_alerts() {
        let root = temp_dir("live-source-alerts");
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
                intent_id: "intent-live-metrics".to_string(),
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

        let metrics = daemon.platform_metrics();
        assert_eq!(metrics.stale_sources, 2);
        assert_eq!(metrics.active_alerts, 2);
        assert_eq!(metrics.live_reconcile_failures, 1);
        assert!(metrics
            .heartbeats
            .iter()
            .any(|status| status.source_id == "live_reconcile"));
        assert!(metrics
            .heartbeats
            .iter()
            .any(|status| status.source_id == "venue:polymarket"));

        let alerts = daemon.active_alerts();
        assert_eq!(alerts.len(), 2);
        assert!(alerts
            .iter()
            .any(|alert| alert.source_id == "live_reconcile"));
        assert!(alerts
            .iter()
            .any(|alert| alert.source_id == "venue:polymarket"));
        assert!(daemon
            .control_plane
            .system
            .status()
            .status
            .starts_with("degraded@"));
    }
}
