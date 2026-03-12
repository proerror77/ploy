//! Coordinator — central orchestrator for multi-agent trading
//!
//! The Coordinator owns the order queue, risk gate, and position aggregator.
//! Agents communicate with it via `CoordinatorHandle` (clone-friendly).
//! The main `run()` loop uses `tokio::select!` to:
//!   - Process incoming order intents (risk check → enqueue)
//!   - Process agent state updates (heartbeats)
//!   - Periodically drain the queue and execute orders
//!   - Periodically refresh GlobalState from aggregators

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use sqlx::PgPool;

use crate::domain::{OrderRequest, Side};
use crate::error::Result;
use crate::platform::{
    AgentRiskParams, DeploymentState, Domain, MarketSelector, OrderIntent, OrderPriority,
    StrategyDeployment,
};
use crate::strategy::execution::executor::OrderExecutor;

use super::admission::{
    buy_intent_missing_deployment_reason, sell_reduce_only_violation_reason, AdmissionController,
    IntentDuplicateGuard,
};
use super::capital::CapitalPolicy;
use super::command::{
    CoordinatorCommand, CoordinatorControlCommand, DomainIngressSnapshot, GovernanceAgentSnapshot,
    GovernancePolicyHistoryEntry, GovernancePolicySnapshot, GovernancePolicyUpdate,
    GovernanceStatusSnapshot,
};
use super::capital::CryptoHorizon;
use super::config::{CoordinatorConfig, DuplicateGuardScope};
use super::governance::{governance_block_reason, GovernanceController, IngressMode};
use super::journal::ExecutionJournal;
use super::position::PositionAggregator;
use super::queue::OrderQueue;
use super::risk::{RiskCheckResult, RiskGate};
use super::state::{AgentSnapshot, GlobalState, QueueStatsSnapshot};

mod control_surface;
mod execution;
mod ingress;
mod order_updates;
mod recovery;
mod runtime_status;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone)]
struct AgentCommandChannel {
    domain: Domain,
    tx: mpsc::Sender<CoordinatorCommand>,
}

#[derive(Debug, Clone)]
struct GovernancePolicy {
    block_new_intents: bool,
    blocked_domains: HashSet<Domain>,
    max_intent_notional_usd: Option<Decimal>,
    max_total_notional_usd: Option<Decimal>,
    updated_at: chrono::DateTime<Utc>,
    updated_by: String,
    reason: Option<String>,
    /// Extensible key-value metadata for cross-agent signaling (e.g., OpenClaw regime)
    metadata: HashMap<String, String>,
}

impl GovernancePolicy {
    fn from_config(config: &CoordinatorConfig) -> Self {
        let blocked_domains = config
            .governance_blocked_domains
            .iter()
            .filter_map(|raw| parse_governance_domain(raw))
            .collect::<HashSet<_>>();

        Self {
            block_new_intents: config.governance_block_new_intents,
            blocked_domains,
            max_intent_notional_usd: config.governance_max_intent_notional_usd,
            max_total_notional_usd: config.governance_max_total_notional_usd,
            updated_at: Utc::now(),
            updated_by: "boot".to_string(),
            reason: Some("loaded from coordinator config".to_string()),
            metadata: HashMap::new(),
        }
    }

    fn try_from_update(update: GovernancePolicyUpdate) -> std::result::Result<Self, String> {
        let mut blocked_domains = HashSet::new();
        for raw in &update.blocked_domains {
            let Some(domain) = parse_governance_domain(raw) else {
                return Err(format!("unknown blocked domain '{}'", raw));
            };
            blocked_domains.insert(domain);
        }

        if update.updated_by.trim().is_empty() {
            return Err("updated_by is required".to_string());
        }

        if let Some(v) = update.max_intent_notional_usd {
            if v <= Decimal::ZERO {
                return Err("max_intent_notional_usd must be > 0".to_string());
            }
        }
        if let Some(v) = update.max_total_notional_usd {
            if v <= Decimal::ZERO {
                return Err("max_total_notional_usd must be > 0".to_string());
            }
        }

        Ok(Self {
            block_new_intents: update.block_new_intents,
            blocked_domains,
            max_intent_notional_usd: update.max_intent_notional_usd,
            max_total_notional_usd: update.max_total_notional_usd,
            updated_at: Utc::now(),
            updated_by: update.updated_by.trim().to_string(),
            reason: update.reason.and_then(|v| {
                let trimmed = v.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }),
            metadata: update.metadata,
        })
    }

    fn to_snapshot(&self) -> GovernancePolicySnapshot {
        let mut blocked_domains = self
            .blocked_domains
            .iter()
            .map(|d| governance_domain_label(*d).to_string())
            .collect::<Vec<_>>();
        blocked_domains.sort();
        GovernancePolicySnapshot {
            block_new_intents: self.block_new_intents,
            blocked_domains,
            max_intent_notional_usd: self.max_intent_notional_usd,
            max_total_notional_usd: self.max_total_notional_usd,
            updated_at: self.updated_at,
            updated_by: self.updated_by.clone(),
            reason: self.reason.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

fn parse_governance_domain(raw: &str) -> Option<Domain> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "sports" => Some(Domain::Sports),
        "crypto" => Some(Domain::Crypto),
        "politics" => Some(Domain::Politics),
        "economics" => Some(Domain::Economics),
        _ => None,
    }
}

fn governance_domain_label(domain: Domain) -> &'static str {
    match domain {
        Domain::Sports => "sports",
        Domain::Crypto => "crypto",
        Domain::Politics => "politics",
        Domain::Economics => "economics",
        Domain::Custom(_) => "custom",
    }
}

fn governance_domain_snapshot_label(domain: Domain) -> String {
    match domain {
        Domain::Custom(id) => format!("custom:{}", id),
        _ => governance_domain_label(domain).to_string(),
    }
}

fn governance_policy_blocked_domains_sorted(policy: &GovernancePolicy) -> Vec<String> {
    let mut blocked_domains = policy
        .blocked_domains
        .iter()
        .map(|d| governance_domain_label(*d).to_string())
        .collect::<Vec<_>>();
    blocked_domains.sort();
    blocked_domains
}

fn parse_persisted_domain(raw: &str) -> Option<Domain> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    match normalized.as_str() {
        "sports" => Some(Domain::Sports),
        "crypto" => Some(Domain::Crypto),
        "politics" => Some(Domain::Politics),
        "economics" => Some(Domain::Economics),
        _ => {
            if let Some(raw_id) = normalized.strip_prefix("custom:") {
                return raw_id.trim().parse::<u32>().ok().map(Domain::Custom);
            }
            if let Some(raw_id) = normalized
                .strip_prefix("custom(")
                .and_then(|v| v.strip_suffix(')'))
            {
                return raw_id.trim().parse::<u32>().ok().map(Domain::Custom);
            }
            None
        }
    }
}

fn parse_persisted_side(raw: &str) -> Option<Side> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "UP" | "YES" => Some(Side::Up),
        "DOWN" | "NO" => Some(Side::Down),
        _ => None,
    }
}

fn string_metadata_from_json(
    raw: Option<sqlx::types::Json<serde_json::Value>>,
) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    let Some(sqlx::types::Json(value)) = raw else {
        return metadata;
    };
    let Some(object) = value.as_object() else {
        return metadata;
    };

    for (key, value) in object {
        if value.is_null() {
            continue;
        }
        if let Some(v) = value.as_str() {
            metadata.insert(key.clone(), v.to_string());
        } else {
            metadata.insert(key.clone(), value.to_string());
        }
    }

    metadata
}

#[derive(Debug)]
struct PersistedExecutionFill {
    intent_id: Uuid,
    agent_id: String,
    domain: Domain,
    market_slug: String,
    token_id: String,
    side: Side,
    is_buy: bool,
    filled_shares: u64,
    fill_price: Decimal,
    executed_at: DateTime<Utc>,
    metadata: HashMap<String, String>,
}

#[derive(Debug)]
struct PersistedExecutionOutcome {
    agent_id: String,
    executed_at: DateTime<Utc>,
    is_failure: bool,
}

fn execution_error_is_failure(error: Option<&str>) -> bool {
    error.map(str::trim).map(|v| !v.is_empty()).unwrap_or(false)
}

async fn load_execution_log_fills(
    pool: &PgPool,
    account_id: &str,
    dry_run: bool,
) -> Result<Vec<PersistedExecutionFill>> {
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            String,
            String,
            String,
            bool,
            i64,
            Option<Decimal>,
            Decimal,
            DateTime<Utc>,
            Option<sqlx::types::Json<serde_json::Value>>,
        ),
    >(
        r#"
        SELECT
            intent_id,
            agent_id,
            domain,
            market_slug,
            token_id,
            market_side,
            is_buy,
            filled_shares,
            avg_fill_price,
            limit_price,
            executed_at,
            metadata
        FROM agent_order_executions
        WHERE account_id = $1
          AND dry_run = $2
          AND filled_shares > 0
        ORDER BY executed_at ASC, id ASC
        "#,
    )
    .bind(account_id)
    .bind(dry_run)
    .fetch_all(pool)
    .await
    .map_err(|e| crate::error::PloyError::Internal(format!("load execution log fills: {}", e)))?;

    let mut fills = Vec::new();
    for (
        intent_id,
        agent_id,
        domain_raw,
        market_slug,
        token_id,
        side_raw,
        is_buy,
        filled_shares_raw,
        avg_fill_price,
        limit_price,
        executed_at,
        metadata_raw,
    ) in rows
    {
        let Some(domain) = parse_persisted_domain(&domain_raw) else {
            warn!(
                account_id = %account_id,
                intent_id = %intent_id,
                domain = %domain_raw,
                "skipping execution-log row with unknown domain during restore"
            );
            continue;
        };
        let Some(side) = parse_persisted_side(&side_raw) else {
            warn!(
                account_id = %account_id,
                intent_id = %intent_id,
                side = %side_raw,
                "skipping execution-log row with unknown side during restore"
            );
            continue;
        };
        let Ok(filled_shares) = u64::try_from(filled_shares_raw) else {
            warn!(
                account_id = %account_id,
                intent_id = %intent_id,
                filled_shares = filled_shares_raw,
                "skipping execution-log row with invalid filled_shares during restore"
            );
            continue;
        };
        if filled_shares == 0 {
            continue;
        }
        let fill_price = avg_fill_price.unwrap_or(limit_price);
        if fill_price <= Decimal::ZERO {
            warn!(
                account_id = %account_id,
                intent_id = %intent_id,
                fill_price = %fill_price,
                "skipping execution-log row with non-positive fill price during restore"
            );
            continue;
        }

        fills.push(PersistedExecutionFill {
            intent_id,
            agent_id,
            domain,
            market_slug,
            token_id,
            side,
            is_buy,
            filled_shares,
            fill_price,
            executed_at,
            metadata: string_metadata_from_json(metadata_raw),
        });
    }

    Ok(fills)
}

async fn load_execution_log_outcomes(
    pool: &PgPool,
    account_id: &str,
    dry_run: bool,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> Result<Vec<PersistedExecutionOutcome>> {
    let rows = sqlx::query_as::<_, (String, DateTime<Utc>, Option<String>)>(
        r#"
        SELECT
            agent_id,
            executed_at,
            error
        FROM agent_order_executions
        WHERE account_id = $1
          AND dry_run = $2
          AND executed_at >= $3
          AND executed_at < $4
        ORDER BY executed_at ASC, id ASC
        "#,
    )
    .bind(account_id)
    .bind(dry_run)
    .bind(window_start)
    .bind(window_end)
    .fetch_all(pool)
    .await
    .map_err(|e| {
        crate::error::PloyError::Internal(format!("load execution log outcomes: {}", e))
    })?;

    Ok(rows
        .into_iter()
        .map(|(agent_id, executed_at, error)| PersistedExecutionOutcome {
            agent_id,
            executed_at,
            is_failure: execution_error_is_failure(error.as_deref()),
        })
        .collect())
}

fn normalized_identity_component(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_ascii_lowercase())
}

fn intent_condition_id(intent: &OrderIntent) -> Option<String> {
    intent
        .condition_id()
        .and_then(normalized_identity_component)
}

fn intent_market_identity(intent: &OrderIntent) -> String {
    if let Some(condition_id) = intent_condition_id(intent) {
        return format!("condition:{}", condition_id);
    }
    if let Some(slug) = normalized_identity_component(&intent.market_slug) {
        return format!("slug:{}", slug);
    }
    if let Some(token) = normalized_identity_component(&intent.token_id) {
        return format!("token:{}", token);
    }
    "unknown".to_string()
}

fn intent_deployment_scope(intent: &OrderIntent) -> String {
    if let Some(scope) = intent
        .deployment_id()
        .and_then(normalized_identity_component)
    {
        return scope;
    }

    let strategy = intent
        .metadata
        .get("strategy")
        .and_then(|v| normalized_identity_component(v))
        .unwrap_or_else(|| "default".to_string());
    format!(
        "agent:{}|strategy:{}",
        intent.agent_id.trim().to_ascii_lowercase(),
        strategy
    )
}


fn deployment_state_label(state: DeploymentState) -> &'static str {
    match state {
        DeploymentState::Enabled => "enabled",
        DeploymentState::Draining => "draining",
        DeploymentState::Disabled => "disabled",
        DeploymentState::Archived => "archived",
    }
}

fn deployment_lifecycle_violation_reason(
    intent: &OrderIntent,
    deployment: &StrategyDeployment,
) -> Option<String> {
    if deployment.allows_intent_purpose(intent.purpose) {
        return None;
    }

    Some(format!(
        "deployment {} is {}; purpose {:?} is not allowed",
        deployment.id,
        deployment_state_label(deployment.effective_state()),
        intent.purpose
    ))
}


/// Resolve the notional reference price for sell-side exposure release.
///
/// Returns `(price, has_explicit_entry_price)` where `has_explicit_entry_price`
/// indicates whether the value came from metadata.
fn sell_release_reference_price(
    intent: &OrderIntent,
    execution_price: Decimal,
) -> Option<(Decimal, bool)> {
    if let Some(entry_price) = intent
        .metadata
        .get("entry_price")
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| Decimal::from_str(v).ok())
        .filter(|v| *v > Decimal::ZERO)
    {
        return Some((entry_price, true));
    }

    if execution_price > Decimal::ZERO {
        return Some((execution_price, false));
    }

    (intent.limit_price > Decimal::ZERO).then_some((intent.limit_price, false))
}


async fn persist_governance_policy(
    pool: &PgPool,
    account_id: &str,
    policy: &GovernancePolicy,
) -> Result<()> {
    let blocked_domains = governance_policy_blocked_domains_sorted(policy);
    let mut tx = pool.begin().await.map_err(|e| {
        crate::error::PloyError::Internal(format!("begin governance policy tx: {}", e))
    })?;

    sqlx::query(
        r#"
        INSERT INTO coordinator_governance_policies (
            account_id,
            block_new_intents,
            blocked_domains,
            max_intent_notional_usd,
            max_total_notional_usd,
            updated_at,
            updated_by,
            reason
        )
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
        ON CONFLICT (account_id) DO UPDATE SET
            block_new_intents = EXCLUDED.block_new_intents,
            blocked_domains = EXCLUDED.blocked_domains,
            max_intent_notional_usd = EXCLUDED.max_intent_notional_usd,
            max_total_notional_usd = EXCLUDED.max_total_notional_usd,
            updated_at = EXCLUDED.updated_at,
            updated_by = EXCLUDED.updated_by,
            reason = EXCLUDED.reason
        "#,
    )
    .bind(account_id)
    .bind(policy.block_new_intents)
    .bind(sqlx::types::Json(blocked_domains.clone()))
    .bind(policy.max_intent_notional_usd)
    .bind(policy.max_total_notional_usd)
    .bind(policy.updated_at)
    .bind(policy.updated_by.clone())
    .bind(policy.reason.clone())
    .execute(&mut *tx)
    .await
    .map_err(|e| crate::error::PloyError::Internal(format!("persist governance policy: {}", e)))?;

    sqlx::query(
        r#"
        INSERT INTO coordinator_governance_policy_history (
            account_id,
            block_new_intents,
            blocked_domains,
            max_intent_notional_usd,
            max_total_notional_usd,
            updated_at,
            updated_by,
            reason
        )
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
        "#,
    )
    .bind(account_id)
    .bind(policy.block_new_intents)
    .bind(sqlx::types::Json(blocked_domains))
    .bind(policy.max_intent_notional_usd)
    .bind(policy.max_total_notional_usd)
    .bind(policy.updated_at)
    .bind(policy.updated_by.clone())
    .bind(policy.reason.clone())
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        crate::error::PloyError::Internal(format!("append governance policy history entry: {}", e))
    })?;

    tx.commit().await.map_err(|e| {
        crate::error::PloyError::Internal(format!("commit governance policy tx: {}", e))
    })?;

    Ok(())
}

fn clamp_governance_history_limit(limit: usize) -> usize {
    limit.clamp(1, 500)
}

async fn load_governance_policy_history(
    pool: &PgPool,
    account_id: &str,
    limit: usize,
) -> Result<Vec<GovernancePolicyHistoryEntry>> {
    let limit = clamp_governance_history_limit(limit) as i64;
    let rows = sqlx::query_as::<
        _,
        (
            i64,
            bool,
            sqlx::types::Json<Vec<String>>,
            Option<Decimal>,
            Option<Decimal>,
            chrono::DateTime<Utc>,
            String,
            Option<String>,
        ),
    >(
        r#"
        SELECT
            id,
            block_new_intents,
            blocked_domains,
            max_intent_notional_usd,
            max_total_notional_usd,
            updated_at,
            updated_by,
            reason
        FROM coordinator_governance_policy_history
        WHERE account_id = $1
        ORDER BY updated_at DESC, id DESC
        LIMIT $2
        "#,
    )
    .bind(account_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| {
        crate::error::PloyError::Internal(format!("load governance policy history: {}", e))
    })?;

    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                block_new_intents,
                sqlx::types::Json(blocked_domains),
                max_intent_notional_usd,
                max_total_notional_usd,
                updated_at,
                updated_by,
                reason,
            )| GovernancePolicyHistoryEntry {
                id,
                block_new_intents,
                blocked_domains: blocked_domains
                    .into_iter()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
                    .collect(),
                max_intent_notional_usd,
                max_total_notional_usd,
                updated_at,
                updated_by,
                reason: reason.and_then(|v| {
                    let trimmed = v.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                }),
                metadata: HashMap::new(),
            },
        )
        .collect())
}

