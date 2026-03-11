use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::agent_runtime::AgentRiskParams;
use crate::control_plane::StrategyDeployment;
use crate::coordinator::OrderIntent;
use crate::domain::Domain;
use crate::error::Result;

use super::super::command::{CoordinatorCommand, CoordinatorControlCommand};
use super::super::governance::IngressMode;
use super::{AgentCommandChannel, Coordinator, CoordinatorHandle};

impl CoordinatorHandle {
    /// Submit an order intent to the coordinator for risk checking and execution
    pub async fn submit_order(&self, intent: OrderIntent) -> Result<()> {
        self.validate_submit_order_intent(&intent).await?;
        self.order_tx.send(intent).await.map_err(|_| {
            crate::error::PloyError::Internal("coordinator order channel closed".into())
        })
    }

    /// Report agent state (heartbeat + position/PnL snapshot)
    pub async fn update_agent_state(
        &self,
        snapshot: super::super::state::AgentSnapshot,
    ) -> Result<()> {
        self.state_tx.send(snapshot).await.map_err(|_| {
            crate::error::PloyError::Internal("coordinator state channel closed".into())
        })
    }

    /// Pause all agents
    pub async fn pause_all(&self) -> Result<()> {
        self.governance.set_global_mode(IngressMode::Paused).await;
        self.persist_governance_state_if_configured().await?;
        self.control_tx
            .send(CoordinatorControlCommand::PauseAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Resume all agents
    pub async fn resume_all(&self) -> Result<()> {
        self.governance.set_global_mode(IngressMode::Running).await;
        self.persist_governance_state_if_configured().await?;
        self.control_tx
            .send(CoordinatorControlCommand::ResumeAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Force-close all positions and stop agents
    pub async fn force_close_all(&self) -> Result<()> {
        self.governance.set_global_mode(IngressMode::Halted).await;
        self.persist_governance_state_if_configured().await?;
        self.control_tx
            .send(CoordinatorControlCommand::ForceCloseAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Shutdown all agents gracefully
    pub async fn shutdown_all(&self) -> Result<()> {
        self.governance.set_global_mode(IngressMode::Halted).await;
        self.persist_governance_state_if_configured().await?;
        self.control_tx
            .send(CoordinatorControlCommand::ShutdownAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Pause a specific domain
    pub async fn pause_domain(&self, domain: Domain) -> Result<()> {
        self.governance
            .set_domain_mode(domain, IngressMode::Paused)
            .await;
        self.persist_governance_state_if_configured().await?;
        self.control_tx
            .send(CoordinatorControlCommand::PauseDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Resume a specific domain
    pub async fn resume_domain(&self, domain: Domain) -> Result<()> {
        self.governance
            .set_domain_mode(domain, IngressMode::Running)
            .await;
        self.persist_governance_state_if_configured().await?;
        self.control_tx
            .send(CoordinatorControlCommand::ResumeDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Force-close positions for one domain
    pub async fn force_close_domain(&self, domain: Domain) -> Result<()> {
        self.governance
            .set_domain_mode(domain, IngressMode::Halted)
            .await;
        self.persist_governance_state_if_configured().await?;
        self.control_tx
            .send(CoordinatorControlCommand::ForceCloseDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Shutdown one domain
    pub async fn shutdown_domain(&self, domain: Domain) -> Result<()> {
        self.governance
            .set_domain_mode(domain, IngressMode::Halted)
            .await;
        self.persist_governance_state_if_configured().await?;
        self.control_tx
            .send(CoordinatorControlCommand::ShutdownDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Pause a single agent by ID (used by OpenClaw meta-agent)
    pub async fn pause_agent(&self, agent_id: &str) -> Result<()> {
        self.governance.pause_agent(agent_id).await;
        self.persist_governance_state_if_configured().await?;
        self.control_tx
            .send(CoordinatorControlCommand::PauseAgent(agent_id.to_string()))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Resume a single agent by ID (used by OpenClaw meta-agent)
    pub async fn resume_agent(&self, agent_id: &str) -> Result<()> {
        self.governance.resume_agent(agent_id).await;
        self.persist_governance_state_if_configured().await?;
        self.control_tx
            .send(CoordinatorControlCommand::ResumeAgent(agent_id.to_string()))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Shared deployment registry (single source of truth for API + coordinator).
    pub fn shared_deployments(&self) -> Arc<RwLock<HashMap<String, StrategyDeployment>>> {
        self.admission.shared_deployments()
    }

    /// Runtime-enabled domains for this coordinator process.
    pub fn allowed_domains(&self) -> Arc<HashSet<Domain>> {
        self.allowed_domains.clone()
    }

    /// Whether a domain is enabled in this runtime.
    pub fn is_domain_allowed(&self, domain: Domain) -> bool {
        self.allowed_domains.contains(&domain)
    }

    /// Whether an agent_id is registered/authorized for order ingress.
    pub async fn is_agent_authorized(&self, agent_id: &str) -> bool {
        self.authorized_agents.read().await.contains(agent_id)
    }
}

impl Coordinator {
    pub async fn authorize_external_agent(&self, agent_id: &str, params: AgentRiskParams) {
        let id = agent_id.trim();
        if id.is_empty() {
            return;
        }
        self.authorized_agents.write().await.insert(id.to_string());
        self.risk_gate.register_agent(id, params).await;
        info!(agent_id = %id, "external ingress agent authorized");
    }

    /// Send a command to a specific agent
    pub async fn send_command(&self, agent_id: &str, cmd: CoordinatorCommand) -> Result<()> {
        if let Some(tx) = self.agent_commands.get(agent_id) {
            tx.tx.send(cmd).await.map_err(|_| {
                crate::error::PloyError::Internal(format!(
                    "agent {} command channel closed",
                    agent_id
                ))
            })
        } else {
            Err(crate::error::PloyError::Internal(format!(
                "agent {} not registered",
                agent_id
            )))
        }
    }

    fn should_apply_domain_cmd(&self, entry: &AgentCommandChannel, target: Domain) -> bool {
        entry.domain == target
    }

    async fn cancel_queued_buy_intents(&self, domain: Option<Domain>, reason: &str) {
        let dropped = {
            let mut queue = self.order_queue.write().await;
            queue.remove_buy_orders(domain)
        };

        if dropped.is_empty() {
            return;
        }

        for intent in dropped {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.to_string()), None)
                .await;
            self.settle_domain_failure(&intent).await;
        }
    }

    /// Pause all agents
    pub async fn pause_all(&self) {
        self.governance.set_global_mode(IngressMode::Paused).await;
        if let Err(error) = self.persist_governance_state_if_configured().await {
            warn!(error = %error, "failed to persist governance state after global pause");
        }
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::Pause).await {
                warn!(agent_id = %id, error = %e, "failed to send pause");
            }
        }
    }

    /// Resume all agents
    pub async fn resume_all(&self) {
        self.governance.set_global_mode(IngressMode::Running).await;
        if let Err(error) = self.persist_governance_state_if_configured().await {
            warn!(error = %error, "failed to persist governance state after global resume");
        }
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::Resume).await {
                warn!(agent_id = %id, error = %e, "failed to send resume");
            }
        }
    }

    /// Force-close all agents (best-effort)
    pub async fn force_close_all(&self) {
        self.governance.set_global_mode(IngressMode::Halted).await;
        if let Err(error) = self.persist_governance_state_if_configured().await {
            warn!(error = %error, "failed to persist governance state after global halt");
        }
        self.cancel_queued_buy_intents(None, "dropped by coordinator global halt")
            .await;
        info!("coordinator: sending force-close to all agents");
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::ForceClose).await {
                warn!(agent_id = %id, error = %e, "failed to send force-close");
            }
        }
    }

    /// Shutdown all agents gracefully
    pub async fn shutdown(&self) {
        self.governance.set_global_mode(IngressMode::Halted).await;
        if let Err(error) = self.persist_governance_state_if_configured().await {
            warn!(error = %error, "failed to persist governance state after shutdown");
        }
        self.cancel_queued_buy_intents(None, "dropped by coordinator shutdown")
            .await;
        info!("coordinator: sending shutdown to all agents");
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::Shutdown).await {
                warn!(agent_id = %id, error = %e, "failed to send shutdown");
            }
        }
    }

    /// Pause one domain
    pub async fn pause_domain(&self, domain: Domain) {
        self.governance
            .set_domain_mode(domain, IngressMode::Paused)
            .await;
        if let Err(error) = self.persist_governance_state_if_configured().await {
            warn!(error = %error, domain = ?domain, "failed to persist governance state after domain pause");
        }
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::Pause).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain pause");
                }
            }
        }
    }

    /// Resume one domain
    pub async fn resume_domain(&self, domain: Domain) {
        self.governance
            .set_domain_mode(domain, IngressMode::Running)
            .await;
        if let Err(error) = self.persist_governance_state_if_configured().await {
            warn!(error = %error, domain = ?domain, "failed to persist governance state after domain resume");
        }
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::Resume).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain resume");
                }
            }
        }
    }

