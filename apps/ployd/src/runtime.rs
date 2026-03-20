use crate::config::PlatformConfig;
use ploy_deployments::{WorkerLaunchSpec, WorkerSupervisor};
use ploy_operator_contracts::{
    DeploymentApplyRequest, DesiredState, FillSnapshot, IntentPurpose, ObservedState,
    OrderSnapshot, PaperIntentResponse, PnlSnapshotResponse, PositionSnapshotResponse,
    RiskSnapshotResponse, TradingIntentSnapshot, TradingStateSnapshot,
};
use ploy_platform::{ControlPlane, DeploymentRecord};
use ploy_trading::{
    FillRecord, OrderState, TradeSide, TradingIntent, TradingRuntime, TradingRuntimeSnapshot,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct PloyDaemon {
    pub config: PlatformConfig,
    pub control_plane: ControlPlane,
    pub supervisor: WorkerSupervisor,
    pub trading: BTreeMap<String, TradingRuntime>,
}

impl PloyDaemon {
    pub fn boot(config: &PlatformConfig) -> io::Result<Self> {
        let mut control_plane = ControlPlane::default();
        control_plane
            .system
            .set_status(format!("running@{}", config.listen_addr));

        let mut daemon = Self {
            config: config.clone(),
            control_plane,
            supervisor: WorkerSupervisor::default(),
            trading: BTreeMap::new(),
        };
        daemon.load_registry()?;
        daemon.tick();

        Ok(daemon)
    }

    pub fn write_runtime_snapshots(&mut self) -> io::Result<()> {
        self.load_registry()?;
        self.tick();
        self.persist_registry()?;
        fs::create_dir_all(&self.config.runtime_root)?;
        write_json(
            &self.config.status_file,
            &self.control_plane.system.status(),
        )?;
        write_json(
            &self.config.deployment_status_file,
            &self.control_plane.deployments.summaries(),
        )?;
        write_json(&self.config.trading_state_file, &self.trading_state())?;
        Ok(())
    }

    pub fn run_forever(&mut self) -> io::Result<()> {
        loop {
            self.write_runtime_snapshots()?;
            thread::sleep(Duration::from_millis(self.config.tick_interval_ms));
        }
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

    pub fn apply_deployment(
        &mut self,
        request: DeploymentApplyRequest,
    ) -> io::Result<DeploymentRecord> {
        let record = DeploymentRecord {
            deployment_id: request.deployment_id,
            bundle_id: request.bundle_id,
            runtime_mode: request.runtime_mode,
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

    pub fn set_desired_state(
        &mut self,
        deployment_id: &str,
        desired_state: DesiredState,
    ) -> io::Result<Option<DeploymentRecord>> {
        let Some(record) = self.control_plane.deployments.get(deployment_id).cloned() else {
            return Ok(None);
        };

        self.control_plane
            .deployments
            .set_desired_state(deployment_id, desired_state);
        self.control_plane
            .deployments
            .set_observed_state(deployment_id, observed_state_for_desired(desired_state));
        self.persist_registry()?;
        self.write_runtime_snapshots()?;
        Ok(self
            .control_plane
            .deployments
            .get(&record.deployment_id)
            .cloned())
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
        runtime.acknowledge_order(&order_id, venue_order_id);
        Ok(PaperIntentResponse {
            deployment_id,
            intent_id,
            order_id,
            state: "acknowledged".to_string(),
        })
    }

    pub fn record_fill(&mut self, deployment_id: &str, fill: FillRecord) {
        if let Some(runtime) = self.trading.get_mut(deployment_id) {
            runtime.record_fill(fill);
        }
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
                state: order_state_wire(order.state),
                filled_qty: order.filled_qty,
                rejection_reason: order.rejection_reason,
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
        },
    }
}

fn trade_side_wire(side: TradeSide) -> String {
    match side {
        TradeSide::Buy => "buy".to_string(),
        TradeSide::Sell => "sell".to_string(),
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

fn intent_purpose_wire(purpose: ploy_trading::IntentPurpose) -> IntentPurpose {
    match purpose {
        ploy_trading::IntentPurpose::Entry => IntentPurpose::Entry,
        ploy_trading::IntentPurpose::Exit => IntentPurpose::Exit,
        ploy_trading::IntentPurpose::Reduce => IntentPurpose::Reduce,
        ploy_trading::IntentPurpose::Hedge => IntentPurpose::Hedge,
        ploy_trading::IntentPurpose::Cancel => IntentPurpose::Cancel,
    }
}

fn observed_state_for_desired(desired_state: DesiredState) -> ObservedState {
    match desired_state {
        DesiredState::Running => ObservedState::Starting,
        DesiredState::Paused => ObservedState::Paused,
        DesiredState::Stopped => ObservedState::Stopped,
    }
}

pub fn next_paper_intent_id(deployment_id: &str) -> String {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_millis();
    format!("{deployment_id}-{unique}")
}

pub fn run_shared_forever(daemon: Arc<Mutex<PloyDaemon>>) -> io::Result<()> {
    loop {
        let tick_interval_ms = {
            let mut daemon = daemon
                .lock()
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "daemon lock poisoned"))?;
            daemon.write_runtime_snapshots()?;
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
    use super::PloyDaemon;
    use crate::config::PlatformConfig;
    use ploy_operator_contracts::{DesiredState, ObservedState};
    use ploy_trading::{FillRecord, IntentPurpose, TradeSide, TradingIntent};
    use rust_decimal_macros::dec;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployd-{label}-{unique}"))
    }

    #[test]
    fn daemon_loads_platform_config() {
        let config = PlatformConfig {
            listen_addr: "127.0.0.1:9090".to_string(),
            ..PlatformConfig::default()
        };

        let daemon = PloyDaemon::boot(&config).expect("boot");
        let status = daemon.control_plane.system.status();
        assert!(status.status.contains("127.0.0.1:9090"));
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
        assert!(status.status.contains("127.0.0.1:8081"));

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
}