async fn load_governance_policy(
    pool: &PgPool,
    account_id: &str,
) -> Result<Option<GovernancePolicy>> {
    let row = sqlx::query_as::<
        _,
        (
            bool,
            sqlx::types::Json<Vec<String>>,
            Option<Decimal>,
            Option<Decimal>,
            chrono::DateTime<Utc>,
            String,
            Option<String>,
        ),
    >(
        r#"
        SELECT
            block_new_intents,
            blocked_domains,
            max_intent_notional_usd,
            max_total_notional_usd,
            updated_at,
            updated_by,
            reason
        FROM coordinator_governance_policies
        WHERE account_id = $1
        "#,
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| crate::error::PloyError::Internal(format!("load governance policy: {}", e)))?;

    let Some((
        block_new_intents,
        sqlx::types::Json(raw_blocked_domains),
        max_intent_notional_usd,
        max_total_notional_usd,
        updated_at,
        updated_by,
        reason,
    )) = row
    else {
        return Ok(None);
    };

    let mut blocked_domains = HashSet::new();
    let mut unknown_domains = Vec::new();
    for raw in raw_blocked_domains {
        if let Some(domain) = parse_governance_domain(&raw) {
            blocked_domains.insert(domain);
        } else {
            unknown_domains.push(raw);
        }
    }
    if !unknown_domains.is_empty() {
        warn!(
            account_id = %account_id,
            domains = ?unknown_domains,
            "ignoring unknown governance blocked domains from DB"
        );
    }

    let max_intent_notional_usd = max_intent_notional_usd.filter(|v| *v > Decimal::ZERO);
    let max_total_notional_usd = max_total_notional_usd.filter(|v| *v > Decimal::ZERO);
    let updated_by = {
        let trimmed = updated_by.trim();
        if trimmed.is_empty() {
            "db.restore".to_string()
        } else {
            trimmed.to_string()
        }
    };
    let reason = reason.and_then(|v| {
        let trimmed = v.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    });

    Ok(Some(GovernancePolicy {
        block_new_intents,
        blocked_domains,
        max_intent_notional_usd,
        max_total_notional_usd,
        updated_at,
        updated_by,
        reason,
        metadata: HashMap::new(),
    }))
}

/// Clonable handle given to agents for submitting orders and state updates
#[derive(Clone)]
pub struct CoordinatorHandle {
    account_id: String,
    order_tx: mpsc::Sender<OrderIntent>,
    state_tx: mpsc::Sender<AgentSnapshot>,
    control_tx: mpsc::Sender<CoordinatorControlCommand>,
    global_state: Arc<RwLock<GlobalState>>,
    risk_gate: Arc<RiskGate>,
    order_queue: Arc<RwLock<OrderQueue>>,
    capital_policy: Arc<CapitalPolicy>,
    positions: Arc<PositionAggregator>,
    governance: Arc<GovernanceController>,
    admission: Arc<AdmissionController>,
    allowed_domains: Arc<HashSet<Domain>>,
    authorized_agents: Arc<RwLock<HashSet<String>>>,
    governance_store_pool: Option<PgPool>,
}

impl CoordinatorHandle {
    async fn deployment_lifecycle_violation(&self, intent: &OrderIntent) -> Option<String> {
        let deployment_id = intent.deployment_id()?.to_string();
        let deployments = self.deployments.read().await;
        let deployment = deployments.get(deployment_id.as_str())?;
        deployment_lifecycle_violation_reason(intent, deployment)
    }

    /// Submit an order intent to the coordinator for risk checking and execution
    pub async fn submit_order(&self, intent: OrderIntent) -> Result<()> {
        if !self.allowed_domains.contains(&intent.domain) {
            return Err(crate::error::PloyError::Validation(format!(
                "domain {} is not enabled for this runtime",
                intent.domain
            )));
        }
        if let Some(reason) = buy_intent_missing_deployment_reason(&intent) {
            return Err(crate::error::PloyError::Validation(reason));
        }
        if let Some(reason) = self.deployment_lifecycle_violation(&intent).await {
            return Err(crate::error::PloyError::Validation(reason));
        }

        if !intent.is_buy {
            let tracked_open_shares = self
                .positions
                .agent_open_shares_for_token_side(
                    &intent.agent_id,
                    intent.domain,
                    &intent.token_id,
                    intent.side,
                )
                .await;
            let pending_sell_shares = self.order_queue.read().await.pending_sell_shares_for(
                &intent.agent_id,
                intent.domain,
                &intent.token_id,
                intent.side,
            );
            if let Some(reason) =
                sell_reduce_only_violation_reason(&intent, tracked_open_shares, pending_sell_shares)
            {
                return Err(crate::error::PloyError::Validation(reason));
            }
        }

        // Binary-options semantics (Polymarket): SELL intents are treated as
        // reduce-only exits and must remain allowed during pause/halt.
        if intent.is_buy {
            let global_mode = *self.ingress_mode.read().await;
            let domain_mode = self
                .domain_ingress_mode
                .read()
                .await
                .get(&intent.domain)
                .copied()
                .unwrap_or(IngressMode::Running);

            if global_mode != IngressMode::Running {
                return Err(crate::error::PloyError::Validation(format!(
                    "coordinator global ingress is {:?}; new intents are blocked",
                    global_mode
                )));
            }

            if domain_mode != IngressMode::Running {
                return Err(crate::error::PloyError::Validation(format!(
                    "coordinator {:?} ingress is {:?}; new intents are blocked",
                    intent.domain, domain_mode
                )));
            }
        }
        self.order_tx.send(intent).await.map_err(|_| {
            crate::error::PloyError::Internal("coordinator order channel closed".into())
        })
    }

    /// Report agent state (heartbeat + position/PnL snapshot)
    pub async fn update_agent_state(&self, snapshot: AgentSnapshot) -> Result<()> {
        self.state_tx.send(snapshot).await.map_err(|_| {
            crate::error::PloyError::Internal("coordinator state channel closed".into())
        })
    }