    /// Force-close all agents in one domain
    pub async fn force_close_domain(&self, domain: Domain) {
        self.governance
            .set_domain_mode(domain, IngressMode::Halted)
            .await;
        if let Err(error) = self.persist_governance_state_if_configured().await {
            warn!(error = %error, domain = ?domain, "failed to persist governance state after domain halt");
        }
        self.cancel_queued_buy_intents(Some(domain), "dropped by coordinator domain halt")
            .await;
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::ForceClose).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain force-close");
                }
            }
        }
    }

    /// Shutdown all agents in one domain
    pub async fn shutdown_domain(&self, domain: Domain) {
        self.governance
            .set_domain_mode(domain, IngressMode::Halted)
            .await;
        if let Err(error) = self.persist_governance_state_if_configured().await {
            warn!(error = %error, domain = ?domain, "failed to persist governance state after domain shutdown");
        }
        self.cancel_queued_buy_intents(Some(domain), "dropped by coordinator domain shutdown")
            .await;
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::Shutdown).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain shutdown");
                }
            }
        }
    }

    pub(super) async fn handle_control_command(&self, cmd: CoordinatorControlCommand) {
        match cmd {
            CoordinatorControlCommand::PauseAll => self.pause_all().await,
            CoordinatorControlCommand::ResumeAll => self.resume_all().await,
            CoordinatorControlCommand::ForceCloseAll => self.force_close_all().await,
            CoordinatorControlCommand::ShutdownAll => self.shutdown().await,
            CoordinatorControlCommand::PauseDomain(domain) => self.pause_domain(domain).await,
            CoordinatorControlCommand::ResumeDomain(domain) => self.resume_domain(domain).await,
            CoordinatorControlCommand::ForceCloseDomain(domain) => {
                self.force_close_domain(domain).await
            }
            CoordinatorControlCommand::ShutdownDomain(domain) => self.shutdown_domain(domain).await,
            CoordinatorControlCommand::PauseAgent(id) => {
                self.governance.pause_agent(&id).await;
                if let Err(error) = self.persist_governance_state_if_configured().await {
                    warn!(error = %error, agent_id = %id, "failed to persist governance state after agent pause");
                }
                self.send_command(&id, CoordinatorCommand::Pause).await.ok();
            }
            CoordinatorControlCommand::ResumeAgent(id) => {
                self.governance.resume_agent(&id).await;
                if let Err(error) = self.persist_governance_state_if_configured().await {
                    warn!(error = %error, agent_id = %id, "failed to persist governance state after agent resume");
                }
                self.send_command(&id, CoordinatorCommand::Resume)
                    .await
                    .ok();
            }
        }
    }
}
