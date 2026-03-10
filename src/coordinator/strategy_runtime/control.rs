use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{info, warn};

use crate::agent_runtime::AgentStatus;
use crate::coordinator::{AgentHealthResponse, AgentSnapshot, CoordinatorCommand};
use crate::domain::Domain;
use crate::strategy::StrategyManager;

pub(super) async fn drive_managed_runtime_control_loop(
    strategy_label: &str,
    agent_id: &str,
    domain: Domain,
    strategy_id: &str,
    started_at: DateTime<Utc>,
    manager: Arc<StrategyManager>,
    paused: Arc<AtomicBool>,
    orders_submitted: Arc<AtomicU64>,
    orders_filled: Arc<AtomicU64>,
    status: &mut AgentStatus,
    cmd_rx: &mut mpsc::Receiver<CoordinatorCommand>,
    shutdown_rx: &mut broadcast::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                info!(
                    strategy = strategy_label,
                    agent_id = agent_id,
                    strategy_id = %strategy_id,
                    "managed strategy runtime shutdown requested"
                );
                break;
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(command) => {
                        if handle_control_command(
                            strategy_label,
                            agent_id,
                            domain,
                            strategy_id,
                            started_at,
                            manager.clone(),
                            paused.clone(),
                            orders_submitted.clone(),
                            orders_filled.clone(),
                            status,
                            command,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    None => {
                        warn!(
                            strategy = strategy_label,
                            agent_id = agent_id,
                            strategy_id = %strategy_id,
                            "managed strategy runtime command channel closed"
                        );
                        break;
                    }
                }
            }
        }
    }
}

async fn handle_control_command(
    strategy_label: &str,
    agent_id: &str,
    domain: Domain,
    strategy_id: &str,
    started_at: DateTime<Utc>,
    manager: Arc<StrategyManager>,
    paused: Arc<AtomicBool>,
    orders_submitted: Arc<AtomicU64>,
    orders_filled: Arc<AtomicU64>,
    status: &mut AgentStatus,
    command: CoordinatorCommand,
) -> bool {
    match command {
        CoordinatorCommand::Pause => {
            paused.store(true, Ordering::Relaxed);
            *status = AgentStatus::Paused;
            info!(
                strategy = strategy_label,
                agent_id = agent_id,
                strategy_id = %strategy_id,
                "managed strategy runtime paused"
            );
            false
        }
        CoordinatorCommand::Resume => {
            paused.store(false, Ordering::Relaxed);
            *status = AgentStatus::Running;
            info!(
                strategy = strategy_label,
                agent_id = agent_id,
                strategy_id = %strategy_id,
                "managed strategy runtime resumed"
            );
            false
        }
        CoordinatorCommand::ForceClose => {
            warn!(
                strategy = strategy_label,
                agent_id = agent_id,
                strategy_id = %strategy_id,
                "managed strategy runtime force-close requested"
            );
            true
        }
        CoordinatorCommand::Shutdown => {
            info!(
                strategy = strategy_label,
                agent_id = agent_id,
                strategy_id = %strategy_id,
                "managed strategy runtime shutdown command received"
            );
            true
        }
        CoordinatorCommand::HealthCheck(tx) => {
            respond_health_check(
                agent_id,
                strategy_label,
                domain,
                strategy_id,
                started_at,
                manager,
                *status,
                orders_submitted,
                orders_filled,
                tx,
            )
            .await;
            false
        }
    }
}

async fn respond_health_check(
    agent_id: &str,
    strategy_label: &str,
    domain: Domain,
    strategy_id: &str,
    started_at: DateTime<Utc>,
    manager: Arc<StrategyManager>,
    status: AgentStatus,
    orders_submitted: Arc<AtomicU64>,
    orders_filled: Arc<AtomicU64>,
    tx: oneshot::Sender<AgentHealthResponse>,
) {
    let position_count = manager
        .get_strategy_status(strategy_id)
        .await
        .map(|strategy_status| strategy_status.position_count)
        .unwrap_or(0);
    let snapshot = AgentSnapshot {
        agent_id: agent_id.to_string(),
        name: strategy_label.to_string(),
        domain,
        status,
        position_count,
        exposure: Decimal::ZERO,
        daily_pnl: Decimal::ZERO,
        unrealized_pnl: Decimal::ZERO,
        metrics: HashMap::new(),
        last_heartbeat: Utc::now(),
        error_message: None,
    };
    let uptime_secs = (Utc::now() - started_at).num_seconds().max(0) as u64;
    let _ = tx.send(AgentHealthResponse {
        snapshot,
        is_healthy: matches!(status, AgentStatus::Running | AgentStatus::Paused),
        uptime_secs,
        orders_submitted: orders_submitted.load(Ordering::Relaxed),
        orders_filled: orders_filled.load(Ordering::Relaxed),
    });
}