    /// Pause all agents
    pub async fn pause_all(&self) -> Result<()> {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Paused;
        }
        self.domain_ingress_mode.write().await.clear();
        self.control_tx
            .send(CoordinatorControlCommand::PauseAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Resume all agents
    pub async fn resume_all(&self) -> Result<()> {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Running;
        }
        self.domain_ingress_mode.write().await.clear();
        self.control_tx
            .send(CoordinatorControlCommand::ResumeAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Force-close all positions and stop agents
    pub async fn force_close_all(&self) -> Result<()> {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Halted;
        }
        self.domain_ingress_mode.write().await.clear();
        self.control_tx
            .send(CoordinatorControlCommand::ForceCloseAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Shutdown all agents gracefully
    pub async fn shutdown_all(&self) -> Result<()> {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Halted;
        }
        self.domain_ingress_mode.write().await.clear();
        self.control_tx
            .send(CoordinatorControlCommand::ShutdownAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Pause a specific domain
    pub async fn pause_domain(&self, domain: Domain) -> Result<()> {
        {
            let mut domain_mode = self.domain_ingress_mode.write().await;
            domain_mode.insert(domain, IngressMode::Paused);
        }
        self.control_tx
            .send(CoordinatorControlCommand::PauseDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Resume a specific domain
    pub async fn resume_domain(&self, domain: Domain) -> Result<()> {
        {
            let mut domain_mode = self.domain_ingress_mode.write().await;
            domain_mode.remove(&domain);
        }
        self.control_tx
            .send(CoordinatorControlCommand::ResumeDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Force-close positions for one domain
    pub async fn force_close_domain(&self, domain: Domain) -> Result<()> {
        {
            let mut domain_mode = self.domain_ingress_mode.write().await;
            domain_mode.insert(domain, IngressMode::Halted);
        }
        self.control_tx
            .send(CoordinatorControlCommand::ForceCloseDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Shutdown one domain
    pub async fn shutdown_domain(&self, domain: Domain) -> Result<()> {
        {
            let mut domain_mode = self.domain_ingress_mode.write().await;
            domain_mode.insert(domain, IngressMode::Halted);
        }
        self.control_tx
            .send(CoordinatorControlCommand::ShutdownDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Pause a single agent by ID (used by OpenClaw meta-agent)
    pub async fn pause_agent(&self, agent_id: &str) -> Result<()> {
        self.control_tx
            .send(CoordinatorControlCommand::PauseAgent(agent_id.to_string()))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Resume a single agent by ID (used by OpenClaw meta-agent)
    pub async fn resume_agent(&self, agent_id: &str) -> Result<()> {
        self.control_tx
            .send(CoordinatorControlCommand::ResumeAgent(agent_id.to_string()))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Read the current global state (non-blocking snapshot)
    pub async fn read_state(&self) -> GlobalState {
        self.global_state.read().await.clone()
    }

    /// Shared deployment registry (single source of truth for API + coordinator).
    pub fn shared_deployments(&self) -> Arc<RwLock<HashMap<String, StrategyDeployment>>> {
        self.deployments.clone()
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
    pub fn is_agent_authorized(&self, agent_id: &str) -> bool {
        self.authorized_agents
            .read()
            .map(|agents| agents.contains(agent_id))
            .unwrap_or(false)
    }

    /// Read current account-level governance policy.
    pub async fn governance_policy(&self) -> GovernancePolicySnapshot {
        self.governance_policy.read().await.to_snapshot()
    }

    /// Read account-level governance policy change history (latest first).
    pub async fn governance_policy_history(
        &self,
        limit: usize,
    ) -> Result<Vec<GovernancePolicyHistoryEntry>> {
        let Some(pool) = self.governance_store_pool.as_ref() else {
            return Err(crate::error::PloyError::Validation(
                "governance history store is unavailable in this runtime".to_string(),
            ));
        };
        load_governance_policy_history(pool, &self.account_id, limit).await
    }

    /// Replace account-level governance policy (control-plane managed).
    pub async fn update_governance_policy(
        &self,
        update: GovernancePolicyUpdate,
    ) -> Result<GovernancePolicySnapshot> {
        let next = GovernancePolicy::try_from_update(update)
            .map_err(crate::error::PloyError::Validation)?;
        if let Some(pool) = self.governance_store_pool.as_ref() {
            persist_governance_policy(pool, &self.account_id, &next).await?;
        }
        let snapshot = next.to_snapshot();
        let mut policy = self.governance_policy.write().await;
        *policy = next;
        Ok(snapshot)
    }

    /// Read runtime governance + risk + capital ledger snapshot.
    pub async fn governance_status(&self) -> GovernanceStatusSnapshot {
        let ingress_mode = self.ingress_mode.read().await.as_str().to_string();
        let domain_ingress_modes = {
            let modes = self.domain_ingress_mode.read().await;
            let mut rows = modes
                .iter()
                .map(|(domain, mode)| DomainIngressSnapshot {
                    domain: governance_domain_snapshot_label(*domain),
                    mode: mode.as_str().to_string(),
                })
                .collect::<Vec<_>>();
            rows.sort_by(|a, b| a.domain.cmp(&b.domain));
            rows
        };
        let policy = self.governance_policy.read().await.to_snapshot();
        let risk_state = self.risk_gate.state().await;
        let platform_exposure_usd = self.risk_gate.total_exposure().await;
        let (daily_pnl_usd, _, _) = self.risk_gate.daily_stats().await;
        let daily_loss_limit_usd = self.risk_gate.daily_loss_limit();
        let (queue, other_pending_buy_notional_usd) = {
            let queue = self.order_queue.read().await;
            (
                QueueStatsSnapshot::from(queue.stats()),
                queue.pending_buy_notional_excluding_domains(&[
                    Domain::Crypto,
                    Domain::Sports,
                    Domain::Politics,
                    Domain::Economics,
                ]),
            )
        };
        let agents = {
            let global = self.global_state.read().await;
            let mut rows = global
                .agents
                .values()
                .map(|snap| GovernanceAgentSnapshot {
                    agent_id: snap.agent_id.clone(),
                    name: snap.name.clone(),
                    domain: governance_domain_snapshot_label(snap.domain),
                    status: snap.status.to_string().to_ascii_lowercase(),
                    exposure: snap.exposure,
                    daily_pnl: snap.daily_pnl,
                    last_heartbeat: snap.last_heartbeat,
                    error_message: snap.error_message.clone(),
                })
                .collect::<Vec<_>>();
            rows.sort_by(|a, b| a.domain.cmp(&b.domain).then_with(|| a.name.cmp(&b.name)));
            rows
        };

        let (crypto, mut deployments) = {
            let allocator = self.crypto_allocator.read().await;
            (
                allocator.ledger_snapshot(),
                allocator.deployment_ledger_snapshot(),
            )
        };
        let (sports, sports_deployments) = {
            let allocator = self.sports_allocator.read().await;
            (
                allocator.ledger_snapshot(),
                allocator.deployment_ledger_snapshot(),
            )
        };
        deployments.extend(sports_deployments);
        let (politics, politics_deployments) = {
            let allocator = self.politics_allocator.read().await;
            (
                allocator.ledger_snapshot(),
                allocator.deployment_ledger_snapshot(),
            )
        };
        deployments.extend(politics_deployments);
        let (economics, economics_deployments) = {
            let allocator = self.economics_allocator.read().await;
            (
                allocator.ledger_snapshot(),
                allocator.deployment_ledger_snapshot(),
            )
        };
        deployments.extend(economics_deployments);
        deployments.sort_by(|a, b| {
            a.domain
                .cmp(&b.domain)
                .then_with(|| a.deployment_id.cmp(&b.deployment_id))
        });
        let allocator_open_notional = crypto.open_notional_usd
            + sports.open_notional_usd
            + politics.open_notional_usd
            + economics.open_notional_usd;
        let allocator_pending_notional = crypto.pending_notional_usd
            + sports.pending_notional_usd
            + politics.pending_notional_usd
            + economics.pending_notional_usd;
        let open_notional_usd = platform_exposure_usd.max(allocator_open_notional);
        let account_notional_usd =
            open_notional_usd + allocator_pending_notional + other_pending_buy_notional_usd;

        GovernanceStatusSnapshot {
            account_id: self.account_id.clone(),
            ingress_mode,
            domain_ingress_modes,
            policy,
            account_notional_usd,
            platform_exposure_usd,
            risk_state,
            daily_pnl_usd,
            daily_loss_limit_usd,
            queue,
            agents,
            allocators: vec![crypto, sports, politics, economics],
            deployments,
            updated_at: Utc::now(),
        }
    }
}

/// The Coordinator — owns shared infrastructure and runs the main event loop
pub struct Coordinator {
    config: CoordinatorConfig,
    account_id: String,
    allowed_domains: Arc<HashSet<Domain>>,
    authorized_agents: Arc<RwLock<HashSet<String>>>,
    admission: Arc<AdmissionController>,
    risk_gate: Arc<RiskGate>,
    order_queue: Arc<RwLock<OrderQueue>>,
    capital_policy: Arc<CapitalPolicy>,
    positions: Arc<PositionAggregator>,
    executor: Arc<OrderExecutor>,
    global_state: Arc<RwLock<GlobalState>>,
    journal: ExecutionJournal,
    governance_store_pool: Option<PgPool>,
    governance: Arc<GovernanceController>,
    stale_heartbeat_warn_at: Arc<RwLock<HashMap<String, chrono::DateTime<Utc>>>>,
    order_update_sinks: Arc<RwLock<HashMap<String, mpsc::Sender<crate::strategy::OrderUpdate>>>>,

    // Channels
    order_tx: mpsc::Sender<OrderIntent>,
    order_rx: mpsc::Receiver<OrderIntent>,
    state_tx: mpsc::Sender<AgentSnapshot>,
    state_rx: mpsc::Receiver<AgentSnapshot>,
    control_tx: mpsc::Sender<CoordinatorControlCommand>,
    control_rx: mpsc::Receiver<CoordinatorControlCommand>,

    // Per-agent command channels
    agent_commands: HashMap<String, AgentCommandChannel>,
}

/// Extracted context for the intent-processing worker task.
///
/// Contains only the Arc-wrapped fields that `handle_order_intent` needs,
/// allowing it to run in a dedicated tokio task without blocking the
/// coordinator's main `select!` loop.
pub(super) struct IntentWorkerCtx {
    pub(super) account_id: String,
    pub(super) allowed_domains: Arc<HashSet<Domain>>,
    pub(super) admission: Arc<AdmissionController>,
    pub(super) risk_gate: Arc<RiskGate>,
    pub(super) order_queue: Arc<RwLock<OrderQueue>>,
    pub(super) capital_policy: Arc<CapitalPolicy>,
    pub(super) positions: Arc<PositionAggregator>,
    pub(super) executor: Arc<OrderExecutor>,
    pub(super) journal: ExecutionJournal,
    pub(super) governance: Arc<GovernanceController>,
    pub(super) order_update_sinks:
        Arc<RwLock<HashMap<String, mpsc::Sender<crate::strategy::OrderUpdate>>>>,
}

impl Coordinator {
    pub fn new(
        config: CoordinatorConfig,
        executor: Arc<OrderExecutor>,
        account_id: String,
        allowed_domains: HashSet<Domain>,
    ) -> Self {
        let (order_tx, order_rx) = mpsc::channel(256);
        let (state_tx, state_rx) = mpsc::channel(128);
        let (control_tx, control_rx) = mpsc::channel(32);

        let allowed_domains = Arc::new(allowed_domains);
        let authorized_agents = Arc::new(RwLock::new(HashSet::new()));
        let risk_gate = Arc::new(RiskGate::new(config.risk.clone()));
        let order_queue = Arc::new(RwLock::new(OrderQueue::new(1024)));
        let admission = Arc::new(AdmissionController::new(&config));
        let capital_policy = Arc::new(CapitalPolicy::new(&config));
        let positions = Arc::new(PositionAggregator::new());
        let global_state = Arc::new(RwLock::new(GlobalState::new()));
        let governance = Arc::new(GovernanceController::new(&config));
        let stale_heartbeat_warn_at = Arc::new(RwLock::new(HashMap::new()));
        let order_update_sinks = Arc::new(RwLock::new(HashMap::new()));
        let account_id = if account_id.trim().is_empty() {
            "default".to_string()
        } else {
            account_id
        };
        let journal = ExecutionJournal::new(account_id.clone());

        Self {
            config,
            account_id,
            allowed_domains,
            authorized_agents,
            admission,
            risk_gate,
            order_queue,
            capital_policy,
            positions,
            executor,
            global_state,
            journal,
            governance_store_pool: None,
            governance,
            stale_heartbeat_warn_at,
            order_update_sinks,
            order_tx,
            order_rx,
            state_tx,
            state_rx,
            control_tx,
            control_rx,
            agent_commands: HashMap::new(),
        }
    }

    /// Create a clonable handle for agents
    pub fn handle(&self) -> CoordinatorHandle {
        CoordinatorHandle {
            account_id: self.account_id.clone(),
            order_tx: self.order_tx.clone(),
            state_tx: self.state_tx.clone(),
            control_tx: self.control_tx.clone(),
            global_state: self.global_state.clone(),
            risk_gate: self.risk_gate.clone(),
            order_queue: self.order_queue.clone(),
            capital_policy: self.capital_policy.clone(),
            positions: self.positions.clone(),
            governance: self.governance.clone(),
            admission: self.admission.clone(),
            allowed_domains: self.allowed_domains.clone(),
            authorized_agents: self.authorized_agents.clone(),
            governance_store_pool: self.governance_store_pool.clone(),
        }
    }

    /// Shared global state reference (for TUI)
    pub fn global_state(&self) -> Arc<RwLock<GlobalState>> {
        self.global_state.clone()
    }

    /// Position aggregator reference
    pub fn positions(&self) -> Arc<PositionAggregator> {
        self.positions.clone()
    }

    /// Build an `IntentWorkerCtx` from the coordinator's shared fields.
    fn intent_worker_ctx(&self) -> IntentWorkerCtx {
        IntentWorkerCtx {
            account_id: self.account_id.clone(),
            allowed_domains: self.allowed_domains.clone(),
            admission: self.admission.clone(),
            risk_gate: self.risk_gate.clone(),
            order_queue: self.order_queue.clone(),
            capital_policy: self.capital_policy.clone(),
            positions: self.positions.clone(),
            executor: self.executor.clone(),
            journal: self.journal.clone(),
            governance: self.governance.clone(),
            order_update_sinks: self.order_update_sinks.clone(),
        }
    }

    /// Convenience delegation for tests and direct callers.
    pub(super) async fn handle_order_intent(&self, intent: OrderIntent) {
        self.intent_worker_ctx().handle_order_intent(intent).await;
    }

    /// Register an agent and return its command receiver
    pub async fn register_agent(
        &mut self,
        agent_id: String,
        domain: Domain,
        risk_params: AgentRiskParams,
    ) -> mpsc::Receiver<CoordinatorCommand> {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        self.agent_commands
            .insert(agent_id.clone(), AgentCommandChannel { domain, tx: cmd_tx });
        self.authorized_agents
            .write()
            .await
            .insert(agent_id.clone());

        // Register with risk gate (now safe since register_agent is async)
        self.risk_gate
            .register_agent_with_domain(&agent_id, domain, risk_params)
            .await;

        info!(agent_id, "agent registered with coordinator");
        cmd_rx
    }

    /// Main coordinator loop — blocks until shutdown
    pub async fn run(mut self, mut shutdown_rx: tokio::sync::broadcast::Receiver<()>) {
        info!(
            agents = self.agent_commands.len(),
            "coordinator starting main loop"
        );

        let drain_interval = tokio::time::Duration::from_millis(self.config.queue_drain_ms);
        let refresh_interval = tokio::time::Duration::from_millis(self.config.state_refresh_ms);

        let mut drain_tick = tokio::time::interval(drain_interval);
        let mut refresh_tick = tokio::time::interval(refresh_interval);

        // Don't burst-fire missed ticks
        drain_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Spawn a dedicated worker for order intent processing so that
        // handle_order_intent (which does risk checks, capital reservation,
        // and queue enqueue) does not block the select! loop.  A bounded
        // channel preserves sequential intent ordering — important for
        // consistent capital reservation and duplicate-intent detection.
        let (intent_worker_tx, mut intent_worker_rx) = mpsc::channel::<OrderIntent>(64);

        // Move the fields the worker needs into an Arc-friendly bundle.
        // All accessed fields are already Arc-wrapped or Clone, so we
        // just clone the references the worker will use.
        let worker_self = IntentWorkerCtx {
            account_id: self.account_id.clone(),
            allowed_domains: self.allowed_domains.clone(),
            admission: self.admission.clone(),
            risk_gate: self.risk_gate.clone(),
            order_queue: self.order_queue.clone(),
            capital_policy: self.capital_policy.clone(),
            positions: self.positions.clone(),
            executor: self.executor.clone(),
            journal: self.journal.clone(),
            governance: self.governance.clone(),
            order_update_sinks: self.order_update_sinks.clone(),
        };

        let intent_worker = tokio::spawn(async move {
            while let Some(intent) = intent_worker_rx.recv().await {
                worker_self.handle_order_intent(intent).await;
            }
            debug!("intent worker: channel closed, exiting");
        });

        loop {
            tokio::select! {
                // --- Control commands (pause/resume/force-close) ---
                Some(cmd) = self.control_rx.recv() => {
                    self.handle_control_command(cmd).await;
                }

                // --- Incoming order intents ---
                Some(intent) = self.order_rx.recv() => {
                    if let Err(e) = intent_worker_tx.send(intent).await {
                        warn!("intent worker channel closed, dropping intent: {}", e);
                    }
                }

                // --- Agent state updates (heartbeats) ---
                Some(snapshot) = self.state_rx.recv() => {
                    self.handle_state_update(snapshot).await;
                }

                // --- Periodic: drain queue and execute ---
                _ = drain_tick.tick() => {
                    self.drain_and_execute().await;
                }

                // --- Periodic: refresh global state ---
                _ = refresh_tick.tick() => {
                    self.refresh_global_state().await;
                }

                // --- Shutdown signal ---
                _ = shutdown_rx.recv() => {
                    info!("coordinator: shutdown signal received");
                    // Drop the sender so the intent worker drains remaining
                    // intents and exits cleanly.
                    drop(intent_worker_tx);
                    let _ = intent_worker.await;
                    self.shutdown().await;
                    break;
                }
            }
        }

        info!("coordinator: main loop exited");
    }

    /// Risk-check an incoming order intent and enqueue if passed
    async fn handle_order_intent(&self, intent: OrderIntent) {
        let mut intent = intent;
        let agent_id = intent.agent_id.clone();
        let intent_id = intent.intent_id;
        let strategy_max_shares = intent.shares;

        if !self.is_domain_allowed(intent.domain) {
            let reason = format!("domain {} is not enabled for this runtime", intent.domain);
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by runtime domain allowlist"
            );
            return;
        }

        if let Some(reason) = buy_intent_missing_deployment_reason(&intent) {
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked due to missing deployment identity"
            );
            return;
        }
        if let Some(deployment_id) = intent.deployment_id().map(ToString::to_string) {
            let deployments = self.deployments.read().await;
            if let Some(deployment) = deployments.get(deployment_id.as_str()) {
                if let Some(reason) = deployment_lifecycle_violation_reason(&intent, deployment) {
                    self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                        .await;
                    warn!(
                        %agent_id, %intent_id, reason = %reason,
                        "order blocked by deployment lifecycle state"
                    );
                    return;
                }
            }
        }

        if !intent.is_buy {
            let tracked_open_shares = self
                .positions
                .agent_open_shares_for_token_side(
                    &intent.agent_id,
                    intent.domain,
                    &intent.token_id,
                    intent.side,
                )
                .await;
            let pending_sell_shares = self.order_queue.read().await.pending_sell_shares_for(
                &intent.agent_id,
                intent.domain,
                &intent.token_id,
                intent.side,
            );

            if let Some(reason) =
                sell_reduce_only_violation_reason(&intent, tracked_open_shares, pending_sell_shares)
            {
                self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                    .await;
                warn!(
                    %agent_id, %intent_id, reason = %reason,
                    "order blocked by reduce-only sell guard"
                );
                return;
            }
        }
        let ingress_mode = *self.ingress_mode.read().await;
        if intent.is_buy && ingress_mode != IngressMode::Running {
            let reason = format!(
                "Coordinator ingress is {:?}; blocking BUY intent while paused/halted",
                ingress_mode
            );
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by coordinator ingress state"
            );
            return;
        }
        if intent.is_buy {
            let domain_mode = self
                .domain_ingress_mode
                .read()
                .await
                .get(&intent.domain)
                .copied()
                .unwrap_or(IngressMode::Running);
            if domain_mode != IngressMode::Running {
                let reason = format!(
                    "Domain {:?} ingress is {:?}; blocking BUY intent while paused/halted",
                    intent.domain, domain_mode
                );
                self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                    .await;
                warn!(
                    %agent_id, %intent_id, reason = %reason,
                    "order blocked by coordinator domain ingress state"
                );
                return;
            }
        }
        // Per-agent pause check
        if intent.is_buy
            && self
                .paused_agent_ids
                .read()
                .await
                .contains(&intent.agent_id)
        {
            let reason = format!("Agent {} is paused; blocking BUY intent", intent.agent_id);
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by per-agent pause"
            );
            return;
        }

        if let Some(reason) = self.check_governance_policy(&intent).await {
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by global governance policy"
            );
            return;
        }

        if let Err(reason) = self.enforce_live_buy_deployment_gate(&mut intent).await {
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by deployment gate"
            );
            return;
        }

        self.persist_signal_from_intent(&intent).await;
        if !intent.is_buy {
            self.persist_exit_reason_intent(&intent).await;
        }

        if let Some(reason) = self.check_duplicate_intent(&intent).await {
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by duplicate-intent guard"
            );
            return;
        }

        if let Some(reason) = self.apply_kelly_sizing(&mut intent).await {
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by kelly sizing policy"
            );
            return;
        }

        if let Some(reason) = self.apply_min_order_constraints(&mut intent, strategy_max_shares) {
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by venue minimum constraints"
            );
            return;
        }

        let mut adjusted: Option<(u64, String)> = None;
        let mut evaluated = intent;
        for attempt in 0..3 {
            match self.risk_gate.check_order(&evaluated).await {
                RiskCheckResult::Passed => {
                    if let Some(reason) = self.reserve_domain_capital(&evaluated).await {
                        self.persist_risk_decision(
                            &evaluated,
                            "BLOCKED",
                            Some(reason.clone()),
                            adjusted.clone(),
                        )
                        .await;
                        warn!(
                            %agent_id, %intent_id, reason = %reason,
                            "order blocked by domain allocator"
                        );
                        return;
                    }

                    self.persist_risk_decision(&evaluated, "PASSED", None, adjusted.clone())
                        .await;
                    let mut queue = self.order_queue.write().await;
                    match queue.enqueue(evaluated) {
                        Ok(()) => {
                            debug!(
                                %agent_id, %intent_id,
                                "order enqueued"
                            );
                        }
                        Err(e) => {
                            self.release_domain_reservation(intent_id).await;
                            warn!(%agent_id, %intent_id, error = %e, "queue full, order dropped");
                        }
                    }
                    return;
                }
                RiskCheckResult::Blocked(reason) => {
                    self.persist_risk_decision(
                        &evaluated,
                        "BLOCKED",
                        Some(reason.to_string()),
                        adjusted.clone(),
                    )
                    .await;
                    warn!(
                        %agent_id, %intent_id,
                        reason = ?reason,
                        "order blocked by risk gate"
                    );
                    return;
                }
                RiskCheckResult::Adjusted(suggestion) => {
                    // Apply adjustment locally and re-evaluate; agents do not automatically resubmit.
                    if suggestion.max_shares == 0 {
                        let reason =
                            format!("risk-gate suggested max_shares=0: {}", suggestion.reason);
                        self.persist_risk_decision(
                            &evaluated,
                            "BLOCKED",
                            Some(reason.clone()),
                            adjusted.clone(),
                        )
                        .await;
                        warn!(%agent_id, %intent_id, reason = %reason, "order blocked after risk adjustment");
                        return;
                    }

                    adjusted = Some((suggestion.max_shares, suggestion.reason.clone()));
                    evaluated.shares = suggestion.max_shares;
                    info!(
                        %agent_id, %intent_id,
                        attempt,
                        max_shares = suggestion.max_shares,
                        reason = %suggestion.reason,
                        "order adjusted by risk gate; re-evaluating"
                    );
                }
            }
        }

        let reason = "risk-gate adjustment loop exceeded max attempts".to_string();
        self.persist_risk_decision(&evaluated, "BLOCKED", Some(reason.clone()), adjusted)
            .await;
        warn!(%agent_id, %intent_id, reason = %reason, "order blocked");
    }

    fn deployment_gate_required() -> bool {
        match std::env::var("PLOY_DEPLOYMENT_GATE_REQUIRED")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
        {
            Some(v) => !matches!(v.as_str(), "0" | "false" | "no" | "off"),
            None => true,
        }
    }

    fn deployments_state_path() -> PathBuf {
        if let Ok(path) = std::env::var("PLOY_DEPLOYMENTS_FILE") {
            return PathBuf::from(path);
        }

        let container_data_root = Path::new("/opt/ploy/data");
        if container_data_root.exists() {
            return container_data_root.join("state/deployments.json");
        }

        let repo_state_deployment = Path::new("data/state/deployments.json");
        if repo_state_deployment.exists() {
            return repo_state_deployment.to_path_buf();
        }

        let repo_root_deployment = Path::new("deployment/deployments.json");
        if repo_root_deployment.exists() {
            return repo_root_deployment.to_path_buf();
        }

        let container_deployment = Path::new("/opt/ploy/deployment/deployments.json");
        if container_deployment.exists() {
            return container_deployment.to_path_buf();
        }

        PathBuf::from("data/state/deployments.json")
    }

    fn parse_strategy_deployments(raw: &str) -> HashMap<String, StrategyDeployment> {
        let mut out = HashMap::new();
        if let Ok(items) = serde_json::from_str::<Vec<StrategyDeployment>>(raw) {
            for mut dep in items {
                let id = dep.id.trim().to_string();
                if id.is_empty() {
                    continue;
                }
                dep.id = id.clone();
                dep.normalize_account_ids_in_place();
                out.insert(id, dep);
            }
        }
        out
    }

    fn load_strategy_deployments() -> HashMap<String, StrategyDeployment> {
        let raw = std::env::var("PLOY_STRATEGY_DEPLOYMENTS_JSON")
            .or_else(|_| std::env::var("PLOY_DEPLOYMENTS_JSON"))
            .unwrap_or_default();
        if !raw.trim().is_empty() {
            return Self::parse_strategy_deployments(&raw);
        }

        let repo_state_path = Path::new("data/state/deployments.json");
        let container_data_path = Path::new("/opt/ploy/data/state/deployments.json");
        let candidates = [
            Self::deployments_state_path(),
            repo_state_path.to_path_buf(),
            container_data_path.to_path_buf(),
            Path::new("deployment/deployments.json").to_path_buf(),
            Path::new("/opt/ploy/deployment/deployments.json").to_path_buf(),
        ];

        for path in candidates {
            if let Ok(contents) = std::fs::read_to_string(path) {
                let parsed = Self::parse_strategy_deployments(&contents);
                if !parsed.is_empty() {
                    return parsed;
                }
            }
        }

        HashMap::new()
    }

    async fn refresh_strategy_deployments(&self) {
        let loaded = Self::load_strategy_deployments();
        let mut deployments = self.deployments.write().await;
        *deployments = loaded;
    }

    fn metadata_value<'a>(metadata: &'a HashMap<String, String>, keys: &[&str]) -> Option<&'a str> {
        keys.iter()
            .find_map(|k| metadata.get(*k))
            .map(String::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    }

    fn normalized_token(raw: &str) -> String {
        raw.trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect()
    }

    fn strategy_matches(intent_strategy: &str, deployment_strategy: &str) -> bool {
        let intent = Self::normalized_token(intent_strategy);
        let dep = Self::normalized_token(deployment_strategy);
        if intent.is_empty() || dep.is_empty() {
            return false;
        }
        intent == dep || intent.contains(&dep) || dep.contains(&intent)
    }

    fn selector_matches_intent(
        deployment: &StrategyDeployment,
        market_slug: &str,
        metadata: &HashMap<String, String>,
    ) -> bool {
        match &deployment.market_selector {
            MarketSelector::Static {
                symbol,
                series_id,
                market_slug: expected_market_slug,
            } => {
                if let Some(expected) = expected_market_slug
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    if !market_slug.eq_ignore_ascii_case(expected) {
                        return false;
                    }
                }

                if let Some(expected) = symbol.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                    if let Some(actual) = Self::metadata_value(metadata, &["symbol"]) {
                        if !actual.eq_ignore_ascii_case(expected) {
                            return false;
                        }
                    }
                }

                if let Some(expected) = series_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    if let Some(actual) =
                        Self::metadata_value(metadata, &["series_id", "event_series_id"])
                    {
                        if !actual.eq_ignore_ascii_case(expected) {
                            return false;
                        }
                    }
                }

                true
            }
            MarketSelector::Dynamic { domain, .. } => *domain == deployment.domain,
        }
    }

    fn timeframe_hint(intent: &OrderIntent) -> Option<String> {
        if let Some(raw) = Self::metadata_value(&intent.metadata, &["timeframe", "horizon"]) {
            if let Some(h) = CryptoHorizon::from_hint(raw) {
                return Some(h.as_str().to_string());
            }
            return Some(raw.to_ascii_lowercase());
        }

        if let Some(raw) = Self::metadata_value(&intent.metadata, &["series_id", "event_series_id"])
        {
            if let Some(h) = CryptoHorizon::from_hint(raw) {
                return Some(h.as_str().to_string());
            }
        }

        CryptoHorizon::from_hint(&intent.market_slug).map(|h| h.as_str().to_string())
    }

    fn deployment_matches_timeframe(deployment: &StrategyDeployment, intent: &OrderIntent) -> bool {
        let Some(timeframe) = Self::timeframe_hint(intent) else {
            return true;
        };
        timeframe.eq_ignore_ascii_case(deployment.timeframe.as_str())
    }

    fn deployment_runtime_eligible(
        deployment: &StrategyDeployment,
        account_id: &str,
        dry_run: bool,
        intent: &OrderIntent,
    ) -> bool {
        deployment.is_enabled_for_runtime(account_id, dry_run)
            && deployment.domain == intent.domain
            && Self::deployment_matches_timeframe(deployment, intent)
            && Self::selector_matches_intent(deployment, &intent.market_slug, &intent.metadata)
    }

    fn apply_deployment_metadata(intent: &mut OrderIntent, deployment: &StrategyDeployment) {
        intent
            .metadata
            .insert("deployment_id".to_string(), deployment.id.clone());
        intent
            .metadata
            .entry("timeframe".to_string())
            .or_insert_with(|| deployment.timeframe.as_str().to_string());
        intent
            .metadata
            .entry("allocator_profile".to_string())
            .or_insert_with(|| deployment.allocator_profile.clone());
        intent
            .metadata
            .entry("risk_profile".to_string())
            .or_insert_with(|| deployment.risk_profile.clone());
        intent
            .metadata
            .entry("deployment_strategy".to_string())
            .or_insert_with(|| deployment.strategy.clone());
        intent
            .metadata
            .entry("deployment_priority".to_string())
            .or_insert_with(|| deployment.priority.to_string());
        intent
            .metadata
            .entry("deployment_cooldown_secs".to_string())
            .or_insert_with(|| deployment.cooldown_secs.to_string());

        if let MarketSelector::Static {
            symbol,
            series_id,
            market_slug,
        } = &deployment.market_selector
        {
            if let Some(value) = symbol.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                intent
                    .metadata
                    .entry("symbol".to_string())
                    .or_insert_with(|| value.to_string());
            }
            if let Some(value) = series_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                intent
                    .metadata
                    .entry("series_id".to_string())
                    .or_insert_with(|| value.to_string());
                intent
                    .metadata
                    .entry("event_series_id".to_string())
                    .or_insert_with(|| value.to_string());
            }
            if let Some(value) = market_slug
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                intent
                    .metadata
                    .entry("selector_market_slug".to_string())
                    .or_insert_with(|| value.to_string());
            }
        }
    }

    fn enforce_deployment_gate_with_snapshot(
        account_id: &str,
        dry_run: bool,
        deployments: &HashMap<String, StrategyDeployment>,
        intent: &mut OrderIntent,
    ) -> std::result::Result<(), String> {
        if !intent.is_buy || dry_run || !Self::deployment_gate_required() {
            return Ok(());
        }

        if deployments.is_empty() {
            return Err(
                "deployment registry is empty while deployment gate is required".to_string(),
            );
        }

        if let Some(deployment_id) = Self::metadata_value(&intent.metadata, &["deployment_id"]) {
            let Some(deployment) = deployments.get(deployment_id) else {
                return Err(format!("unknown deployment_id: {}", deployment_id));
            };
            if !Self::deployment_runtime_eligible(deployment, account_id, dry_run, intent) {
                return Err(format!(
                    "deployment {} is not eligible for runtime/account/domain/timeframe/selector binding",
                    deployment.id
                ));
            }
            if let Some(reason) = deployment_lifecycle_violation_reason(intent, deployment) {
                return Err(reason);
            }
            Self::apply_deployment_metadata(intent, deployment);
            return Ok(());
        }

        let strategy = Self::metadata_value(&intent.metadata, &["strategy", "deployment_strategy"])
            .ok_or_else(|| "strategy metadata is required for live BUY intents".to_string())?;

        let mut candidates: Vec<&StrategyDeployment> = deployments
            .values()
            .filter(|deployment| {
                Self::deployment_runtime_eligible(deployment, account_id, dry_run, intent)
                    && Self::strategy_matches(strategy, deployment.strategy.as_str())
                    && deployment.allows_intent_purpose(intent.purpose)
            })
            .collect();

        if candidates.is_empty() {
            let mut domain_candidates: Vec<&StrategyDeployment> = deployments
                .values()
                .filter(|deployment| {
                    Self::deployment_runtime_eligible(deployment, account_id, dry_run, intent)
                        && deployment.allows_intent_purpose(intent.purpose)
                })
                .collect();
            domain_candidates.sort_by(|a, b| a.id.cmp(&b.id));

            if domain_candidates.len() == 1 {
                let deployment = domain_candidates[0];
                Self::apply_deployment_metadata(intent, deployment);
                intent.metadata.insert(
                    "deployment_resolution".to_string(),
                    "domain_singleton_fallback".to_string(),
                );
                return Ok(());
            }

            return Err(format!(
                "no eligible deployment found for strategy={} domain={} market={}",
                strategy, intent.domain, intent.market_slug
            ));
        }

        candidates.sort_by(|a, b| a.id.cmp(&b.id));

        if candidates.len() > 1 {
            let ids = candidates
                .iter()
                .map(|d| d.id.clone())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "ambiguous deployment resolution for strategy={} market={}: {}",
                strategy, intent.market_slug, ids
            ));
        }

        let deployment = candidates[0];
        Self::apply_deployment_metadata(intent, deployment);
        Ok(())
    }

    async fn enforce_live_buy_deployment_gate(
        &self,
        intent: &mut OrderIntent,
    ) -> std::result::Result<(), String> {
        if !intent.is_buy || self.executor.is_dry_run() || !Self::deployment_gate_required() {
            return Ok(());
        }
        if !self.is_domain_allowed(intent.domain) {
            return Err(format!(
                "domain {} is not enabled for this runtime",
                intent.domain
            ));
        }

        let explicit_id =
            Self::metadata_value(&intent.metadata, &["deployment_id"]).map(ToString::to_string);
        let should_refresh = {
            let deployments = self.deployments.read().await;
            deployments.is_empty()
                || explicit_id
                    .as_ref()
                    .is_some_and(|id| !deployments.contains_key(id.as_str()))
        };
        if should_refresh {
            self.refresh_strategy_deployments().await;
        }

        let deployments = self.deployments.read().await;
        Self::enforce_deployment_gate_with_snapshot(
            self.account_id.as_str(),
            self.executor.is_dry_run(),
            &deployments,
            intent,
        )
    }

    async fn apply_kelly_sizing(&self, intent: &mut OrderIntent) -> Option<String> {
        if !self.config.kelly_sizing_enabled {
            return None;
        }
        if !intent.is_buy {
            return None;
        }
        if intent.priority == OrderPriority::Critical {
            return None;
        }
        if intent.limit_price <= Decimal::ZERO || intent.limit_price >= Decimal::ONE {
            return None;
        }

        let p = intent
            .metadata
            .get("signal_fair_value")
            .or_else(|| intent.metadata.get("signal_win_prob"))
            .and_then(|v| Decimal::from_str(v).ok())?;
        let p = p.max(Decimal::ZERO).min(Decimal::ONE);
        let price = intent.limit_price;
        let edge = p - price;

        if edge < self.config.kelly_min_edge {
            return Some(format!(
                "kelly edge {} below min {}",
                edge, self.config.kelly_min_edge
            ));
        }

        let denom = Decimal::ONE - price;
        if denom <= Decimal::ZERO {
            return Some("kelly denom <= 0".to_string());
        }

        let raw_kelly = ((p - price) / denom).max(Decimal::ZERO).min(Decimal::ONE);
        if raw_kelly <= Decimal::ZERO {
            return Some("kelly fraction <= 0 (no positive edge)".to_string());
        }

        let mut effective_fraction = (raw_kelly * self.config.kelly_fraction_multiplier)
            .max(Decimal::ZERO)
            .min(Decimal::ONE);
        if let Some(conf) = intent
            .metadata
            .get("signal_confidence")
            .and_then(|v| Decimal::from_str(v).ok())
        {
            effective_fraction *= conf.max(Decimal::ZERO).min(Decimal::ONE);
        }

        if effective_fraction <= Decimal::ZERO {
            return Some("kelly effective fraction <= 0".to_string());
        }

        // Prefer sizing against allocator remaining budget when enabled; otherwise,
        // treat the strategy-provided notional as the bankroll for relative sizing.
        let bankroll = match intent.domain {
            Domain::Crypto => {
                let allocator = self.crypto_allocator.read().await;
                allocator
                    .available_notional_for(intent)
                    .unwrap_or_else(|| intent.notional_value())
            }
            Domain::Sports => {
                let allocator = self.sports_allocator.read().await;
                allocator
                    .available_notional_for(intent)
                    .unwrap_or_else(|| intent.notional_value())
            }
            _ => intent.notional_value(),
        };

        if bankroll <= Decimal::ZERO {
            return Some("kelly bankroll <= 0".to_string());
        }

        let target_notional = (bankroll * effective_fraction).max(Decimal::ZERO);
        if target_notional <= Decimal::ZERO {
            return Some("kelly target_notional <= 0".to_string());
        }

        let sized_shares = (target_notional / price)
            .floor()
            .to_u64()
            .unwrap_or(0)
            .min(intent.shares);

        let mut final_shares = sized_shares;
        if final_shares == 0 {
            let floor_shares = self.config.kelly_min_shares.min(intent.shares);
            if floor_shares > 0 {
                final_shares = floor_shares;
                intent
                    .metadata
                    .insert("kelly_min_shares_applied".to_string(), "true".to_string());
                intent.metadata.insert(
                    "kelly_min_shares_floor".to_string(),
                    floor_shares.to_string(),
                );
            } else {
                return Some("kelly sizing produced 0 shares".to_string());
            }
        }

        if final_shares < intent.shares {
            intent.shares = final_shares;
        }

        intent
            .metadata
            .insert("kelly_fraction_raw".to_string(), raw_kelly.to_string());
        intent.metadata.insert(
            "kelly_fraction_multiplier".to_string(),
            self.config.kelly_fraction_multiplier.to_string(),
        );
        intent.metadata.insert(
            "kelly_fraction_effective".to_string(),
            effective_fraction.to_string(),
        );
        intent
            .metadata
            .insert("kelly_bankroll_usd".to_string(), bankroll.to_string());
        intent.metadata.insert(
            "kelly_target_notional_usd".to_string(),
            target_notional.to_string(),
        );
        intent
            .metadata
            .insert("kelly_sized_shares".to_string(), sized_shares.to_string());
        if final_shares != sized_shares {
            intent
                .metadata
                .insert("kelly_final_shares".to_string(), final_shares.to_string());
        }

        None
    }

    fn apply_min_order_constraints(
        &self,
        intent: &mut OrderIntent,
        strategy_max_shares: u64,
    ) -> Option<String> {
        if !intent.is_buy {
            return None;
        }
        // Never force-size emergency/critical intents.
        if intent.priority == OrderPriority::Critical {
            return None;
        }
        if intent.limit_price <= Decimal::ZERO {
            return None;
        }

        let min_shares_cfg = self.config.min_order_shares.max(1);
        let min_notional = self.config.min_order_notional_usd.max(Decimal::ZERO);

        let mut required_shares = min_shares_cfg;
        if min_notional > Decimal::ZERO {
            // Exchange enforces a minimum order notional for (marketable) buys.
            // We enforce it pre-submit to avoid deterministic 400s that trip the circuit breaker.
            let min_shares_for_notional = (min_notional / intent.limit_price)
                .ceil()
                .to_u64()
                .unwrap_or(u64::MAX);
            required_shares = required_shares.max(min_shares_for_notional);
        }

        if required_shares <= 1 {
            return None;
        }

        if required_shares > strategy_max_shares {
            return Some(format!(
                "venue minimum requires {} shares (min_shares={}, min_notional_usd={}) but strategy_max_shares={}",
                required_shares, min_shares_cfg, min_notional, strategy_max_shares
            ));
        }

        if intent.shares < required_shares {
            let before = intent.shares;
            intent.shares = required_shares;
            intent
                .metadata
                .insert("venue_min_order_applied".to_string(), "true".to_string());
            intent.metadata.insert(
                "venue_min_order_before_shares".to_string(),
                before.to_string(),
            );
            intent.metadata.insert(
                "venue_min_order_required_shares".to_string(),
                required_shares.to_string(),
            );
            intent.metadata.insert(
                "venue_min_order_min_shares".to_string(),
                min_shares_cfg.to_string(),
            );
            intent.metadata.insert(
                "venue_min_order_min_notional_usd".to_string(),
                min_notional.to_string(),
            );
        }

        None
    }

    async fn check_duplicate_intent(&self, intent: &OrderIntent) -> Option<String> {
        let mut guard = self.duplicate_guard.write().await;
        guard.register_or_block(intent, Utc::now())
    }

    async fn check_governance_policy(&self, intent: &OrderIntent) -> Option<String> {
        let policy = self.governance_policy.read().await.clone();
        let current_notional = self.current_account_notional().await;
        governance_block_reason(&policy, intent, current_notional)
    }

    async fn current_account_notional(&self) -> Decimal {
        let platform_exposure = self.risk_gate.total_exposure().await;

        let (crypto_open, crypto_pending) = {
            let allocator = self.crypto_allocator.read().await;
            (allocator.open.total, allocator.pending.total)
        };
        let (sports_open, sports_pending) = {
            let allocator = self.sports_allocator.read().await;
            (allocator.open.total, allocator.pending.total)
        };
        let (politics_open, politics_pending) = {
            let allocator = self.politics_allocator.read().await;
            (allocator.open.total, allocator.pending.total)
        };
        let (economics_open, economics_pending) = {
            let allocator = self.economics_allocator.read().await;
            (allocator.open.total, allocator.pending.total)
        };
        let other_pending_buy_notional = self
            .order_queue
            .read()
            .await
            .pending_buy_notional_excluding_domains(&[
                Domain::Crypto,
                Domain::Sports,
                Domain::Politics,
                Domain::Economics,
            ]);

        let allocator_open = crypto_open + sports_open + politics_open + economics_open;
        let open_notional = platform_exposure.max(allocator_open);
        let allocator_pending =
            crypto_pending + sports_pending + politics_pending + economics_pending;
        open_notional + allocator_pending + other_pending_buy_notional
    }

    async fn reserve_domain_capital(&self, intent: &OrderIntent) -> Option<String> {
        if !intent.is_buy {
            return None;
        }
        match intent.domain {
            Domain::Crypto => {
                let mut allocator = self.crypto_allocator.write().await;
                allocator.reserve_buy(intent).err()
            }
            Domain::Sports => {
                let mut allocator = self.sports_allocator.write().await;
                allocator.reserve_buy(intent).err()
            }
            Domain::Politics => {
                let mut allocator = self.politics_allocator.write().await;
                allocator.reserve_buy(intent).err()
            }
            Domain::Economics => {
                let mut allocator = self.economics_allocator.write().await;
                allocator.reserve_buy(intent).err()
            }
            _ => None,
        }
    }

    async fn release_domain_reservation(&self, intent_id: Uuid) {
        {
            let mut allocator = self.crypto_allocator.write().await;
            allocator.release_buy_reservation(intent_id);
        }
        {
            let mut allocator = self.sports_allocator.write().await;
            allocator.release_buy_reservation(intent_id);
        }
        {
            let mut allocator = self.politics_allocator.write().await;
            allocator.release_buy_reservation(intent_id);
        }
        let mut allocator = self.economics_allocator.write().await;
        allocator.release_buy_reservation(intent_id);
    }

    async fn settle_domain_success(
        &self,
        intent: &OrderIntent,
        filled_shares: u64,
        fill_price: Decimal,
    ) {
        match intent.domain {
            Domain::Crypto => {
                let mut allocator = self.crypto_allocator.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            Domain::Sports => {
                let mut allocator = self.sports_allocator.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            Domain::Politics => {
                let mut allocator = self.politics_allocator.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            Domain::Economics => {
                let mut allocator = self.economics_allocator.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            _ => {}
        }
    }

    async fn settle_domain_failure(&self, intent: &OrderIntent) {
        if !intent.is_buy {
            return;
        }
        match intent.domain {
            Domain::Crypto => {
                let mut allocator = self.crypto_allocator.write().await;
                allocator.release_buy_reservation(intent.intent_id);
            }
            Domain::Sports => {
                let mut allocator = self.sports_allocator.write().await;
                allocator.release_buy_reservation(intent.intent_id);
            }
            Domain::Politics => {
                let mut allocator = self.politics_allocator.write().await;
                allocator.release_buy_reservation(intent.intent_id);
            }
            Domain::Economics => {
                let mut allocator = self.economics_allocator.write().await;
                allocator.release_buy_reservation(intent.intent_id);
            }
            _ => {}
        }
    }

    /// Update agent snapshot in global state
    async fn handle_state_update(&self, snapshot: AgentSnapshot) {
        let agent_id = snapshot.agent_id.clone();

        // Store snapshot
        let mut state = self.global_state.write().await;
        state.agents.insert(agent_id, snapshot);
    }

    async fn refresh_risk_exposure_for_agent(&self, agent_id: &str) {
        // RiskGate exposure should be derived from executed positions, not agent self-reporting.
        let stats = self.positions.agent_stats(agent_id).await;
        self.risk_gate
            .update_agent_exposure(
                agent_id,
                stats.exposure,
                stats.unrealized_pnl,
                stats.position_count,
                stats.unhedged_count.min(u32::MAX as usize) as u32,
            )
            .await;
    }

    /// Drain the order queue and execute via OrderExecutor
    async fn drain_and_execute(&self) {
        let (expired, batch) = {
            let mut queue = self.order_queue.write().await;
            let expired = queue.cleanup_expired_intents();
            let batch = queue.dequeue_batch(self.config.batch_size);
            (expired, batch)
        };

        for intent in expired {
            self.settle_domain_failure(&intent).await;
        }

        if batch.is_empty() {
            return;
        }

        debug!(count = batch.len(), "draining order queue");

        for intent in batch {
            let agent_id = intent.agent_id.clone();
            let intent_id = intent.intent_id;
            let execute_started_at = Utc::now();
            let queue_delay_ms = execute_started_at
                .signed_duration_since(intent.created_at)
                .num_milliseconds()
                .max(0);

            // Convert OrderIntent → OrderRequest for the executor
            let request = self.intent_to_request(&intent);

            match self.executor.execute(&request).await {
                Ok(result) => {
                    info!(
                        %agent_id, %intent_id,
                        order_id = %result.order_id,
                        filled = result.filled_shares,
                        "order executed successfully"
                    );

                    self.persist_execution(
                        &intent,
                        &request,
                        Some(&result),
                        None,
                        Some(queue_delay_ms),
                    )
                    .await;

                    let fill_price = result.avg_fill_price.unwrap_or(intent.limit_price);
                    self.settle_domain_success(&intent, result.filled_shares, fill_price)
                        .await;

                    let mut realized_pnl = Decimal::ZERO;
                    if result.filled_shares > 0 {
                        if intent.is_buy {
                            let _ = self
                                .positions
                                .open_position(
                                    &agent_id,
                                    intent.domain.clone(),
                                    &intent.market_slug,
                                    &intent.token_id,
                                    intent.side.clone(),
                                    result.filled_shares,
                                    fill_price,
                                )
                                .await;
                        } else {
                            realized_pnl = self
                                .apply_sell_fill_to_positions(
                                    &intent,
                                    result.filled_shares,
                                    fill_price,
                                )
                                .await;
                        }

                        self.refresh_risk_exposure_for_agent(&agent_id).await;
                    }

                    // Record execution outcome with RiskGate (including realized PnL on exits).
                    // For binary options, PnL is realized on SELL fills (reduce/close).
                    if realized_pnl < Decimal::ZERO {
                        self.risk_gate
                            .record_success(&agent_id, Decimal::ZERO)
                            .await;
                        self.risk_gate
                            .record_loss(&agent_id, realized_pnl.abs())
                            .await;
                    } else {
                        self.risk_gate.record_success(&agent_id, realized_pnl).await;
                    }

                    // Record execution outcome with realized PnL attribution.
                    self.risk_gate.record_success(&agent_id, realized_pnl).await;
                }
                Err(e) => {
                    error!(
                        %agent_id, %intent_id,
                        error = %e,
                        "order execution failed"
                    );

                    self.persist_execution(
                        &intent,
                        &request,
                        None,
                        Some(e.to_string()),
                        Some(queue_delay_ms),
                    )
                    .await;

                    self.risk_gate
                        .record_failure(&agent_id, &e.to_string())
                        .await;

                    self.settle_domain_failure(&intent).await;
                }
            }
        }
    }

    async fn apply_sell_fill_to_positions(
        &self,
        intent: &OrderIntent,
        filled_shares: u64,
        exit_price: Decimal,
    ) -> Decimal {
        if filled_shares == 0 {
            return Decimal::ZERO;
        }

        let mut remaining = filled_shares;
        let mut realized_pnl = Decimal::ZERO;
        let mut matching_positions = self
            .positions
            .get_agent_positions(&intent.agent_id)
            .await
            .into_iter()
            .filter(|pos| {
                pos.domain == intent.domain
                    && pos.market_slug == intent.market_slug
                    && pos.token_id == intent.token_id
                    && pos.side == intent.side
            })
            .collect::<Vec<_>>();

        matching_positions.sort_by_key(|p| p.entry_time);

        for pos in matching_positions {
            if remaining == 0 {
                break;
            }
            let reduce_by = remaining.min(pos.shares);
            if let Some(pnl) = self
                .positions
                .reduce_position(&pos.position_id, reduce_by, exit_price)
                .await
            {
                realized_pnl += pnl;
            }
            remaining -= reduce_by;
        }

        if remaining > 0 {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                unmatched_shares = remaining,
                "sell fill exceeded tracked position shares; allocator adjusted, position book partially unmatched"
            );
        }

        realized_pnl
    }

    async fn persist_execution(
        &self,
        intent: &OrderIntent,
        request: &OrderRequest,
        result: Option<&crate::strategy::execution::executor::ExecutionResult>,
        error_message: Option<String>,
        queue_delay_ms: Option<i64>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let dry_run = self.executor.is_dry_run();

        let (order_id, status, filled_shares, avg_fill_price, elapsed_ms) = match result {
            Some(r) => (
                Some(r.order_id.clone()),
                format!("{:?}", r.status),
                r.filled_shares as i64,
                r.avg_fill_price,
                Some(r.elapsed_ms as i64),
            ),
            None => (
                None,
                format!("{:?}", crate::domain::OrderStatus::Failed),
                0,
                None,
                None,
            ),
        };

        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));
        let config_hash = intent.metadata.get("config_hash").cloned();

        let query = sqlx::query(
            r#"
            INSERT INTO agent_order_executions (
                account_id,
                agent_id,
                intent_id,
                domain,
                market_slug,
                token_id,
                market_side,
                is_buy,
                shares,
                limit_price,
                order_id,
                status,
                filled_shares,
                avg_fill_price,
                elapsed_ms,
                dry_run,
                error,
                intent_created_at,
                metadata
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19
            )
            ON CONFLICT (intent_id) DO UPDATE SET
                order_id = EXCLUDED.order_id,
                status = EXCLUDED.status,
                filled_shares = EXCLUDED.filled_shares,
                avg_fill_price = EXCLUDED.avg_fill_price,
                elapsed_ms = EXCLUDED.elapsed_ms,
                dry_run = EXCLUDED.dry_run,
                error = EXCLUDED.error,
                metadata = EXCLUDED.metadata,
                executed_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(&intent.agent_id)
        .bind(intent.intent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.side.as_str())
        .bind(intent.is_buy)
        .bind(intent.shares as i64)
        .bind(request.limit_price)
        .bind(order_id)
        .bind(status)
        .bind(filled_shares)
        .bind(avg_fill_price)
        .bind(elapsed_ms)
        .bind(dry_run)
        .bind(error_message.clone())
        .bind(intent.created_at)
        .bind(sqlx::types::Json(metadata));

        if let Err(e) = query.execute(pool).await {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist agent order execution"
            );
        }

        self.persist_execution_analysis(intent, request, result, queue_delay_ms, config_hash)
            .await;

        if !intent.is_buy {
            self.persist_exit_reason_execution(intent, result, error_message)
                .await;
        }
    }

    fn metadata_decimal(intent: &OrderIntent, key: &str) -> Option<Decimal> {
        intent
            .metadata
            .get(key)
            .and_then(|v| Decimal::from_str(v).ok())
    }

    async fn persist_signal_from_intent(&self, intent: &OrderIntent) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let strategy_id = intent
            .metadata
            .get("strategy")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let signal_type = intent
            .metadata
            .get("signal_type")
            .cloned()
            .unwrap_or_else(|| {
                if intent.is_buy {
                    "entry_intent".to_string()
                } else {
                    "exit_intent".to_string()
                }
            });
        let symbol = intent.metadata.get("symbol").cloned();
        let confidence = Self::metadata_decimal(intent, "signal_confidence");
        let momentum_value = Self::metadata_decimal(intent, "signal_momentum_value");
        let short_ma = Self::metadata_decimal(intent, "signal_short_ma");
        let long_ma = Self::metadata_decimal(intent, "signal_long_ma");
        let rolling_volatility = Self::metadata_decimal(intent, "signal_rolling_volatility");
        let fair_value = Self::metadata_decimal(intent, "signal_fair_value");
        let market_price = Self::metadata_decimal(intent, "signal_market_price");
        let edge = Self::metadata_decimal(intent, "signal_edge");
        let config_hash = intent.metadata.get("config_hash").cloned();
        let context =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO signal_history (
                account_id, intent_id, agent_id, strategy_id, domain, signal_type, market_slug, token_id,
                symbol, side, confidence, momentum_value, short_ma, long_ma, rolling_volatility,
                fair_value, market_price, edge, config_hash, context
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,
                $9,$10,$11,$12,$13,$14,$15,
                $16,$17,$18,$19,$20
            )
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(&strategy_id)
        .bind(intent.domain.to_string())
        .bind(&signal_type)
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(symbol)
        .bind(intent.side.as_str())
        .bind(confidence)
        .bind(momentum_value)
        .bind(short_ma)
        .bind(long_ma)
        .bind(rolling_volatility)
        .bind(fair_value)
        .bind(market_price)
        .bind(edge)
        .bind(config_hash)
        .bind(sqlx::types::Json(context))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist signal history"
            );
        }
    }

    async fn persist_risk_decision(
        &self,
        intent: &OrderIntent,
        decision: &str,
        block_reason: Option<String>,
        adjusted: Option<(u64, String)>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let (suggestion_max_shares, suggestion_reason) = adjusted
            .map(|(shares, reason)| (Some(shares as i64), Some(reason)))
            .unwrap_or((None, None));
        let config_hash = intent.metadata.get("config_hash").cloned();
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO risk_gate_decisions (
                account_id, intent_id, agent_id, domain, decision, block_reason, suggestion_max_shares,
                suggestion_reason, notional_value, config_hash, metadata
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
            ON CONFLICT (intent_id) DO UPDATE SET
                decision = EXCLUDED.decision,
                block_reason = EXCLUDED.block_reason,
                suggestion_max_shares = EXCLUDED.suggestion_max_shares,
                suggestion_reason = EXCLUDED.suggestion_reason,
                notional_value = EXCLUDED.notional_value,
                config_hash = EXCLUDED.config_hash,
                metadata = EXCLUDED.metadata,
                decided_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(decision)
        .bind(block_reason)
        .bind(suggestion_max_shares)
        .bind(suggestion_reason)
        .bind(intent.notional_value())
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist risk gate decision"
            );
        }
    }

    async fn persist_exit_reason_intent(&self, intent: &OrderIntent) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let reason_code = intent
            .metadata
            .get("exit_reason")
            .or_else(|| intent.metadata.get("reason_code"))
            .cloned()
            .unwrap_or_else(|| "UNKNOWN".to_string());
        let reason_detail = intent.metadata.get("exit_detail").cloned();
        let entry_price = Self::metadata_decimal(intent, "entry_price");
        let pnl_pct = Self::metadata_decimal(intent, "pnl_pct");
        let config_hash = intent.metadata.get("config_hash").cloned();
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO exit_reasons (
                account_id, intent_id, agent_id, domain, market_slug, token_id, market_side, reason_code,
                reason_detail, entry_price, pnl_pct, status, config_hash, metadata
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'INTENT_SUBMITTED',$12,$13)
            ON CONFLICT (intent_id) DO UPDATE SET
                reason_code = EXCLUDED.reason_code,
                reason_detail = EXCLUDED.reason_detail,
                entry_price = COALESCE(EXCLUDED.entry_price, exit_reasons.entry_price),
                pnl_pct = COALESCE(EXCLUDED.pnl_pct, exit_reasons.pnl_pct),
                status = EXCLUDED.status,
                config_hash = EXCLUDED.config_hash,
                metadata = EXCLUDED.metadata,
                updated_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.side.as_str())
        .bind(reason_code)
        .bind(reason_detail)
        .bind(entry_price)
        .bind(pnl_pct)
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist exit reason intent"
            );
        }
    }

    async fn persist_exit_reason_execution(
        &self,
        intent: &OrderIntent,
        result: Option<&crate::strategy::execution::executor::ExecutionResult>,
        error_message: Option<String>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let executed_price = result.and_then(|r| r.avg_fill_price);
        let status = result
            .map(|r| format!("{:?}", r.status))
            .unwrap_or_else(|| "Failed".to_string());
        let reason_detail = error_message.or_else(|| {
            intent
                .metadata
                .get("exit_detail")
                .cloned()
                .or_else(|| intent.metadata.get("error").cloned())
        });
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO exit_reasons (
                account_id, intent_id, agent_id, domain, market_slug, token_id, market_side, reason_code,
                reason_detail, entry_price, exit_price, pnl_pct, status, config_hash, metadata
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,
                $9,$10,$11,$12,$13,$14,$15
            )
            ON CONFLICT (intent_id) DO UPDATE SET
                reason_detail = COALESCE(EXCLUDED.reason_detail, exit_reasons.reason_detail),
                exit_price = COALESCE(EXCLUDED.exit_price, exit_reasons.exit_price),
                pnl_pct = COALESCE(EXCLUDED.pnl_pct, exit_reasons.pnl_pct),
                status = EXCLUDED.status,
                config_hash = COALESCE(EXCLUDED.config_hash, exit_reasons.config_hash),
                metadata = EXCLUDED.metadata,
                updated_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.side.as_str())
        .bind(
            intent
                .metadata
                .get("exit_reason")
                .or_else(|| intent.metadata.get("reason_code"))
                .cloned()
                .unwrap_or_else(|| "UNKNOWN".to_string()),
        )
        .bind(reason_detail)
        .bind(Self::metadata_decimal(intent, "entry_price"))
        .bind(executed_price)
        .bind(Self::metadata_decimal(intent, "pnl_pct"))
        .bind(status)
        .bind(intent.metadata.get("config_hash").cloned())
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist exit reason execution"
            );
        }
    }

    async fn persist_execution_analysis(
        &self,
        intent: &OrderIntent,
        request: &OrderRequest,
        execution_result: Option<&crate::strategy::execution::executor::ExecutionResult>,
        queue_delay_ms: Option<i64>,
        config_hash: Option<String>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let expected_price = request.limit_price;
        let executed_price = execution_result.and_then(|r| r.avg_fill_price);
        let execution_latency_ms = execution_result.map(|r| r.elapsed_ms as i64);
        let total_latency_ms = match (queue_delay_ms, execution_latency_ms) {
            (Some(q), Some(e)) => Some(q + e),
            (Some(q), None) => Some(q),
            (None, Some(e)) => Some(e),
            (None, None) => None,
        };

        let actual_slippage_bps = executed_price.and_then(|fill| {
            if expected_price.is_zero() {
                return None;
            }
            let signed = if intent.is_buy {
                (fill - expected_price) / expected_price
            } else {
                (expected_price - fill) / expected_price
            };
            Some(signed * Decimal::from(10_000))
        });

        let expected_slippage_bps = Self::metadata_decimal(intent, "expected_slippage_bps")
            .or_else(|| Self::metadata_decimal(intent, "signal_expected_slippage_bps"));
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));
        let status = execution_result
            .map(|r| format!("{:?}", r.status))
            .unwrap_or_else(|| "Failed".to_string());

        let persist_result = sqlx::query(
            r#"
            INSERT INTO execution_analysis (
                account_id, intent_id, agent_id, domain, market_slug, token_id, is_buy,
                expected_price, executed_price, expected_slippage_bps, actual_slippage_bps,
                queue_delay_ms, execution_latency_ms, total_latency_ms,
                status, dry_run, config_hash, metadata
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,
                $8,$9,$10,$11,
                $12,$13,$14,
                $15,$16,$17,$18
            )
            ON CONFLICT (intent_id) DO UPDATE SET
                executed_price = EXCLUDED.executed_price,
                expected_slippage_bps = EXCLUDED.expected_slippage_bps,
                actual_slippage_bps = EXCLUDED.actual_slippage_bps,
                queue_delay_ms = EXCLUDED.queue_delay_ms,
                execution_latency_ms = EXCLUDED.execution_latency_ms,
                total_latency_ms = EXCLUDED.total_latency_ms,
                status = EXCLUDED.status,
                dry_run = EXCLUDED.dry_run,
                config_hash = EXCLUDED.config_hash,
                metadata = EXCLUDED.metadata,
                recorded_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.is_buy)
        .bind(expected_price)
        .bind(executed_price)
        .bind(expected_slippage_bps)
        .bind(actual_slippage_bps)
        .bind(queue_delay_ms)
        .bind(execution_latency_ms)
        .bind(total_latency_ms)
        .bind(status)
        .bind(self.executor.is_dry_run())
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = persist_result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist execution analysis"
            );
        }

        self.persist_live_strategy_evaluation(
            intent,
            request,
            execution_result,
            expected_slippage_bps,
            actual_slippage_bps,
            total_latency_ms,
        )
        .await;
    }

    async fn persist_live_strategy_evaluation(
        &self,
        intent: &OrderIntent,
        request: &OrderRequest,
        execution_result: Option<&crate::strategy::execution::executor::ExecutionResult>,
        expected_slippage_bps: Option<Decimal>,
        actual_slippage_bps: Option<Decimal>,
        total_latency_ms: Option<i64>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let strategy_id = intent
            .metadata
            .get("strategy")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let deployment_id = intent
            .metadata
            .get("deployment_id")
            .map(String::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let timeframe = intent
            .metadata
            .get("timeframe")
            .map(String::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let score = Self::metadata_decimal(intent, "signal_confidence")
            .or_else(|| Self::metadata_decimal(intent, "signal_edge"));

        let status = match execution_result {
            Some(result) => match result.status {
                crate::domain::OrderStatus::Submitted
                | crate::domain::OrderStatus::PartiallyFilled
                | crate::domain::OrderStatus::Filled => "PASS",
                crate::domain::OrderStatus::Cancelled => "WARN",
                crate::domain::OrderStatus::Pending
                | crate::domain::OrderStatus::Rejected
                | crate::domain::OrderStatus::Expired
                | crate::domain::OrderStatus::Failed => "FAIL",
            },
            None => "FAIL",
        };

        let evidence_hash = intent.intent_id.to_string();
        let evidence_payload = serde_json::json!({
            "intent_id": intent.intent_id.to_string(),
            "agent_id": intent.agent_id.clone(),
            "is_buy": intent.is_buy,
            "shares": intent.shares,
            "request_limit_price": request.limit_price.to_string(),
            "order_side": request.order_side.to_string(),
            "expected_slippage_bps": expected_slippage_bps.map(|v| v.to_string()),
            "actual_slippage_bps": actual_slippage_bps.map(|v| v.to_string()),
            "total_latency_ms": total_latency_ms,
            "dry_run": self.executor.is_dry_run(),
            "execution": execution_result.map(|r| serde_json::json!({
                "order_id": r.order_id.clone(),
                "status": format!("{:?}", r.status),
                "filled_shares": r.filled_shares,
                "avg_fill_price": r.avg_fill_price.map(|p| p.to_string()),
                "elapsed_ms": r.elapsed_ms
            })),
        });
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let insert = sqlx::query(
            r#"
            INSERT INTO strategy_evaluations (
                account_id,
                strategy_id,
                deployment_id,
                domain,
                stage,
                status,
                score,
                timeframe,
                sample_size,
                evidence_kind,
                evidence_ref,
                evidence_hash,
                evidence_payload,
                metadata
            )
            VALUES (
                $1,$2,$3,$4,'LIVE',$5,$6,$7,1,
                'execution_analysis',$8,$9,$10,$11
            )
            ON CONFLICT (account_id, strategy_id, stage, evidence_hash) DO NOTHING
            "#,
        )
        .bind(&self.account_id)
        .bind(strategy_id)
        .bind(deployment_id)
        .bind(intent.domain.to_string())
        .bind(status)
        .bind(score)
        .bind(timeframe)
        .bind(intent.intent_id.to_string())
        .bind(evidence_hash)
        .bind(sqlx::types::Json(evidence_payload))
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = insert {
            warn!(
                account_id = %self.account_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist live strategy evaluation evidence"
            );
        }
    }

    async fn persist_risk_runtime_state(&self) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let risk_state = self.risk_gate.state().await;
        let (daily_pnl, _, _) = self.risk_gate.daily_stats().await;
        let daily_loss_limit = self.risk_gate.daily_loss_limit();
        let drawdown = self.risk_gate.drawdown_snapshot().await;
        let daily_date = Utc::now().date_naive();

        let result = sqlx::query(
            r#"
            INSERT INTO risk_runtime_state (
                account_id,
                risk_state,
                daily_date,
                daily_pnl,
                daily_loss_limit,
                current_equity,
                equity_peak,
                current_drawdown,
                max_drawdown_observed
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
            ON CONFLICT (account_id) DO UPDATE SET
                risk_state = EXCLUDED.risk_state,
                daily_date = EXCLUDED.daily_date,
                daily_pnl = EXCLUDED.daily_pnl,
                daily_loss_limit = EXCLUDED.daily_loss_limit,
                current_equity = EXCLUDED.current_equity,
                equity_peak = EXCLUDED.equity_peak,
                current_drawdown = EXCLUDED.current_drawdown,
                max_drawdown_observed = EXCLUDED.max_drawdown_observed,
                updated_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(format!("{:?}", risk_state))
        .bind(daily_date)
        .bind(daily_pnl)
        .bind(daily_loss_limit)
        .bind(drawdown.current_equity)
        .bind(drawdown.equity_peak)
        .bind(drawdown.current_drawdown)
        .bind(drawdown.max_drawdown_observed)
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                account_id = %self.account_id,
                error = %e,
                "failed to persist risk runtime state"
            );
        }
    }

    /// Refresh GlobalState from aggregators
    async fn refresh_global_state(&self) {
        let portfolio = self.positions.aggregate().await;
        let positions = self.positions.all_positions().await;
        let risk_state = self.risk_gate.state().await;
        let (daily_pnl, _, _) = self.risk_gate.daily_stats().await;
        let daily_loss_limit = self.risk_gate.daily_loss_limit();
        let (current_drawdown, max_drawdown_observed) = self.risk_gate.drawdown_stats().await;
        let max_drawdown_limit = self.risk_gate.max_drawdown_limit();
        let mut circuit_breaker_events = self.risk_gate.circuit_breaker_events().await;
        // Retain only the most recent events to prevent unbounded memory growth in long-running deployments.
        const MAX_CB_EVENTS: usize = 500;
        if circuit_breaker_events.len() > MAX_CB_EVENTS {
            circuit_breaker_events.drain(..circuit_breaker_events.len() - MAX_CB_EVENTS);
        }
        let queue_stats = self.order_queue.read().await.stats();
        let total_realized = self.positions.total_realized_pnl().await;

        let mut state = self.global_state.write().await;
        state.portfolio = portfolio;
        state.positions = positions;
        state.risk_state = risk_state;
        state.daily_pnl = daily_pnl;
        state.daily_loss_limit = daily_loss_limit;
        state.current_drawdown = current_drawdown;
        state.max_drawdown_observed = max_drawdown_observed;
        state.max_drawdown_limit = max_drawdown_limit;
        state.circuit_breaker_events = circuit_breaker_events;
        state.queue_stats = QueueStatsSnapshot::from(queue_stats);
        state.total_realized_pnl = total_realized;
        state.last_refresh = Utc::now();

        // Check for stale agents
        let timeout = chrono::Duration::milliseconds(self.config.heartbeat_timeout_ms as i64);
        let stale_warn_cooldown =
            chrono::Duration::seconds(self.config.heartbeat_stale_warn_cooldown_secs as i64);
        let now = Utc::now();
        let mut stale_warn_at = self.stale_heartbeat_warn_at.write().await;
        for (id, agent) in state.agents.iter_mut() {
            if now - agent.last_heartbeat > timeout
                && matches!(agent.status, crate::platform::AgentStatus::Running)
            {
                let should_warn = stale_warn_at
                    .get(id)
                    .map(|last_warned_at| now - *last_warned_at >= stale_warn_cooldown)
                    .unwrap_or(true);
                if should_warn {
                    warn!(
                        agent_id = %id,
                        last_heartbeat = %agent.last_heartbeat,
                        stale_ms = (now - agent.last_heartbeat).num_milliseconds(),
                        timeout_ms = self.config.heartbeat_timeout_ms,
                        "agent heartbeat stale"
                    );
                    stale_warn_at.insert(id.clone(), now);
                }
                agent.error_message = Some("heartbeat timeout".into());
            }
        }
        drop(stale_warn_at);
        drop(state);

        self.persist_risk_runtime_state().await;
    }

    fn infer_time_bucket_seconds(intent: &OrderIntent) -> i64 {
        if let Some(raw) = intent.metadata.get("event_window_secs") {
            if let Ok(v) = raw.trim().parse::<i64>() {
                if v > 0 {
                    return v;
                }
            }
        }

        let mut hints: Vec<&str> = Vec::new();
        if let Some(h) = intent.metadata.get("timeframe") {
            hints.push(h.as_str());
        }
        if let Some(h) = intent.metadata.get("horizon") {
            hints.push(h.as_str());
        }
        if let Some(h) = intent.metadata.get("series_id") {
            hints.push(h.as_str());
        }

        for raw in hints {
            if let Some(horizon) = CryptoHorizon::from_hint(raw) {
                return match horizon {
                    CryptoHorizon::M15 => 15 * 60,
                    CryptoHorizon::M5 => 5 * 60,
                    CryptoHorizon::Other => 5 * 60,
                };
            }
        }

        5 * 60
    }

    fn sanitize_idempotency_component(input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        for ch in input.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':' | '.' | '|') {
                out.push(ch);
            } else {
                out.push('_');
            }
        }
        out
    }

    fn stable_idempotency_key(
        account_id: &str,
        intent: &OrderIntent,
        default_scope: DuplicateGuardScope,
    ) -> String {
        if let Some(key) = intent
            .metadata
            .get("idempotency_key")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
        {
            return Self::sanitize_idempotency_component(key);
        }

        // Align idempotency with duplicate-guard semantics.
        // - When the guard is market-scoped, avoid including deployment_id in the key so
        //   cross-deployment duplicate intents resolve to the same idempotency key.
        // - When deployment-scoped, key includes deployment_id.
        let scope = match intent
            .metadata
            .get("duplicate_guard_scope")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("deployment") | Some("dep") => DuplicateGuardScope::Deployment,
            Some("market") | Some("global") => DuplicateGuardScope::Market,
            _ => default_scope,
        };
        let scope_label = match scope {
            DuplicateGuardScope::Market => "market",
            DuplicateGuardScope::Deployment => "deployment",
        };
        let dep_label = match scope {
            DuplicateGuardScope::Market => "market".to_string(),
            DuplicateGuardScope::Deployment => IntentDuplicateGuard::deployment_scope(intent),
        };

        let window_secs = Self::infer_time_bucket_seconds(intent);
        let ts = intent
            .metadata
            .get("event_time")
            .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(intent.created_at)
            .timestamp();
        let bucket = ts.div_euclid(window_secs);
        let side = intent.side.as_str();
        let order_kind = if intent.is_buy { "buy" } else { "sell" };

        Self::sanitize_idempotency_component(&format!(
            "acct:{account}|scope:{scope}|dep:{dep}|dom:{dom}|mkt:{mkt}|side:{side}|kind:{kind}|bucket:{bucket}",
            account = account_id,
            scope = scope_label,
            dep = dep_label,
            dom = intent.domain.to_string().to_ascii_lowercase(),
            mkt = intent_market_identity(intent),
            side = side.to_ascii_lowercase(),
            kind = order_kind,
            bucket = bucket,
        ))
    }

    /// Convert an OrderIntent into an OrderRequest for the executor
    fn intent_to_request(&self, intent: &OrderIntent) -> OrderRequest {
        use crate::domain::OrderSide;

        let order_side = if intent.is_buy {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };

        let idempotency_key = Self::stable_idempotency_key(
            &self.account_id,
            intent,
            self.config.duplicate_guard_scope,
        );
        OrderRequest {
            client_order_id: format!("intent:{}", intent.intent_id),
            idempotency_key: Some(idempotency_key),
            token_id: intent.token_id.clone(),
            market_side: intent.side.clone(),
            order_side,
            shares: intent.shares,
            limit_price: intent.limit_price,
            order_type: crate::domain::OrderType::Limit,
            time_in_force: crate::domain::TimeInForce::GTC,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::PolymarketClient;
    use crate::config::ExecutionConfig;
    use crate::platform::{
        AgentStatus, DeploymentExecutionMode, DeploymentState, Domain, IntentPurpose,
        MarketSelector, OrderPriority, QueueStats, StrategyDeployment, StrategyLifecycleStage,
        StrategyProductType, Timeframe,
    };
    use crate::strategy::execution::executor::OrderExecutor;
    use rust_decimal_macros::dec;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn mock_snapshot(agent_id: &str) -> AgentSnapshot {
        AgentSnapshot {
            agent_id: agent_id.into(),
            name: agent_id.into(),
            domain: Domain::Crypto,
            status: AgentStatus::Running,
            position_count: 1,
            exposure: dec!(100),
            daily_pnl: dec!(5),
            unrealized_pnl: dec!(2),
            metrics: HashMap::new(),
            last_heartbeat: Utc::now(),
            error_message: None,
        }
    }

    fn make_test_handle() -> (CoordinatorHandle, Coordinator) {
        let client = PolymarketClient::new("https://clob.polymarket.com", true)
            .expect("build dry-run polymarket client");
        let executor = Arc::new(OrderExecutor::new(client, ExecutionConfig::default()));
        let allowed_domains = HashSet::from([Domain::Crypto, Domain::Sports]);
        let coordinator = Coordinator::new(
            CoordinatorConfig::default(),
            executor,
            "acct-test".to_string(),
            allowed_domains,
        );
        let handle = coordinator.handle();
        (handle, coordinator)
    }

    #[tokio::test]
    async fn test_handle_pause_and_resume_agent_enqueue_control_commands() {
        let (handle, mut coordinator) = make_test_handle();

        handle
            .pause_agent("openclaw")
            .await
            .expect("pause agent command accepted");
        assert!(matches!(
            coordinator.control_rx.try_recv(),
            Ok(CoordinatorControlCommand::PauseAgent(agent_id)) if agent_id == "openclaw"
        ));

        handle
            .resume_agent("openclaw")
            .await
            .expect("resume agent command accepted");
        assert!(matches!(
            coordinator.control_rx.try_recv(),
            Ok(CoordinatorControlCommand::ResumeAgent(agent_id)) if agent_id == "openclaw"
        ));
    }

    #[tokio::test]
    async fn test_handle_read_state_and_governance_policy_round_trip() {
        let (handle, _coordinator) = make_test_handle();

        let state = handle.read_state().await;
        assert_eq!(state.active_agent_count(), 0);

        let snapshot = handle
            .update_governance_policy(GovernancePolicyUpdate {
                block_new_intents: true,
                blocked_domains: vec!["sports".to_string()],
                max_intent_notional_usd: Some(dec!(5)),
                max_total_notional_usd: Some(dec!(25)),
                updated_by: "openclaw".to_string(),
                reason: Some("risk_off".to_string()),
                metadata: HashMap::from([("mode".to_string(), "risk_off".to_string())]),
            })
            .await
            .expect("update governance policy");

        assert!(snapshot.block_new_intents);
        assert_eq!(snapshot.updated_by, "openclaw");
        assert_eq!(snapshot.blocked_domains, vec!["sports".to_string()]);
        assert_eq!(snapshot.max_intent_notional_usd, Some(dec!(5)));
        assert_eq!(
            snapshot.metadata.get("mode").map(String::as_str),
            Some("risk_off")
        );

        let readback = handle.governance_policy().await;
        assert!(readback.block_new_intents);
        assert_eq!(readback.max_total_notional_usd, Some(dec!(25)));
        assert_eq!(
            readback.metadata.get("mode").map(String::as_str),
            Some("risk_off")
        );
    }

    #[test]
    fn test_global_state_defaults() {
        let state = GlobalState::new();
        assert_eq!(state.active_agent_count(), 0);
        assert_eq!(state.total_exposure(), Decimal::ZERO);
        assert_eq!(state.total_unrealized_pnl(), Decimal::ZERO);
    }

    #[test]
    fn test_global_state_active_count() {
        let mut state = GlobalState::new();
        state.agents.insert("a".into(), mock_snapshot("a"));
        state.agents.insert("b".into(), {
            let mut s = mock_snapshot("b");
            s.status = AgentStatus::Paused;
            s
        });
        assert_eq!(state.active_agent_count(), 1);
    }

    #[test]
    fn test_queue_stats_snapshot_from() {
        let qs = QueueStats {
            current_size: 5,
            max_size: 100,
            enqueued_total: 50,
            dequeued_total: 45,
            expired_total: 3,
            critical_count: 1,
            high_count: 2,
            normal_count: 1,
            low_count: 1,
        };
        let snap = QueueStatsSnapshot::from(qs);
        assert_eq!(snap.current_size, 5);
        assert_eq!(snap.enqueued_total, 50);
    }

    fn make_intent(is_buy: bool, priority: OrderPriority) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "crypto_lob_ml",
            Domain::Crypto,
            "btc-updown-5m-123",
            "token-up-123",
            crate::domain::Side::Up,
            is_buy,
            100,
            dec!(0.42),
        );
        intent.priority = priority;
        intent
    }

    fn make_deployment(
        id: &str,
        strategy: &str,
        domain: Domain,
        timeframe: Timeframe,
        execution_mode: DeploymentExecutionMode,
        state: DeploymentState,
    ) -> StrategyDeployment {
        StrategyDeployment {
            id: id.to_string(),
            strategy: strategy.to_string(),
            strategy_version: "test".to_string(),
            domain,
            market_selector: MarketSelector::Dynamic {
                domain,
                query: None,
                min_liquidity_usd: None,
                max_spread_bps: None,
                min_time_remaining_secs: None,
                max_time_remaining_secs: None,
            },
            timeframe,
            enabled: true,
            state,
            allocator_profile: "default".to_string(),
            risk_profile: "default".to_string(),
            priority: 50,
            cooldown_secs: 60,
            account_ids: Vec::new(),
            execution_mode,
            lifecycle_stage: StrategyLifecycleStage::Live,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: None,
            last_evaluation_score: None,
        }
    }

    #[tokio::test]
    async fn test_submit_order_accepts_entry_for_enabled_deployment() {
        let (handle, _coordinator) = make_test_handle();
        let deployment = make_deployment(
            "deploy.crypto.enabled",
            "momentum",
            Domain::Crypto,
            Timeframe::M5,
            DeploymentExecutionMode::Any,
            DeploymentState::Enabled,
        );
        handle
            .shared_deployments()
            .write()
            .await
            .insert(deployment.id.clone(), deployment.clone());

        let intent = make_intent(true, OrderPriority::Normal)
            .with_deployment_id(deployment.id.clone())
            .with_purpose(IntentPurpose::Entry);

        handle
            .submit_order(intent)
            .await
            .expect("enabled entry accepted");
    }

    #[tokio::test]
    async fn test_submit_order_rejects_entry_for_draining_deployment() {
        let (handle, _coordinator) = make_test_handle();
        let deployment = make_deployment(
            "deploy.crypto.draining",
            "momentum",
            Domain::Crypto,
            Timeframe::M5,
            DeploymentExecutionMode::Any,
            DeploymentState::Draining,
        );
        handle
            .shared_deployments()
            .write()
            .await
            .insert(deployment.id.clone(), deployment.clone());

        let intent = make_intent(true, OrderPriority::Normal)
            .with_deployment_id(deployment.id.clone())
            .with_purpose(IntentPurpose::Entry);

        let err = handle
            .submit_order(intent)
            .await
            .expect_err("draining deployment blocks entry");
        assert!(err.to_string().contains("draining"));
    }

    #[tokio::test]
    async fn test_submit_order_allows_cancel_for_draining_deployment() {
        let (handle, _coordinator) = make_test_handle();
        let deployment = make_deployment(
            "deploy.crypto.draining.cancel",
            "momentum",
            Domain::Crypto,
            Timeframe::M5,
            DeploymentExecutionMode::Any,
            DeploymentState::Draining,
        );
        handle
            .shared_deployments()
            .write()
            .await
            .insert(deployment.id.clone(), deployment.clone());

        let intent = make_intent(true, OrderPriority::Normal)
            .with_deployment_id(deployment.id.clone())
            .with_purpose(IntentPurpose::Cancel);

        handle
            .submit_order(intent)
            .await
            .expect("draining deployment allows cancel-purpose intent");
    }

    #[test]
    fn test_buy_intent_requires_deployment_id_metadata() {
        let intent = make_intent(true, OrderPriority::Normal);
        let reason = buy_intent_missing_deployment_reason(&intent);
        assert_eq!(
            reason.as_deref(),
            Some("BUY intent missing required metadata field 'deployment_id'")
        );
    }

    #[test]
    fn test_sell_intent_does_not_require_deployment_id_metadata() {
        let intent = make_intent(false, OrderPriority::Normal);
        assert!(buy_intent_missing_deployment_reason(&intent).is_none());
    }

    #[test]
    fn test_sell_reduce_only_violation_when_no_tracked_shares() {
        let intent = make_intent(false, OrderPriority::Normal);
        let reason = sell_reduce_only_violation_reason(&intent, 0, 0);
        assert!(reason
            .unwrap_or_default()
            .contains("no tracked open shares"));
    }

    #[test]
    fn test_sell_reduce_only_violation_when_requested_exceeds_tracked() {
        let intent = make_intent(false, OrderPriority::Normal);
        let reason = sell_reduce_only_violation_reason(&intent, 30, 0);
        assert!(reason
            .unwrap_or_default()
            .contains("requested shares 100 exceeds available reduce-only shares 30"));
    }

    #[test]
    fn test_sell_reduce_only_allows_with_sufficient_tracked_shares() {
        let intent = make_intent(false, OrderPriority::Normal);
        assert!(sell_reduce_only_violation_reason(&intent, 100, 0).is_none());
        assert!(sell_reduce_only_violation_reason(&intent, 150, 0).is_none());
    }

    #[test]
    fn test_sell_reduce_only_violation_when_pending_sells_exhaust_available() {
        let intent = make_intent(false, OrderPriority::Normal);
        let reason = sell_reduce_only_violation_reason(&intent, 100, 100);
        assert!(reason
            .unwrap_or_default()
            .contains("fully reserved by pending SELL intents 100"));
    }

    #[test]
    fn test_sell_reduce_only_violation_when_requested_exceeds_available_after_pending() {
        let intent = make_intent(false, OrderPriority::Normal);
        let reason = sell_reduce_only_violation_reason(&intent, 100, 40);
        assert!(reason
            .unwrap_or_default()
            .contains("requested shares 100 exceeds available reduce-only shares 60"));
    }

    #[test]
    fn test_duplicate_guard_blocks_repeated_buy_within_window() {
        let mut guard = IntentDuplicateGuard::new(1000, true, DuplicateGuardScope::Market);
        let now = Utc::now();
        let intent = make_intent(true, OrderPriority::Normal);

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(300))
            .is_some());
    }

    #[test]
    fn test_duplicate_guard_allows_after_window() {
        let mut guard = IntentDuplicateGuard::new(500, true, DuplicateGuardScope::Market);
        let now = Utc::now();
        let intent = make_intent(true, OrderPriority::Normal);

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(700))
            .is_none());
    }

    #[test]
    fn test_duplicate_guard_blocks_same_market_even_if_token_differs() {
        let mut guard = IntentDuplicateGuard::new(1_000, true, DuplicateGuardScope::Market);
        let now = Utc::now();
        let first = make_intent(true, OrderPriority::Normal);
        let mut second = make_intent(true, OrderPriority::Normal);
        second.token_id = "token-down-123".to_string();
        second.side = crate::domain::Side::Down;

        assert!(guard.register_or_block(&first, now).is_none());
        assert!(guard
            .register_or_block(&second, now + chrono::Duration::milliseconds(100))
            .is_some());
    }

    #[test]
    fn test_duplicate_guard_blocks_same_condition_with_different_slugs() {
        let mut guard = IntentDuplicateGuard::new(1_000, true, DuplicateGuardScope::Market);
        let now = Utc::now();
        let mut first = make_intent(true, OrderPriority::Normal);
        let mut second = make_intent(true, OrderPriority::Normal);
        first.market_slug = "slug-a".to_string();
        second.market_slug = "slug-b".to_string();
        first.metadata.insert(
            "condition_id".to_string(),
            "0xABCD00000000000000000000000000000000000000000000000000000000".to_string(),
        );
        second.metadata.insert(
            "condition_id".to_string(),
            "0xabcd00000000000000000000000000000000000000000000000000000000".to_string(),
        );

        assert!(guard.register_or_block(&first, now).is_none());
        assert!(guard
            .register_or_block(&second, now + chrono::Duration::milliseconds(100))
            .is_some());
    }

    #[test]
    fn test_duplicate_guard_allows_same_market_for_different_deployments() {
        let mut guard = IntentDuplicateGuard::new(1_000, true, DuplicateGuardScope::Deployment);
        let now = Utc::now();
        let mut first = make_intent(true, OrderPriority::Normal);
        let mut second = make_intent(true, OrderPriority::Normal);

        first.metadata.insert(
            "deployment_id".to_string(),
            "crypto.pm.btc.15m.momentum".to_string(),
        );
        second.metadata.insert(
            "deployment_id".to_string(),
            "crypto.pm.btc.15m.patternmem".to_string(),
        );

        assert!(guard.register_or_block(&first, now).is_none());
        assert!(guard
            .register_or_block(&second, now + chrono::Duration::milliseconds(100))
            .is_none());
    }

    #[test]
    fn test_duplicate_guard_blocks_same_market_for_different_deployments_in_market_scope() {
        let mut guard = IntentDuplicateGuard::new(1_000, true, DuplicateGuardScope::Market);
        let now = Utc::now();
        let mut first = make_intent(true, OrderPriority::Normal);
        let mut second = make_intent(true, OrderPriority::Normal);

        first.metadata.insert(
            "deployment_id".to_string(),
            "crypto.pm.btc.15m.momentum".to_string(),
        );
        second.metadata.insert(
            "deployment_id".to_string(),
            "crypto.pm.btc.15m.patternmem".to_string(),
        );

        assert!(guard.register_or_block(&first, now).is_none());
        assert!(guard
            .register_or_block(&second, now + chrono::Duration::milliseconds(100))
            .is_some());
    }

    #[test]
    fn test_duplicate_guard_does_not_block_sells() {
        let mut guard = IntentDuplicateGuard::new(10_000, true, DuplicateGuardScope::Market);
        let now = Utc::now();
        let intent = make_intent(false, OrderPriority::Normal);

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(10))
            .is_none());
    }

    #[test]
    fn test_duplicate_guard_skips_critical_orders() {
        let mut guard = IntentDuplicateGuard::new(10_000, true, DuplicateGuardScope::Market);
        let now = Utc::now();
        let intent = make_intent(true, OrderPriority::Critical);

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(10))
            .is_none());
    }

    #[test]
    fn test_deployment_gate_blocks_live_buy_without_strategy_metadata() {
        let mut deployments = HashMap::new();
        deployments.insert(
            "crypto-momentum-15m".to_string(),
            make_deployment(
                "crypto-momentum-15m",
                "momentum",
                Domain::Crypto,
                Timeframe::M15,
                DeploymentExecutionMode::LiveOnly,
                DeploymentState::Enabled,
            ),
        );

        let mut intent = make_intent(true, OrderPriority::Normal);
        let result = Coordinator::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("strategy metadata is required"));
    }

    #[test]
    fn test_deployment_gate_accepts_explicit_deployment_and_applies_metadata() {
        let mut deployments = HashMap::new();
        deployments.insert(
            "crypto-momentum-15m".to_string(),
            make_deployment(
                "crypto-momentum-15m",
                "momentum",
                Domain::Crypto,
                Timeframe::M15,
                DeploymentExecutionMode::LiveOnly,
                DeploymentState::Enabled,
            ),
        );

        let mut intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("strategy", "crypto_momentum")
            .with_metadata("deployment_id", "crypto-momentum-15m");
        intent.market_slug = "btc-updown-15m-xyz".to_string();

        let result = Coordinator::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_ok());
        assert_eq!(
            intent.metadata.get("deployment_id").map(String::as_str),
            Some("crypto-momentum-15m")
        );
        assert_eq!(
            intent.metadata.get("timeframe").map(String::as_str),
            Some("15m")
        );
    }

    #[test]
    fn test_deployment_gate_blocks_ambiguous_inferred_deployments() {
        let mut deployments = HashMap::new();
        deployments.insert(
            "crypto-momentum-a".to_string(),
            make_deployment(
                "crypto-momentum-a",
                "momentum",
                Domain::Crypto,
                Timeframe::Other("other".to_string()),
                DeploymentExecutionMode::Any,
                DeploymentState::Enabled,
            ),
        );
        deployments.insert(
            "crypto-momentum-b".to_string(),
            make_deployment(
                "crypto-momentum-b",
                "momentum",
                Domain::Crypto,
                Timeframe::Other("other".to_string()),
                DeploymentExecutionMode::Any,
                DeploymentState::Enabled,
            ),
        );

        let mut intent =
            make_intent(true, OrderPriority::Normal).with_metadata("strategy", "momentum");
        intent.market_slug = "btc-updown-unknown".to_string();

        let result = Coordinator::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("ambiguous deployment resolution"));
    }

    #[test]
    fn test_deployment_gate_blocks_runtime_scope_mismatch() {
        let mut deployment = make_deployment(
            "crypto-momentum-15m",
            "momentum",
            Domain::Crypto,
            Timeframe::M15,
            DeploymentExecutionMode::DryRunOnly,
            DeploymentState::Enabled,
        );
        deployment.account_ids = vec!["acct-b".to_string()];

        let mut deployments = HashMap::new();
        deployments.insert("crypto-momentum-15m".to_string(), deployment);

        let mut intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("strategy", "momentum")
            .with_metadata("deployment_id", "crypto-momentum-15m");
        intent.market_slug = "btc-updown-15m-xyz".to_string();

        let result = Coordinator::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not eligible"));
    }

    #[test]
    fn test_deployment_gate_infers_unique_by_timeframe_hint() {
        let mut deployments = HashMap::new();
        deployments.insert(
            "crypto-momentum-5m".to_string(),
            make_deployment(
                "crypto-momentum-5m",
                "momentum",
                Domain::Crypto,
                Timeframe::M5,
                DeploymentExecutionMode::Any,
                DeploymentState::Enabled,
            ),
        );
        deployments.insert(
            "crypto-momentum-15m".to_string(),
            make_deployment(
                "crypto-momentum-15m",
                "momentum",
                Domain::Crypto,
                Timeframe::M15,
                DeploymentExecutionMode::Any,
                DeploymentState::Enabled,
            ),
        );

        let mut intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("strategy", "crypto_momentum")
            .with_metadata("horizon", "15m");
        intent.market_slug = "btc-updown-15m-xyz".to_string();

        let result = Coordinator::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_ok());
        assert_eq!(
            intent.metadata.get("deployment_id").map(String::as_str),
            Some("crypto-momentum-15m")
        );
    }

    #[test]
    fn test_intent_to_request_uses_stable_idempotency_key_by_window() {
        let mut intent = OrderIntent::new(
            "openclaw",
            Domain::Crypto,
            "btc-updown-15m-20260219-1200",
            "token-up-1",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.45),
        );
        intent.metadata.insert(
            "deployment_id".to_string(),
            "crypto.pm.btc.15m.patternmem".to_string(),
        );
        intent
            .metadata
            .insert("horizon".to_string(), "15m".to_string());
        intent
            .metadata
            .insert("event_time".to_string(), "2026-02-19T12:07:00Z".to_string());
        intent.metadata.insert(
            "condition_id".to_string(),
            "0xABCD00000000000000000000000000000000000000000000000000000000".to_string(),
        );

        let key =
            Coordinator::stable_idempotency_key("acct-main", &intent, DuplicateGuardScope::Market);

        assert_ne!(key, intent.intent_id.to_string());
        assert!(key.contains("acct-main"));
        assert!(key.contains("scope:market"));
        assert!(key.contains("dep:market"));
        assert!(key
            .contains("condition:0xabcd00000000000000000000000000000000000000000000000000000000"));
    }

    #[test]
    fn test_stable_idempotency_key_fallback_uses_intent_created_at() {
        let mut first = OrderIntent::new(
            "openclaw",
            Domain::Crypto,
            "btc-updown-15m",
            "token-up-1",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.45),
        );
        let mut second = first.clone();
        first.created_at = chrono::DateTime::parse_from_rfc3339("2026-02-19T12:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);
        second.created_at = chrono::DateTime::parse_from_rfc3339("2026-02-19T13:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);

        let first_key =
            Coordinator::stable_idempotency_key("acct-main", &first, DuplicateGuardScope::Market);
        let second_key =
            Coordinator::stable_idempotency_key("acct-main", &second, DuplicateGuardScope::Market);

        assert_ne!(first_key, second_key);
    }

    #[test]
    fn test_stable_idempotency_key_is_slug_independent_when_condition_present() {
        let mut first = OrderIntent::new(
            "openclaw",
            Domain::Sports,
            "nba-lakers-celtics-v1",
            "token-up-1",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.45),
        );
        first.metadata.insert(
            "deployment_id".to_string(),
            "sports.pm.nba.moneyline".to_string(),
        );
        first.metadata.insert(
            "condition_id".to_string(),
            "0x1111000000000000000000000000000000000000000000000000000000000000".to_string(),
        );
        first
            .metadata
            .insert("event_time".to_string(), "2026-02-20T12:00:00Z".to_string());

        let mut second = first.clone();
        second.market_slug = "nba-lakers-celtics-v2".to_string();

        let first_key =
            Coordinator::stable_idempotency_key("acct-main", &first, DuplicateGuardScope::Market);
        let second_key =
            Coordinator::stable_idempotency_key("acct-main", &second, DuplicateGuardScope::Market);
        assert_eq!(first_key, second_key);
    }

    #[test]
    fn test_governance_policy_update_rejects_unknown_domain() {
        let update = GovernancePolicyUpdate {
            block_new_intents: false,
            blocked_domains: vec!["unknown".to_string()],
            max_intent_notional_usd: None,
            max_total_notional_usd: None,
            updated_by: "openclaw".to_string(),
            reason: None,
            metadata: HashMap::new(),
        };

        let parsed = GovernancePolicy::try_from_update(update);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_governance_policy_blocks_domain() {
        let policy = GovernancePolicy::try_from_update(GovernancePolicyUpdate {
            block_new_intents: false,
            blocked_domains: vec!["sports".to_string()],
            max_intent_notional_usd: None,
            max_total_notional_usd: None,
            updated_by: "openclaw".to_string(),
            reason: Some("maintenance".to_string()),
            metadata: HashMap::new(),
        })
        .expect("valid policy");

        let intent = OrderIntent::new(
            "sports",
            Domain::Sports,
            "nba-game-1",
            "sports-token-yes",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.45),
        );
        let reason = governance_block_reason(&policy, &intent, dec!(0));
        assert!(reason.is_some());
    }

    #[test]
    fn test_governance_policy_blocks_projected_total_notional() {
        let policy = GovernancePolicy::try_from_update(GovernancePolicyUpdate {
            block_new_intents: false,
            blocked_domains: vec![],
            max_intent_notional_usd: Some(dec!(50)),
            max_total_notional_usd: Some(dec!(100)),
            updated_by: "openclaw".to_string(),
            reason: None,
            metadata: HashMap::new(),
        })
        .expect("valid policy");

        let intent = OrderIntent::new(
            "crypto",
            Domain::Crypto,
            "btc-updown-5m-1",
            "token-up",
            crate::domain::Side::Up,
            true,
            50,
            dec!(0.50),
        ); // 25 notional
        let reason = governance_block_reason(&policy, &intent, dec!(90));
        assert!(reason
            .unwrap_or_default()
            .contains("max_total_notional_usd"));
    }

    #[test]
    fn test_governance_policy_allows_sell_when_new_intents_blocked() {
        let policy = GovernancePolicy::try_from_update(GovernancePolicyUpdate {
            block_new_intents: true,
            blocked_domains: vec!["sports".to_string()],
            max_intent_notional_usd: Some(dec!(1)),
            max_total_notional_usd: Some(dec!(1)),
            updated_by: "openclaw".to_string(),
            reason: Some("circuit".to_string()),
            metadata: HashMap::new(),
        })
        .expect("valid policy");

        let intent = OrderIntent::new(
            "sports",
            Domain::Sports,
            "nba-game-1",
            "sports-token-yes",
            crate::domain::Side::Up,
            false, // sell/close
            10,
            dec!(0.45),
        );
        let reason = governance_block_reason(&policy, &intent, dec!(999));
        assert!(reason.is_none(), "sell intent should remain allowed");
    }

    #[tokio::test]
    async fn test_handle_force_close_domain_blocks_new_buy_immediately() {
        let (handle, _coordinator) = make_test_handle();
        handle
            .force_close_domain(Domain::Sports)
            .await
            .expect("force-close domain command accepted");

        let intent = OrderIntent::new(
            "sports",
            Domain::Sports,
            "nba-game-1",
            "sports-token-yes",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.45),
        )
        .with_deployment_id("deploy.sports.nba.test");

        let err = handle
            .submit_order(intent)
            .await
            .expect_err("buy intent should be blocked once domain is force-closed");
        assert!(err.to_string().contains("new intents are blocked"));
    }

    #[tokio::test]
    async fn test_handle_shutdown_domain_blocks_new_buy_immediately() {
        let (handle, _coordinator) = make_test_handle();
        handle
            .shutdown_domain(Domain::Sports)
            .await
            .expect("shutdown domain command accepted");

        let intent = OrderIntent::new(
            "sports",
            Domain::Sports,
            "nba-game-2",
            "sports-token-yes",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.40),
        )
        .with_deployment_id("deploy.sports.nba.test");

        let err = handle
            .submit_order(intent)
            .await
            .expect_err("buy intent should be blocked once domain is shut down");
        assert!(err.to_string().contains("new intents are blocked"));
    }

    #[tokio::test]
    async fn test_governance_status_includes_domain_ingress_and_agents() {
        let (handle, _coordinator) = make_test_handle();
        handle
            .pause_domain(Domain::Sports)
            .await
            .expect("pause domain command accepted");
        {
            let mut state = handle.global_state.write().await;
            state.agents.insert(
                "sports_agent".to_string(),
                AgentSnapshot {
                    agent_id: "sports_agent".to_string(),
                    name: "sports_agent".to_string(),
                    domain: Domain::Sports,
                    status: AgentStatus::Running,
                    position_count: 0,
                    exposure: dec!(12.5),
                    daily_pnl: dec!(1.2),
                    unrealized_pnl: dec!(0.3),
                    metrics: HashMap::new(),
                    last_heartbeat: Utc::now(),
                    error_message: None,
                },
            );
        }

        let snapshot = handle.governance_status().await;

        assert!(snapshot
            .domain_ingress_modes
            .iter()
            .any(|row| row.domain == "sports" && row.mode == "paused"));
        assert!(snapshot
            .agents
            .iter()
            .any(|agent| agent.agent_id == "sports_agent"
                && agent.domain == "sports"
                && agent.status == "running"));
    }

    #[test]
    fn test_clamp_governance_history_limit_bounds() {
        assert_eq!(clamp_governance_history_limit(0), 1);
        assert_eq!(clamp_governance_history_limit(25), 25);
        assert_eq!(clamp_governance_history_limit(999), 500);
    }

    #[test]
    fn test_parse_persisted_domain_supports_runtime_and_custom_encodings() {
        assert_eq!(parse_persisted_domain("Crypto"), Some(Domain::Crypto));
        assert_eq!(
            parse_persisted_domain("custom:42"),
            Some(Domain::Custom(42))
        );
        assert_eq!(parse_persisted_domain("Custom(7)"), Some(Domain::Custom(7)));
        assert_eq!(parse_persisted_domain(""), None);
        assert_eq!(parse_persisted_domain("custom:oops"), None);
    }

    #[test]
    fn test_parse_persisted_side_accepts_yes_no_aliases() {
        assert_eq!(parse_persisted_side("UP"), Some(crate::domain::Side::Up));
        assert_eq!(parse_persisted_side("NO"), Some(crate::domain::Side::Down));
        assert_eq!(parse_persisted_side("flat"), None);
    }

    #[test]
    fn test_string_metadata_from_json_normalizes_scalar_values() {
        let metadata = string_metadata_from_json(Some(sqlx::types::Json(serde_json::json!({
            "deployment_id": "deploy.crypto.15m",
            "signal_confidence": 0.73,
            "flag": true,
            "skip": null
        }))));
        assert_eq!(
            metadata.get("deployment_id").map(String::as_str),
            Some("deploy.crypto.15m")
        );
        assert_eq!(
            metadata.get("signal_confidence").map(String::as_str),
            Some("0.73")
        );
        assert_eq!(metadata.get("flag").map(String::as_str), Some("true"));
        assert!(!metadata.contains_key("skip"));
    }

    #[test]
    fn test_execution_error_is_failure_treats_blank_as_success() {
        assert!(execution_error_is_failure(Some("transport timeout")));
        assert!(!execution_error_is_failure(Some("   ")));
        assert!(!execution_error_is_failure(None));
    }

    fn make_allocator_config(total_cap: Decimal) -> CoordinatorConfig {
        let mut cfg = CoordinatorConfig::default();
        cfg.crypto_allocator_enabled = true;
        cfg.crypto_allocator_total_cap_usd = Some(total_cap);
        cfg.crypto_coin_cap_btc_pct = dec!(0.40);
        cfg.crypto_coin_cap_eth_pct = dec!(0.40);
        cfg.crypto_coin_cap_sol_pct = dec!(0.30);
        cfg.crypto_coin_cap_xrp_pct = dec!(0.20);
        cfg.crypto_coin_cap_other_pct = dec!(0.10);
        cfg.crypto_horizon_cap_5m_pct = dec!(0.50);
        cfg.crypto_horizon_cap_15m_pct = dec!(0.60);
        cfg.crypto_horizon_cap_other_pct = dec!(0.25);
        cfg
    }

    fn make_crypto_intent(
        coin: &str,
        horizon: &str,
        is_buy: bool,
        shares: u64,
        limit_price: Decimal,
    ) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "crypto",
            Domain::Crypto,
            "btc-up-or-down",
            "token-up-123",
            crate::domain::Side::Up,
            is_buy,
            shares,
            limit_price,
        );
        intent.metadata.insert("coin".to_string(), coin.to_string());
        intent
            .metadata
            .insert("horizon".to_string(), horizon.to_string());
        if !is_buy {
            intent
                .metadata
                .insert("entry_price".to_string(), limit_price.to_string());
        }
        intent
    }

    #[test]
    fn test_crypto_allocator_blocks_buy_when_coin_cap_exceeded() {
        let cfg = make_allocator_config(dec!(100));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);

        let first = make_crypto_intent("BTC", "5m", true, 60, dec!(0.5)); // $30
        let second = make_crypto_intent("BTC", "5m", true, 30, dec!(0.5)); // $15 -> total $45 > BTC cap $40

        assert!(allocator.reserve_buy(&first).is_ok());
        assert!(allocator.reserve_buy(&second).is_err());
    }

    #[test]
    fn test_crypto_allocator_clamps_total_cap_to_risk_domain_cap() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.crypto_allocator_total_cap_usd = Some(dec!(100));
        cfg.risk.crypto_max_exposure = Some(dec!(60));

        let allocator = CryptoCapitalAllocator::new(&cfg);
        assert_eq!(allocator.total_cap, dec!(60));
    }

    #[test]
    fn test_crypto_allocator_releases_pending_on_buy_failure() {
        let cfg = make_allocator_config(dec!(100));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);
        let intent = make_crypto_intent("BTC", "5m", true, 50, dec!(0.5)); // $25

        assert!(allocator.reserve_buy(&intent).is_ok());
        assert!(allocator.pending.total > Decimal::ZERO);

        allocator.release_buy_reservation(intent.intent_id);

        assert_eq!(allocator.pending.total, Decimal::ZERO);
        assert!(allocator.pending_by_intent.is_empty());
    }

    #[test]
    fn test_crypto_allocator_settles_buy_then_sell() {
        let cfg = make_allocator_config(dec!(200));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);
        let buy = make_crypto_intent("BTC", "15m", true, 100, dec!(0.5)); // reserve $50

        assert!(allocator.reserve_buy(&buy).is_ok());
        allocator.settle_buy_execution(&buy, 80, dec!(0.5)); // open $40

        assert_eq!(allocator.pending.total, Decimal::ZERO);
        assert_eq!(allocator.open.total, dec!(40));

        let mut sell = make_crypto_intent("BTC", "15m", false, 40, dec!(0.5));
        sell.market_slug = buy.market_slug.clone();
        sell.token_id = buy.token_id.clone();
        sell.side = buy.side;
        allocator.settle_sell_execution(&sell, 40, dec!(0.55)); // release by entry price metadata ($20)

        assert_eq!(allocator.open.total, dec!(20));
    }

    #[test]
    fn test_crypto_allocator_sell_without_entry_price_does_not_release_other_positions() {
        let cfg = make_allocator_config(dec!(200));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);

        let mut buy_a = make_crypto_intent("BTC", "15m", true, 100, dec!(0.2)); // $20
        buy_a.market_slug = "btc-updown-a".to_string();
        buy_a.token_id = "token-up-a".to_string();
        buy_a = buy_a.with_deployment_id("deploy.crypto.btc.15m");

        let mut buy_b = make_crypto_intent("BTC", "15m", true, 100, dec!(0.2)); // $20
        buy_b.market_slug = "btc-updown-b".to_string();
        buy_b.token_id = "token-up-b".to_string();
        buy_b.side = crate::domain::Side::Down;
        buy_b = buy_b.with_deployment_id("deploy.crypto.btc.15m");

        assert!(allocator.reserve_buy(&buy_a).is_ok());
        allocator.settle_buy_execution(&buy_a, 100, dec!(0.2));
        assert!(allocator.reserve_buy(&buy_b).is_ok());
        allocator.settle_buy_execution(&buy_b, 100, dec!(0.2));
        assert_eq!(allocator.open.total, dec!(40));

        let mut sell_a = make_crypto_intent("BTC", "15m", false, 100, dec!(0.2));
        sell_a.market_slug = buy_a.market_slug.clone();
        sell_a.token_id = buy_a.token_id.clone();
        sell_a.side = buy_a.side;
        sell_a = sell_a.with_deployment_id("deploy.crypto.btc.15m");
        sell_a.metadata.remove("entry_price");

        // Missing entry_price + high execution price must not release other bucket positions.
        allocator.settle_sell_execution(&sell_a, 100, dec!(0.8));
        assert_eq!(allocator.open.total, dec!(20));
        assert_eq!(allocator.open.by_position.len(), 1);
    }

    fn make_sports_intent(
        market_slug: &str,
        is_buy: bool,
        shares: u64,
        limit_price: Decimal,
    ) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "sports",
            Domain::Sports,
            market_slug,
            "sports-token-yes",
            crate::domain::Side::Up,
            is_buy,
            shares,
            limit_price,
        );
        if !is_buy {
            intent
                .metadata
                .insert("entry_price".to_string(), limit_price.to_string());
        }
        intent
    }

    fn make_domain_market_intent(
        domain: Domain,
        market_slug: &str,
        is_buy: bool,
        shares: u64,
        limit_price: Decimal,
    ) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "domain-agent",
            domain,
            market_slug,
            "domain-token-yes",
            crate::domain::Side::Up,
            is_buy,
            shares,
            limit_price,
        );
        if !is_buy {
            intent
                .metadata
                .insert("entry_price".to_string(), limit_price.to_string());
        }
        intent
    }

    #[test]
    fn test_sports_allocator_auto_splits_by_active_markets() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(30));
        cfg.sports_market_cap_pct = dec!(0.70);
        cfg.sports_auto_split_by_active_markets = true;

        let mut allocator = MarketCapitalAllocator::for_sports(&cfg);

        let game1_buy = make_sports_intent("nba-game-1", true, 100, dec!(0.15)); // $15
        let game2_buy = make_sports_intent("nba-game-2", true, 100, dec!(0.15)); // $15
        let game1_extra = make_sports_intent("nba-game-1", true, 10, dec!(0.10)); // $1

        assert!(allocator.reserve_buy(&game1_buy).is_ok());
        assert!(allocator.reserve_buy(&game2_buy).is_ok());
        assert!(allocator.reserve_buy(&game1_extra).is_err());
    }

    #[test]
    fn test_sports_allocator_releases_pending_on_buy_failure() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(30));

        let mut allocator = MarketCapitalAllocator::for_sports(&cfg);
        let intent = make_sports_intent("nba-game-1", true, 100, dec!(0.10)); // $10

        assert!(allocator.reserve_buy(&intent).is_ok());
        assert!(allocator.pending.total > Decimal::ZERO);

        allocator.release_buy_reservation(intent.intent_id);

        assert_eq!(allocator.pending.total, Decimal::ZERO);
        assert!(allocator.pending_by_intent.is_empty());
    }

    #[test]
    fn test_sports_allocator_clamps_total_cap_to_risk_domain_cap() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(50));
        cfg.risk.sports_max_exposure = Some(dec!(25));

        let allocator = MarketCapitalAllocator::for_sports(&cfg);
        assert_eq!(allocator.total_cap, dec!(25));
    }

    #[test]
    fn test_market_allocator_sell_without_entry_price_does_not_release_other_positions() {
        let mut cfg = make_allocator_config(dec!(200));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(200));
        cfg.sports_market_cap_pct = dec!(1.0);

        let mut allocator = MarketCapitalAllocator::for_sports(&cfg);

        let mut buy_yes = make_sports_intent("nba-game-1", true, 100, dec!(0.2)); // $20
        buy_yes = buy_yes.with_deployment_id("deploy.sports.nba.comeback");

        let mut buy_no = make_sports_intent("nba-game-1", true, 100, dec!(0.2)); // $20
        buy_no.token_id = "sports-token-no".to_string();
        buy_no.side = crate::domain::Side::Down;
        buy_no = buy_no.with_deployment_id("deploy.sports.nba.comeback");

        assert!(allocator.reserve_buy(&buy_yes).is_ok());
        allocator.settle_buy_execution(&buy_yes, 100, dec!(0.2));
        assert!(allocator.reserve_buy(&buy_no).is_ok());
        allocator.settle_buy_execution(&buy_no, 100, dec!(0.2));
        assert_eq!(allocator.open.total, dec!(40));

        let mut sell_yes = make_sports_intent("nba-game-1", false, 100, dec!(0.2));
        sell_yes.token_id = buy_yes.token_id.clone();
        sell_yes.side = buy_yes.side;
        sell_yes = sell_yes.with_deployment_id("deploy.sports.nba.comeback");
        sell_yes.metadata.remove("entry_price");

        // Missing entry_price + high execution price must not release opposite-side position.
        allocator.settle_sell_execution(&sell_yes, 100, dec!(0.8));
        assert_eq!(allocator.open.total, dec!(20));
        assert_eq!(allocator.open.by_position.len(), 1);
    }

    #[test]
    fn test_politics_allocator_clamps_total_cap_to_risk_domain_cap() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.politics_allocator_enabled = true;
        cfg.politics_allocator_total_cap_usd = Some(dec!(40));
        cfg.risk.politics_max_exposure = Some(dec!(18));

        let allocator = MarketCapitalAllocator::for_politics(&cfg);
        assert_eq!(allocator.total_cap, dec!(18));
    }

    #[test]
    fn test_economics_allocator_reserves_with_condition_identity() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.economics_allocator_enabled = true;
        cfg.economics_allocator_total_cap_usd = Some(dec!(30));
        cfg.economics_market_cap_pct = dec!(0.60);
        cfg.economics_auto_split_by_active_markets = true;

        let mut allocator = MarketCapitalAllocator::for_economics(&cfg);
        let mut first =
            make_domain_market_intent(Domain::Economics, "fed-rate-cut-v1", true, 100, dec!(0.10));
        first.metadata.insert(
            "condition_id".to_string(),
            "0x2222000000000000000000000000000000000000000000000000000000000000".to_string(),
        );
        let mut second =
            make_domain_market_intent(Domain::Economics, "fed-rate-cut-v2", true, 100, dec!(0.10));
        second.metadata.insert(
            "condition_id".to_string(),
            "0x2222000000000000000000000000000000000000000000000000000000000000".to_string(),
        );

        assert!(allocator.reserve_buy(&first).is_ok());
        // same condition_id should hit the same market bucket and exceed per-market cap
        assert!(allocator.reserve_buy(&second).is_err());
    }

    #[test]
    fn test_crypto_allocator_ledger_snapshot_reports_open_pending_and_available() {
        let cfg = make_allocator_config(dec!(200));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);

        let buy = make_crypto_intent("BTC", "15m", true, 100, dec!(0.5)); // reserve $50
        assert!(allocator.reserve_buy(&buy).is_ok());
        allocator.settle_buy_execution(&buy, 80, dec!(0.5)); // open $40

        let second = make_crypto_intent("ETH", "5m", true, 20, dec!(0.5)); // pending $10
        assert!(allocator.reserve_buy(&second).is_ok());

        let snap = allocator.ledger_snapshot();
        assert_eq!(snap.domain, "crypto");
        assert_eq!(snap.cap_notional_usd, dec!(200));
        assert_eq!(snap.open_notional_usd, dec!(40));
        assert_eq!(snap.pending_notional_usd, dec!(10));
        assert_eq!(snap.available_notional_usd, dec!(150));
    }

    #[test]
    fn test_sports_allocator_ledger_snapshot_reports_open_pending_and_available() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(50));
        let mut allocator = MarketCapitalAllocator::for_sports(&cfg);

        let buy = make_sports_intent("nba-game-1", true, 100, dec!(0.10)); // reserve $10
        assert!(allocator.reserve_buy(&buy).is_ok());
        allocator.settle_buy_execution(&buy, 50, dec!(0.10)); // open $5

        let pending = make_sports_intent("nba-game-2", true, 40, dec!(0.10)); // pending $4
        assert!(allocator.reserve_buy(&pending).is_ok());

        let snap = allocator.ledger_snapshot();
        assert_eq!(snap.domain, "sports");
        assert_eq!(snap.cap_notional_usd, dec!(50));
        assert_eq!(snap.open_notional_usd, dec!(5));
        assert_eq!(snap.pending_notional_usd, dec!(4));
        assert_eq!(snap.available_notional_usd, dec!(41));
    }

    #[test]
    fn test_crypto_allocator_deployment_ledger_snapshot_groups_open_and_pending() {
        let cfg = make_allocator_config(dec!(200));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);

        let buy_a = make_crypto_intent("BTC", "15m", true, 100, dec!(0.5))
            .with_deployment_id("deploy.crypto.alpha");
        assert!(allocator.reserve_buy(&buy_a).is_ok());
        allocator.settle_buy_execution(&buy_a, 80, dec!(0.5)); // open $40

        let pending_a = make_crypto_intent("BTC", "15m", true, 20, dec!(0.5))
            .with_deployment_id("deploy.crypto.alpha");
        assert!(allocator.reserve_buy(&pending_a).is_ok()); // pending $10

        let buy_b = make_crypto_intent("ETH", "5m", true, 50, dec!(0.4))
            .with_deployment_id("deploy.crypto.beta");
        assert!(allocator.reserve_buy(&buy_b).is_ok());
        allocator.settle_buy_execution(&buy_b, 25, dec!(0.4)); // open $10

        let deployments = allocator.deployment_ledger_snapshot();
        assert_eq!(deployments.len(), 2);
        assert_eq!(deployments[0].deployment_id, "deploy.crypto.alpha");
        assert_eq!(deployments[0].domain, "crypto");
        assert_eq!(deployments[0].open_notional_usd, dec!(40));
        assert_eq!(deployments[0].pending_notional_usd, dec!(10));
        assert_eq!(deployments[0].total_notional_usd, dec!(50));

        assert_eq!(deployments[1].deployment_id, "deploy.crypto.beta");
        assert_eq!(deployments[1].domain, "crypto");
        assert_eq!(deployments[1].open_notional_usd, dec!(10));
        assert_eq!(deployments[1].pending_notional_usd, Decimal::ZERO);
        assert_eq!(deployments[1].total_notional_usd, dec!(10));
    }

    #[test]
    fn test_market_allocator_deployment_ledger_snapshot_groups_open_and_pending() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(60));
        cfg.sports_market_cap_pct = dec!(1.0);
        let mut allocator = MarketCapitalAllocator::for_sports(&cfg);

        let buy_a = make_sports_intent("nba-game-1", true, 100, dec!(0.2))
            .with_deployment_id("deploy.sports.alpha");
        assert!(allocator.reserve_buy(&buy_a).is_ok());
        allocator.settle_buy_execution(&buy_a, 50, dec!(0.2)); // open $10

        let pending_a = make_sports_intent("nba-game-2", true, 20, dec!(0.2))
            .with_deployment_id("deploy.sports.alpha");
        assert!(allocator.reserve_buy(&pending_a).is_ok()); // pending $4

        let buy_b = make_sports_intent("nba-game-3", true, 40, dec!(0.25))
            .with_deployment_id("deploy.sports.beta");
        assert!(allocator.reserve_buy(&buy_b).is_ok());
        allocator.settle_buy_execution(&buy_b, 20, dec!(0.25)); // open $5

        let deployments = allocator.deployment_ledger_snapshot();
        assert_eq!(deployments.len(), 2);
        assert_eq!(deployments[0].deployment_id, "deploy.sports.alpha");
        assert_eq!(deployments[0].domain, "sports");
        assert_eq!(deployments[0].open_notional_usd, dec!(10));
        assert_eq!(deployments[0].pending_notional_usd, dec!(4));
        assert_eq!(deployments[0].total_notional_usd, dec!(14));

        assert_eq!(deployments[1].deployment_id, "deploy.sports.beta");
        assert_eq!(deployments[1].domain, "sports");
        assert_eq!(deployments[1].open_notional_usd, dec!(5));
        assert_eq!(deployments[1].pending_notional_usd, Decimal::ZERO);
        assert_eq!(deployments[1].total_notional_usd, dec!(5));
    }
}
