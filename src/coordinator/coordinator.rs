//! Coordinator — central orchestrator for multi-agent trading
//!
//! The Coordinator owns the order queue, risk gate, and position aggregator.
//! Agents communicate with it via `CoordinatorHandle` (clone-friendly).
//! The main `run()` loop uses `tokio::select!` to:
//!   - Process incoming order intents (risk check → enqueue)
//!   - Process agent state updates (heartbeats)
//!   - Periodically drain the queue and execute orders
//!   - Periodically refresh GlobalState from aggregators

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use sqlx::PgPool;

use crate::domain::{OrderRequest, Side};
use crate::error::Result;
use crate::platform::{
    AgentRiskParams, Domain, OrderIntent, OrderPriority, OrderQueue, PositionAggregator,
    RiskCheckResult, RiskGate,
};
use crate::strategy::executor::OrderExecutor;

use super::command::{
    AllocatorLedgerSnapshot, CoordinatorCommand, CoordinatorControlCommand,
    DeploymentLedgerSnapshot, GovernancePolicyHistoryEntry, GovernancePolicySnapshot,
    GovernancePolicyUpdate, GovernanceStatusSnapshot,
};
use super::config::CoordinatorConfig;
use super::state::{AgentSnapshot, GlobalState, QueueStatsSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngressMode {
    Running,
    Paused,
    Halted,
}

impl IngressMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Halted => "halted",
        }
    }
}

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

fn buy_intent_missing_deployment_reason(intent: &OrderIntent) -> Option<String> {
    if !intent.is_buy {
        return None;
    }

    let has_deployment_id = intent
        .deployment_id()
        .and_then(normalized_identity_component)
        .is_some();

    if has_deployment_id {
        None
    } else {
        Some("BUY intent missing required metadata field 'deployment_id'".to_string())
    }
}

fn sell_reduce_only_violation_reason(
    intent: &OrderIntent,
    tracked_open_shares: u64,
) -> Option<String> {
    if intent.is_buy {
        return None;
    }

    if tracked_open_shares == 0 {
        return Some(format!(
            "SELL intent reduce-only violation: no tracked open shares for token_id={} side={} in domain={}",
            intent.token_id,
            intent.side.as_str(),
            intent.domain
        ));
    }

    if intent.shares > tracked_open_shares {
        return Some(format!(
            "SELL intent reduce-only violation: requested shares {} exceeds tracked open shares {} for token_id={} side={}",
            intent.shares,
            tracked_open_shares,
            intent.token_id,
            intent.side.as_str()
        ));
    }

    None
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

fn governance_block_reason(
    policy: &GovernancePolicy,
    intent: &OrderIntent,
    current_account_notional: Decimal,
) -> Option<String> {
    // Binary-options runtime: sell intents are treated as risk-reducing closes.
    // Governance "new intent" gates must not block exits/de-risking.
    if !intent.is_buy {
        return None;
    }

    if policy.block_new_intents {
        return Some("global governance policy blocks new intents".to_string());
    }

    if policy.blocked_domains.contains(&intent.domain) {
        return Some(format!(
            "domain '{}' is blocked by global governance policy",
            governance_domain_label(intent.domain)
        ));
    }

    let intent_notional = intent.notional_value();
    if let Some(max_intent) = policy.max_intent_notional_usd {
        if intent_notional > max_intent {
            return Some(format!(
                "intent notional {} exceeds governance max_intent_notional_usd {}",
                intent_notional, max_intent
            ));
        }
    }

    if intent.is_buy {
        if let Some(max_total) = policy.max_total_notional_usd {
            let projected = current_account_notional + intent_notional;
            if projected > max_total {
                return Some(format!(
                    "projected account notional {} exceeds governance max_total_notional_usd {}",
                    projected, max_total
                ));
            }
        }
    }

    None
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
    crypto_allocator: Arc<RwLock<CryptoCapitalAllocator>>,
    sports_allocator: Arc<RwLock<MarketCapitalAllocator>>,
    politics_allocator: Arc<RwLock<MarketCapitalAllocator>>,
    economics_allocator: Arc<RwLock<MarketCapitalAllocator>>,
    positions: Arc<PositionAggregator>,
    ingress_mode: Arc<RwLock<IngressMode>>,
    domain_ingress_mode: Arc<RwLock<HashMap<Domain, IngressMode>>>,
    governance_policy: Arc<RwLock<GovernancePolicy>>,
    governance_store_pool: Option<PgPool>,
}

impl CoordinatorHandle {
    /// Submit an order intent to the coordinator for risk checking and execution
    pub async fn submit_order(&self, intent: OrderIntent) -> Result<()> {
        if let Some(reason) = buy_intent_missing_deployment_reason(&intent) {
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
            if let Some(reason) = sell_reduce_only_violation_reason(&intent, tracked_open_shares) {
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
        self.control_tx
            .send(CoordinatorControlCommand::ForceCloseDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Shutdown one domain
    pub async fn shutdown_domain(&self, domain: Domain) -> Result<()> {
        self.control_tx
            .send(CoordinatorControlCommand::ShutdownDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Read the current global state (non-blocking snapshot)
    pub async fn read_state(&self) -> GlobalState {
        self.global_state.read().await.clone()
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
            policy,
            account_notional_usd,
            platform_exposure_usd,
            risk_state,
            daily_pnl_usd,
            daily_loss_limit_usd,
            queue,
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
    risk_gate: Arc<RiskGate>,
    order_queue: Arc<RwLock<OrderQueue>>,
    duplicate_guard: Arc<RwLock<IntentDuplicateGuard>>,
    crypto_allocator: Arc<RwLock<CryptoCapitalAllocator>>,
    sports_allocator: Arc<RwLock<MarketCapitalAllocator>>,
    politics_allocator: Arc<RwLock<MarketCapitalAllocator>>,
    economics_allocator: Arc<RwLock<MarketCapitalAllocator>>,
    positions: Arc<PositionAggregator>,
    executor: Arc<OrderExecutor>,
    global_state: Arc<RwLock<GlobalState>>,
    execution_log_pool: Option<PgPool>,
    governance_store_pool: Option<PgPool>,
    ingress_mode: Arc<RwLock<IngressMode>>,
    domain_ingress_mode: Arc<RwLock<HashMap<Domain, IngressMode>>>,
    governance_policy: Arc<RwLock<GovernancePolicy>>,
    stale_heartbeat_warn_at: Arc<RwLock<HashMap<String, chrono::DateTime<Utc>>>>,

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

#[derive(Debug)]
struct IntentDuplicateGuard {
    enabled: bool,
    window: ChronoDuration,
    recent_buys: HashMap<String, chrono::DateTime<Utc>>,
}

impl IntentDuplicateGuard {
    fn new(window_ms: u64, enabled: bool) -> Self {
        let clamped_ms = window_ms.min(i64::MAX as u64) as i64;
        let window = ChronoDuration::milliseconds(clamped_ms.max(1));
        Self {
            enabled,
            window,
            recent_buys: HashMap::new(),
        }
    }

    fn deployment_scope(intent: &OrderIntent) -> String {
        intent_deployment_scope(intent)
    }

    fn buy_key(intent: &OrderIntent) -> Option<String> {
        // Only guard normal/high-priority ENTRY orders.
        // Use condition_id-first market identity so opposite-side re-entries
        // on the same contract are blocked within the duplicate window.
        // Scope by deployment to avoid blocking independent strategy deployments.
        if !intent.is_buy || intent.priority == OrderPriority::Critical {
            return None;
        }

        Some(format!(
            "{}|{}|{}",
            intent.domain,
            Self::deployment_scope(intent),
            intent_market_identity(intent)
        ))
    }

    fn prune(&mut self, now: chrono::DateTime<Utc>) {
        self.recent_buys
            .retain(|_, ts| now.signed_duration_since(*ts) < self.window);
    }

    fn register_or_block(
        &mut self,
        intent: &OrderIntent,
        now: chrono::DateTime<Utc>,
    ) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let key = Self::buy_key(intent)?;
        self.prune(now);

        if let Some(last) = self.recent_buys.get(&key) {
            let elapsed_ms = now.signed_duration_since(*last).num_milliseconds().max(0);
            return Some(format!(
                "Duplicate buy intent blocked (elapsed={}ms, guard_window={}ms, key={})",
                elapsed_ms,
                self.window.num_milliseconds(),
                key
            ));
        }

        self.recent_buys.insert(key, now);
        None
    }
}

const KNOWN_5M_SERIES_IDS: &[&str] = &["10684", "10683", "10686", "10685"];
const KNOWN_15M_SERIES_IDS: &[&str] = &["10192", "10191", "10423", "10422"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CryptoHorizon {
    M5,
    M15,
    Other,
}

impl CryptoHorizon {
    fn as_str(&self) -> &'static str {
        match self {
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::Other => "other",
        }
    }

    fn from_hint(raw: &str) -> Option<Self> {
        let normalized = raw.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return None;
        }
        if normalized.contains("15m") || normalized == "15" {
            return Some(Self::M15);
        }
        if normalized.contains("5m") || normalized == "5" {
            return Some(Self::M5);
        }
        if KNOWN_15M_SERIES_IDS.iter().any(|id| *id == normalized) {
            return Some(Self::M15);
        }
        if KNOWN_5M_SERIES_IDS.iter().any(|id| *id == normalized) {
            return Some(Self::M5);
        }
        None
    }
}

#[derive(Debug, Clone)]
struct CryptoIntentDimensions {
    coin: String,
    horizon: CryptoHorizon,
    deployment_scope: String,
    position_key: String,
}

impl CryptoIntentDimensions {
    fn from_intent(intent: &OrderIntent) -> Self {
        let coin = Self::parse_coin(intent).unwrap_or_else(|| "OTHER".to_string());
        let horizon = Self::parse_horizon(intent).unwrap_or(CryptoHorizon::Other);
        let market_identity = intent_market_identity(intent);
        let deployment_scope = intent_deployment_scope(intent);
        let position_key = format!(
            "{}|{}|{}|{}",
            deployment_scope,
            market_identity,
            intent.token_id,
            intent.side.as_str()
        );
        Self {
            coin,
            horizon,
            deployment_scope,
            position_key,
        }
    }

    fn parse_coin(intent: &OrderIntent) -> Option<String> {
        if let Some(coin) = intent
            .metadata
            .get("coin")
            .and_then(|raw| Self::normalize_coin(raw))
        {
            return Some(coin);
        }

        if let Some(symbol) = intent.metadata.get("symbol") {
            let cleaned = symbol
                .trim()
                .to_ascii_uppercase()
                .replace("USDT", "")
                .replace("USD", "");
            if let Some(coin) = Self::normalize_coin(&cleaned) {
                return Some(coin);
            }
        }

        let slug = intent.market_slug.to_ascii_lowercase();
        for (needle, coin) in [
            ("bitcoin", "BTC"),
            ("btc", "BTC"),
            ("ethereum", "ETH"),
            ("eth", "ETH"),
            ("solana", "SOL"),
            ("sol", "SOL"),
            ("xrp", "XRP"),
        ] {
            if slug.contains(needle) {
                return Some(coin.to_string());
            }
        }

        None
    }

    fn parse_horizon(intent: &OrderIntent) -> Option<CryptoHorizon> {
        if let Some(h) = intent
            .metadata
            .get("horizon")
            .and_then(|raw| CryptoHorizon::from_hint(raw))
        {
            return Some(h);
        }

        if let Some(h) = intent
            .metadata
            .get("event_series_id")
            .and_then(|raw| CryptoHorizon::from_hint(raw))
        {
            return Some(h);
        }

        if let Some(h) = intent
            .metadata
            .get("series_id")
            .and_then(|raw| CryptoHorizon::from_hint(raw))
        {
            return Some(h);
        }

        CryptoHorizon::from_hint(&intent.market_slug)
    }

    fn normalize_coin(raw: &str) -> Option<String> {
        let coin = raw.trim().to_ascii_uppercase();
        if coin.is_empty() {
            return None;
        }
        Some(match coin.as_str() {
            "BITCOIN" | "BTC" => "BTC".to_string(),
            "ETHEREUM" | "ETH" => "ETH".to_string(),
            "SOLANA" | "SOL" => "SOL".to_string(),
            "XRP" => "XRP".to_string(),
            other => other.to_string(),
        })
    }
}

#[derive(Debug, Clone)]
struct PositionExposure {
    deployment_scope: String,
    coin: String,
    horizon: CryptoHorizon,
    amount: Decimal,
}

#[derive(Debug, Default)]
struct ExposureBook {
    total: Decimal,
    by_coin: HashMap<String, Decimal>,
    by_horizon: HashMap<CryptoHorizon, Decimal>,
    by_position: HashMap<String, PositionExposure>,
}

impl ExposureBook {
    fn value_for_coin(&self, coin: &str) -> Decimal {
        self.by_coin.get(coin).copied().unwrap_or(Decimal::ZERO)
    }

    fn value_for_horizon(&self, horizon: CryptoHorizon) -> Decimal {
        self.by_horizon
            .get(&horizon)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    fn add(&mut self, dims: &CryptoIntentDimensions, amount: Decimal) {
        if amount <= Decimal::ZERO {
            return;
        }
        self.total += amount;
        *self
            .by_coin
            .entry(dims.coin.clone())
            .or_insert(Decimal::ZERO) += amount;
        *self.by_horizon.entry(dims.horizon).or_insert(Decimal::ZERO) += amount;
        self.by_position
            .entry(dims.position_key.clone())
            .and_modify(|pos| {
                pos.amount += amount;
                pos.deployment_scope = dims.deployment_scope.clone();
                pos.coin = dims.coin.clone();
                pos.horizon = dims.horizon;
            })
            .or_insert_with(|| PositionExposure {
                deployment_scope: dims.deployment_scope.clone(),
                coin: dims.coin.clone(),
                horizon: dims.horizon,
                amount,
            });
    }

    fn subtract_from_position_key(&mut self, position_key: &str, amount: Decimal) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let mut removed = Decimal::ZERO;
        let mut coin = None;
        let mut horizon = None;
        let mut delete_key = false;

        if let Some(pos) = self.by_position.get_mut(position_key) {
            removed = amount.min(pos.amount);
            if removed > Decimal::ZERO {
                pos.amount -= removed;
                coin = Some(pos.coin.clone());
                horizon = Some(pos.horizon);
                delete_key = pos.amount <= Decimal::ZERO;
            }
        }

        if delete_key {
            self.by_position.remove(position_key);
        }

        if removed <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        self.total = (self.total - removed).max(Decimal::ZERO);

        if let Some(c) = coin {
            if let Some(v) = self.by_coin.get_mut(&c) {
                *v = (*v - removed).max(Decimal::ZERO);
                if *v == Decimal::ZERO {
                    self.by_coin.remove(&c);
                }
            }
        }

        if let Some(h) = horizon {
            if let Some(v) = self.by_horizon.get_mut(&h) {
                *v = (*v - removed).max(Decimal::ZERO);
                if *v == Decimal::ZERO {
                    self.by_horizon.remove(&h);
                }
            }
        }

        removed
    }

    fn subtract_matching_bucket(
        &mut self,
        deployment_scope: &str,
        coin: &str,
        horizon: CryptoHorizon,
        amount: Decimal,
    ) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let mut remaining = amount;
        let keys: Vec<String> = self
            .by_position
            .iter()
            .filter(|(_, p)| {
                p.deployment_scope == deployment_scope && p.coin == coin && p.horizon == horizon
            })
            .map(|(k, _)| k.clone())
            .collect();

        for key in keys {
            if remaining <= Decimal::ZERO {
                break;
            }
            let removed = self.subtract_from_position_key(&key, remaining);
            remaining -= removed;
        }

        amount - remaining
    }
}

#[derive(Debug, Clone)]
struct PendingCryptoIntent {
    dims: CryptoIntentDimensions,
    requested_notional: Decimal,
}

#[derive(Debug)]
struct CryptoCapitalAllocator {
    enabled: bool,
    total_cap: Decimal,
    coin_cap_pct: HashMap<String, Decimal>,
    horizon_cap_pct: HashMap<CryptoHorizon, Decimal>,
    open: ExposureBook,
    pending: ExposureBook,
    pending_by_intent: HashMap<Uuid, PendingCryptoIntent>,
}

impl CryptoCapitalAllocator {
    fn new(config: &CoordinatorConfig) -> Self {
        let configured_cap = config
            .crypto_allocator_total_cap_usd
            .or(config.risk.crypto_max_exposure)
            .unwrap_or(config.risk.max_platform_exposure);
        let total_cap = config
            .risk
            .crypto_max_exposure
            .map(|risk_cap| configured_cap.min(risk_cap))
            .unwrap_or(configured_cap)
            .max(Decimal::ZERO);

        let mut coin_cap_pct = HashMap::new();
        coin_cap_pct.insert(
            "BTC".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_btc_pct),
        );
        coin_cap_pct.insert(
            "ETH".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_eth_pct),
        );
        coin_cap_pct.insert(
            "SOL".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_sol_pct),
        );
        coin_cap_pct.insert(
            "XRP".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_xrp_pct),
        );
        coin_cap_pct.insert(
            "OTHER".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_other_pct),
        );

        let mut horizon_cap_pct = HashMap::new();
        horizon_cap_pct.insert(
            CryptoHorizon::M5,
            Self::normalize_pct(config.crypto_horizon_cap_5m_pct),
        );
        horizon_cap_pct.insert(
            CryptoHorizon::M15,
            Self::normalize_pct(config.crypto_horizon_cap_15m_pct),
        );
        horizon_cap_pct.insert(
            CryptoHorizon::Other,
            Self::normalize_pct(config.crypto_horizon_cap_other_pct),
        );

        Self {
            enabled: config.crypto_allocator_enabled,
            total_cap,
            coin_cap_pct,
            horizon_cap_pct,
            open: ExposureBook::default(),
            pending: ExposureBook::default(),
            pending_by_intent: HashMap::new(),
        }
    }

    fn normalize_pct(value: Decimal) -> Decimal {
        if value <= Decimal::ZERO {
            Decimal::ZERO
        } else if value >= Decimal::ONE {
            Decimal::ONE
        } else {
            value
        }
    }

    fn reset_runtime_state(&mut self) {
        self.open = ExposureBook::default();
        self.pending = ExposureBook::default();
        self.pending_by_intent.clear();
    }

    fn reserve_buy(&mut self, intent: &OrderIntent) -> std::result::Result<(), String> {
        if !self.enabled || intent.domain != Domain::Crypto || !intent.is_buy {
            return Ok(());
        }

        if self.total_cap <= Decimal::ZERO {
            return Err("Crypto allocator cap is 0; buy intent blocked".to_string());
        }

        let requested = intent.notional_value();
        if requested <= Decimal::ZERO {
            return Err("Crypto buy intent has non-positive notional".to_string());
        }

        let dims = CryptoIntentDimensions::from_intent(intent);

        let projected_total = self.open.total + self.pending.total + requested;
        if projected_total > self.total_cap {
            return Err(format!(
                "Crypto total cap exceeded: projected={} cap={}",
                projected_total, self.total_cap
            ));
        }

        let coin_cap = self.total_cap * self.coin_cap_for(&dims.coin);
        let projected_coin = self.open.value_for_coin(&dims.coin)
            + self.pending.value_for_coin(&dims.coin)
            + requested;
        if projected_coin > coin_cap {
            return Err(format!(
                "Crypto coin cap exceeded: coin={} projected={} cap={}",
                dims.coin, projected_coin, coin_cap
            ));
        }

        let horizon_cap = self.total_cap * self.horizon_cap_for(dims.horizon);
        let projected_horizon = self.open.value_for_horizon(dims.horizon)
            + self.pending.value_for_horizon(dims.horizon)
            + requested;
        if projected_horizon > horizon_cap {
            return Err(format!(
                "Crypto horizon cap exceeded: horizon={} projected={} cap={}",
                dims.horizon.as_str(),
                projected_horizon,
                horizon_cap
            ));
        }

        self.pending.add(&dims, requested);
        self.pending_by_intent.insert(
            intent.intent_id,
            PendingCryptoIntent {
                dims,
                requested_notional: requested,
            },
        );

        Ok(())
    }

    fn release_buy_reservation(&mut self, intent_id: Uuid) {
        let Some(reservation) = self.pending_by_intent.remove(&intent_id) else {
            return;
        };
        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );
    }

    fn settle_buy_execution(
        &mut self,
        intent: &OrderIntent,
        filled_shares: u64,
        fill_price: Decimal,
    ) {
        if !self.enabled || intent.domain != Domain::Crypto || !intent.is_buy {
            return;
        }

        let reservation = self
            .pending_by_intent
            .remove(&intent.intent_id)
            .unwrap_or_else(|| PendingCryptoIntent {
                dims: CryptoIntentDimensions::from_intent(intent),
                requested_notional: intent.notional_value(),
            });

        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );

        if filled_shares == 0 || fill_price <= Decimal::ZERO {
            return;
        }

        let actual_notional = fill_price * Decimal::from(filled_shares);
        self.open.add(&reservation.dims, actual_notional);
    }

    fn settle_sell_execution(
        &mut self,
        intent: &OrderIntent,
        filled_shares: u64,
        execution_price: Decimal,
    ) {
        if !self.enabled || intent.domain != Domain::Crypto || intent.is_buy || filled_shares == 0 {
            return;
        }

        let dims = CryptoIntentDimensions::from_intent(intent);
        let Some((reference_price, has_explicit_entry_price)) =
            sell_release_reference_price(intent, execution_price)
        else {
            return;
        };

        if reference_price <= Decimal::ZERO {
            return;
        }

        let requested_release = Decimal::from(filled_shares) * reference_price;
        let removed_by_key = self
            .open
            .subtract_from_position_key(&dims.position_key, requested_release);
        if has_explicit_entry_price && removed_by_key < requested_release {
            let remaining = requested_release - removed_by_key;
            self.open.subtract_matching_bucket(
                &dims.deployment_scope,
                &dims.coin,
                dims.horizon,
                remaining,
            );
        }
    }

    fn coin_cap_for(&self, coin: &str) -> Decimal {
        self.coin_cap_pct
            .get(coin)
            .copied()
            .or_else(|| self.coin_cap_pct.get("OTHER").copied())
            .unwrap_or(Decimal::ZERO)
    }

    fn horizon_cap_for(&self, horizon: CryptoHorizon) -> Decimal {
        self.horizon_cap_pct
            .get(&horizon)
            .copied()
            .or_else(|| self.horizon_cap_pct.get(&CryptoHorizon::Other).copied())
            .unwrap_or(Decimal::ZERO)
    }

    fn ledger_snapshot(&self) -> AllocatorLedgerSnapshot {
        let open_notional_usd = self.open.total;
        let pending_notional_usd = self.pending.total;
        let used = open_notional_usd + pending_notional_usd;
        let available_notional_usd = (self.total_cap - used).max(Decimal::ZERO);
        AllocatorLedgerSnapshot {
            domain: "crypto".to_string(),
            enabled: self.enabled,
            cap_notional_usd: self.total_cap,
            open_notional_usd,
            pending_notional_usd,
            available_notional_usd,
        }
    }

    fn deployment_ledger_snapshot(&self) -> Vec<DeploymentLedgerSnapshot> {
        let mut by_deployment: HashMap<String, (Decimal, Decimal)> = HashMap::new();

        for position in self.open.by_position.values() {
            let entry = by_deployment
                .entry(position.deployment_scope.clone())
                .or_insert((Decimal::ZERO, Decimal::ZERO));
            entry.0 += position.amount;
        }

        for position in self.pending.by_position.values() {
            let entry = by_deployment
                .entry(position.deployment_scope.clone())
                .or_insert((Decimal::ZERO, Decimal::ZERO));
            entry.1 += position.amount;
        }

        let mut rows = by_deployment
            .into_iter()
            .map(
                |(deployment_id, (open_notional_usd, pending_notional_usd))| {
                    DeploymentLedgerSnapshot {
                        deployment_id,
                        domain: "crypto".to_string(),
                        open_notional_usd,
                        pending_notional_usd,
                        total_notional_usd: open_notional_usd + pending_notional_usd,
                    }
                },
            )
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| a.deployment_id.cmp(&b.deployment_id));
        rows
    }
}

#[derive(Debug, Clone)]
struct MarketIntentDimensions {
    market_key: String,
    deployment_scope: String,
    position_key: String,
}

impl MarketIntentDimensions {
    fn from_intent(intent: &OrderIntent) -> Self {
        let market_key = intent_market_identity(intent);
        let deployment_scope = intent_deployment_scope(intent);
        let position_key = format!(
            "{}|{}|{}|{}",
            deployment_scope,
            market_key,
            intent.token_id,
            intent.side.as_str()
        );
        Self {
            market_key,
            deployment_scope,
            position_key,
        }
    }
}

#[derive(Debug, Clone)]
struct MarketPositionExposure {
    market_key: String,
    deployment_scope: String,
    amount: Decimal,
}

#[derive(Debug, Default)]
struct MarketExposureBook {
    total: Decimal,
    by_market: HashMap<String, Decimal>,
    by_position: HashMap<String, MarketPositionExposure>,
}

impl MarketExposureBook {
    fn value_for_market(&self, market_key: &str) -> Decimal {
        self.by_market
            .get(market_key)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    fn add(&mut self, dims: &MarketIntentDimensions, amount: Decimal) {
        if amount <= Decimal::ZERO {
            return;
        }

        self.total += amount;
        *self
            .by_market
            .entry(dims.market_key.clone())
            .or_insert(Decimal::ZERO) += amount;
        self.by_position
            .entry(dims.position_key.clone())
            .and_modify(|pos| {
                pos.amount += amount;
                pos.market_key = dims.market_key.clone();
                pos.deployment_scope = dims.deployment_scope.clone();
            })
            .or_insert_with(|| MarketPositionExposure {
                market_key: dims.market_key.clone(),
                deployment_scope: dims.deployment_scope.clone(),
                amount,
            });
    }

    fn subtract_from_position_key(&mut self, position_key: &str, amount: Decimal) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let mut removed = Decimal::ZERO;
        let mut market_key = None;
        let mut delete_key = false;

        if let Some(pos) = self.by_position.get_mut(position_key) {
            removed = amount.min(pos.amount);
            if removed > Decimal::ZERO {
                pos.amount -= removed;
                market_key = Some(pos.market_key.clone());
                delete_key = pos.amount <= Decimal::ZERO;
            }
        }

        if delete_key {
            self.by_position.remove(position_key);
        }

        if removed <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        self.total = (self.total - removed).max(Decimal::ZERO);
        if let Some(market) = market_key {
            if let Some(v) = self.by_market.get_mut(&market) {
                *v = (*v - removed).max(Decimal::ZERO);
                if *v == Decimal::ZERO {
                    self.by_market.remove(&market);
                }
            }
        }

        removed
    }

    fn subtract_matching_market(
        &mut self,
        deployment_scope: &str,
        market_key: &str,
        amount: Decimal,
    ) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let mut remaining = amount;
        let keys: Vec<String> = self
            .by_position
            .iter()
            .filter(|(_, p)| p.deployment_scope == deployment_scope && p.market_key == market_key)
            .map(|(k, _)| k.clone())
            .collect();

        for key in keys {
            if remaining <= Decimal::ZERO {
                break;
            }
            let removed = self.subtract_from_position_key(&key, remaining);
            remaining -= removed;
        }

        amount - remaining
    }
}

#[derive(Debug, Clone)]
struct PendingMarketIntent {
    dims: MarketIntentDimensions,
    requested_notional: Decimal,
}

#[derive(Debug)]
struct MarketCapitalAllocator {
    domain: Domain,
    domain_label: &'static str,
    enabled: bool,
    total_cap: Decimal,
    market_cap_pct: Decimal,
    auto_split_by_active_markets: bool,
    open: MarketExposureBook,
    pending: MarketExposureBook,
    pending_by_intent: HashMap<Uuid, PendingMarketIntent>,
}

impl MarketCapitalAllocator {
    fn for_sports(config: &CoordinatorConfig) -> Self {
        Self::new_for_domain(config, Domain::Sports)
    }

    fn for_politics(config: &CoordinatorConfig) -> Self {
        Self::new_for_domain(config, Domain::Politics)
    }

    fn for_economics(config: &CoordinatorConfig) -> Self {
        Self::new_for_domain(config, Domain::Economics)
    }

    fn new_for_domain(config: &CoordinatorConfig, domain: Domain) -> Self {
        let (
            domain_label,
            enabled,
            configured_cap,
            risk_cap,
            market_cap_pct,
            auto_split_by_active_markets,
        ) = match domain {
            Domain::Sports => (
                "sports",
                config.sports_allocator_enabled,
                config.sports_allocator_total_cap_usd,
                config.risk.sports_max_exposure,
                config.sports_market_cap_pct,
                config.sports_auto_split_by_active_markets,
            ),
            Domain::Politics => (
                "politics",
                config.politics_allocator_enabled,
                config.politics_allocator_total_cap_usd,
                config.risk.politics_max_exposure,
                config.politics_market_cap_pct,
                config.politics_auto_split_by_active_markets,
            ),
            Domain::Economics => (
                "economics",
                config.economics_allocator_enabled,
                config.economics_allocator_total_cap_usd,
                config.risk.economics_max_exposure,
                config.economics_market_cap_pct,
                config.economics_auto_split_by_active_markets,
            ),
            Domain::Crypto | Domain::Custom(_) => {
                panic!("market allocator does not support domain {:?}", domain)
            }
        };

        let configured_cap = configured_cap
            .or(risk_cap)
            .unwrap_or(config.risk.max_platform_exposure);
        let total_cap = risk_cap
            .map(|cap| configured_cap.min(cap))
            .unwrap_or(configured_cap)
            .max(Decimal::ZERO);

        Self {
            domain,
            domain_label,
            enabled,
            total_cap,
            market_cap_pct: Self::normalize_pct(market_cap_pct),
            auto_split_by_active_markets,
            open: MarketExposureBook::default(),
            pending: MarketExposureBook::default(),
            pending_by_intent: HashMap::new(),
        }
    }

    fn normalize_pct(value: Decimal) -> Decimal {
        if value <= Decimal::ZERO {
            Decimal::ZERO
        } else if value >= Decimal::ONE {
            Decimal::ONE
        } else {
            value
        }
    }

    fn reset_runtime_state(&mut self) {
        self.open = MarketExposureBook::default();
        self.pending = MarketExposureBook::default();
        self.pending_by_intent.clear();
    }

    fn reserve_buy(&mut self, intent: &OrderIntent) -> std::result::Result<(), String> {
        if !self.enabled || intent.domain != self.domain || !intent.is_buy {
            return Ok(());
        }

        if self.total_cap <= Decimal::ZERO {
            return Err(format!(
                "{} allocator cap is 0; buy intent blocked",
                self.domain_label
            ));
        }

        let requested = intent.notional_value();
        if requested <= Decimal::ZERO {
            return Err(format!(
                "{} buy intent has non-positive notional",
                self.domain_label
            ));
        }

        let dims = MarketIntentDimensions::from_intent(intent);

        let projected_total = self.open.total + self.pending.total + requested;
        if projected_total > self.total_cap {
            return Err(format!(
                "{} total cap exceeded: projected={} cap={}",
                self.domain_label, projected_total, self.total_cap
            ));
        }

        let market_cap = self.market_cap_for(&dims.market_key);
        let projected_market = self.open.value_for_market(&dims.market_key)
            + self.pending.value_for_market(&dims.market_key)
            + requested;
        if projected_market > market_cap {
            return Err(format!(
                "{} market cap exceeded: market={} projected={} cap={}",
                self.domain_label, dims.market_key, projected_market, market_cap
            ));
        }

        self.pending.add(&dims, requested);
        self.pending_by_intent.insert(
            intent.intent_id,
            PendingMarketIntent {
                dims,
                requested_notional: requested,
            },
        );

        Ok(())
    }

    fn release_buy_reservation(&mut self, intent_id: Uuid) {
        let Some(reservation) = self.pending_by_intent.remove(&intent_id) else {
            return;
        };
        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );
    }

    fn settle_buy_execution(
        &mut self,
        intent: &OrderIntent,
        filled_shares: u64,
        fill_price: Decimal,
    ) {
        if !self.enabled || intent.domain != self.domain || !intent.is_buy {
            return;
        }

        let reservation = self
            .pending_by_intent
            .remove(&intent.intent_id)
            .unwrap_or_else(|| PendingMarketIntent {
                dims: MarketIntentDimensions::from_intent(intent),
                requested_notional: intent.notional_value(),
            });

        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );

        if filled_shares == 0 || fill_price <= Decimal::ZERO {
            return;
        }

        let actual_notional = fill_price * Decimal::from(filled_shares);
        self.open.add(&reservation.dims, actual_notional);
    }

    fn settle_sell_execution(
        &mut self,
        intent: &OrderIntent,
        filled_shares: u64,
        execution_price: Decimal,
    ) {
        if !self.enabled || intent.domain != self.domain || intent.is_buy || filled_shares == 0 {
            return;
        }

        let dims = MarketIntentDimensions::from_intent(intent);
        let Some((reference_price, has_explicit_entry_price)) =
            sell_release_reference_price(intent, execution_price)
        else {
            return;
        };

        if reference_price <= Decimal::ZERO {
            return;
        }

        let requested_release = Decimal::from(filled_shares) * reference_price;
        let removed_by_key = self
            .open
            .subtract_from_position_key(&dims.position_key, requested_release);
        if has_explicit_entry_price && removed_by_key < requested_release {
            let remaining = requested_release - removed_by_key;
            self.open
                .subtract_matching_market(&dims.deployment_scope, &dims.market_key, remaining);
        }
    }

    fn market_cap_for(&self, market_key: &str) -> Decimal {
        let fixed_cap = self.total_cap * self.market_cap_pct;
        if !self.auto_split_by_active_markets {
            return fixed_cap;
        }

        let mut active_markets: HashSet<String> = self
            .open
            .by_market
            .iter()
            .filter(|(_, v)| **v > Decimal::ZERO)
            .map(|(k, _)| k.clone())
            .collect();

        for (k, v) in &self.pending.by_market {
            if *v > Decimal::ZERO {
                active_markets.insert(k.clone());
            }
        }

        if !market_key.is_empty() {
            active_markets.insert(market_key.to_string());
        }

        let market_count = active_markets.len().max(1) as u64;
        let dynamic_cap = self.total_cap / Decimal::from(market_count);
        dynamic_cap.min(fixed_cap)
    }

    fn ledger_snapshot(&self) -> AllocatorLedgerSnapshot {
        let open_notional_usd = self.open.total;
        let pending_notional_usd = self.pending.total;
        let used = open_notional_usd + pending_notional_usd;
        let available_notional_usd = (self.total_cap - used).max(Decimal::ZERO);
        AllocatorLedgerSnapshot {
            domain: self.domain_label.to_string(),
            enabled: self.enabled,
            cap_notional_usd: self.total_cap,
            open_notional_usd,
            pending_notional_usd,
            available_notional_usd,
        }
    }

    fn deployment_ledger_snapshot(&self) -> Vec<DeploymentLedgerSnapshot> {
        let mut by_deployment: HashMap<String, (Decimal, Decimal)> = HashMap::new();

        for position in self.open.by_position.values() {
            let entry = by_deployment
                .entry(position.deployment_scope.clone())
                .or_insert((Decimal::ZERO, Decimal::ZERO));
            entry.0 += position.amount;
        }

        for position in self.pending.by_position.values() {
            let entry = by_deployment
                .entry(position.deployment_scope.clone())
                .or_insert((Decimal::ZERO, Decimal::ZERO));
            entry.1 += position.amount;
        }

        let mut rows = by_deployment
            .into_iter()
            .map(
                |(deployment_id, (open_notional_usd, pending_notional_usd))| {
                    DeploymentLedgerSnapshot {
                        deployment_id,
                        domain: self.domain_label.to_string(),
                        open_notional_usd,
                        pending_notional_usd,
                        total_notional_usd: open_notional_usd + pending_notional_usd,
                    }
                },
            )
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| a.deployment_id.cmp(&b.deployment_id));
        rows
    }
}

impl Coordinator {
    pub fn new(
        config: CoordinatorConfig,
        executor: Arc<OrderExecutor>,
        account_id: String,
    ) -> Self {
        let (order_tx, order_rx) = mpsc::channel(256);
        let (state_tx, state_rx) = mpsc::channel(128);
        let (control_tx, control_rx) = mpsc::channel(32);

        let risk_gate = Arc::new(RiskGate::new(config.risk.clone()));
        let order_queue = Arc::new(RwLock::new(OrderQueue::new(1024)));
        let duplicate_guard = Arc::new(RwLock::new(IntentDuplicateGuard::new(
            config.duplicate_guard_window_ms,
            config.duplicate_guard_enabled,
        )));
        let crypto_allocator = Arc::new(RwLock::new(CryptoCapitalAllocator::new(&config)));
        let sports_allocator = Arc::new(RwLock::new(MarketCapitalAllocator::for_sports(&config)));
        let politics_allocator =
            Arc::new(RwLock::new(MarketCapitalAllocator::for_politics(&config)));
        let economics_allocator =
            Arc::new(RwLock::new(MarketCapitalAllocator::for_economics(&config)));
        let positions = Arc::new(PositionAggregator::new());
        let global_state = Arc::new(RwLock::new(GlobalState::new()));
        let ingress_mode = Arc::new(RwLock::new(IngressMode::Running));
        let governance_policy = Arc::new(RwLock::new(GovernancePolicy::from_config(&config)));
        let domain_ingress_mode = Arc::new(RwLock::new(HashMap::new()));
        let stale_heartbeat_warn_at = Arc::new(RwLock::new(HashMap::new()));
        let account_id = if account_id.trim().is_empty() {
            "default".to_string()
        } else {
            account_id
        };

        Self {
            config,
            account_id,
            risk_gate,
            order_queue,
            duplicate_guard,
            crypto_allocator,
            sports_allocator,
            politics_allocator,
            economics_allocator,
            positions,
            executor,
            global_state,
            execution_log_pool: None,
            governance_store_pool: None,
            ingress_mode,
            domain_ingress_mode,
            governance_policy,
            stale_heartbeat_warn_at,
            order_tx,
            order_rx,
            state_tx,
            state_rx,
            control_tx,
            control_rx,
            agent_commands: HashMap::new(),
        }
    }

    /// Enable DB logging for order execution outcomes (including dry-run).
    pub fn set_execution_log_pool(&mut self, pool: PgPool) {
        self.execution_log_pool = Some(pool);
    }

    /// Enable DB persistence for coordinator governance policy.
    pub fn set_governance_store_pool(&mut self, pool: PgPool) {
        self.governance_store_pool = Some(pool);
    }

    /// Restore runtime governance policy from DB (if a persisted row exists).
    pub async fn load_persisted_governance_policy(&self) -> Result<()> {
        let Some(pool) = self.governance_store_pool.as_ref() else {
            return Ok(());
        };

        let Some(policy) = load_governance_policy(pool, &self.account_id).await? else {
            return Ok(());
        };

        let snapshot = policy.to_snapshot();
        let mut state = self.governance_policy.write().await;
        *state = policy;
        info!(
            account_id = %self.account_id,
            updated_by = %snapshot.updated_by,
            updated_at = %snapshot.updated_at,
            "restored governance policy from DB"
        );
        Ok(())
    }

    /// Rebuild runtime position/allocator state from persisted execution fills.
    ///
    /// This prevents cold-start underestimation of account exposure when a process restarts.
    pub async fn restore_runtime_state_from_execution_log(&self) -> Result<()> {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return Ok(());
        };

        let today = Utc::now().date_naive();
        let window_start = DateTime::<Utc>::from_naive_utc_and_offset(
            today
                .and_hms_opt(0, 0, 0)
                .expect("00:00:00 is always a valid UTC time"),
            Utc,
        );
        let window_end = window_start + ChronoDuration::days(1);
        let dry_run = self.executor.is_dry_run();

        let fills = load_execution_log_fills(pool, &self.account_id, dry_run).await?;
        let outcomes_today =
            load_execution_log_outcomes(pool, &self.account_id, dry_run, window_start, window_end)
                .await?;

        if fills.is_empty() && outcomes_today.is_empty() {
            return Ok(());
        }

        self.positions.clear().await;
        {
            let mut allocator = self.crypto_allocator.write().await;
            allocator.reset_runtime_state();
        }
        {
            let mut allocator = self.sports_allocator.write().await;
            allocator.reset_runtime_state();
        }
        {
            let mut allocator = self.politics_allocator.write().await;
            allocator.reset_runtime_state();
        }
        {
            let mut allocator = self.economics_allocator.write().await;
            allocator.reset_runtime_state();
        }

        let restored_fill_count = fills.len();
        let mut restored_agents = HashSet::new();
        let mut daily_total_pnl = Decimal::ZERO;
        let mut daily_domain_pnl: HashMap<Domain, Decimal> = HashMap::new();
        let mut daily_agent_pnl: HashMap<String, Decimal> = HashMap::new();

        for fill in fills {
            let mut intent = OrderIntent::new(
                fill.agent_id.clone(),
                fill.domain,
                fill.market_slug.clone(),
                fill.token_id.clone(),
                fill.side,
                fill.is_buy,
                fill.filled_shares,
                fill.fill_price,
            );
            intent.intent_id = fill.intent_id;
            intent.created_at = fill.executed_at;
            intent.metadata = fill.metadata;

            self.settle_domain_success(&intent, fill.filled_shares, fill.fill_price)
                .await;

            if fill.is_buy {
                let _ = self
                    .positions
                    .open_position(
                        &fill.agent_id,
                        fill.domain,
                        &fill.market_slug,
                        &fill.token_id,
                        fill.side,
                        fill.filled_shares,
                        fill.fill_price,
                    )
                    .await;
            } else {
                let realized_pnl = self
                    .apply_sell_fill_to_positions(&intent, fill.filled_shares, fill.fill_price)
                    .await;
                if fill.executed_at >= window_start && fill.executed_at < window_end {
                    daily_total_pnl += realized_pnl;
                    *daily_domain_pnl.entry(fill.domain).or_insert(Decimal::ZERO) += realized_pnl;
                    *daily_agent_pnl
                        .entry(fill.agent_id.clone())
                        .or_insert(Decimal::ZERO) += realized_pnl;
                }
            }
            restored_agents.insert(fill.agent_id);
        }

        let mut daily_order_count: u32 = 0;
        let mut daily_success_count: u32 = 0;
        let mut daily_failure_count: u32 = 0;
        let mut global_consecutive_failures: u32 = 0;
        let mut per_agent_consecutive_failures: HashMap<String, u32> = HashMap::new();
        let mut last_risk_event_at: Option<DateTime<Utc>> = None;

        for outcome in outcomes_today {
            daily_order_count = daily_order_count.saturating_add(1);
            last_risk_event_at = Some(outcome.executed_at);
            if outcome.is_failure {
                daily_failure_count = daily_failure_count.saturating_add(1);
                global_consecutive_failures = global_consecutive_failures.saturating_add(1);
                let entry = per_agent_consecutive_failures
                    .entry(outcome.agent_id)
                    .or_insert(0);
                *entry = entry.saturating_add(1);
            } else {
                daily_success_count = daily_success_count.saturating_add(1);
                global_consecutive_failures = 0;
                per_agent_consecutive_failures.insert(outcome.agent_id, 0);
            }
        }

        self.risk_gate
            .restore_runtime_counters(
                today,
                daily_total_pnl,
                daily_domain_pnl,
                daily_order_count,
                daily_success_count,
                daily_failure_count,
                global_consecutive_failures,
                daily_agent_pnl,
                per_agent_consecutive_failures,
                last_risk_event_at,
            )
            .await;

        for agent_id in &restored_agents {
            self.refresh_risk_exposure_for_agent(agent_id).await;
        }
        self.refresh_global_state().await;

        info!(
            account_id = %self.account_id,
            fill_count = restored_fill_count,
            restored_agents = restored_agents.len(),
            daily_order_count,
            daily_success_count,
            daily_failure_count,
            global_consecutive_failures,
            "restored coordinator runtime state from execution log"
        );
        Ok(())
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
            crypto_allocator: self.crypto_allocator.clone(),
            sports_allocator: self.sports_allocator.clone(),
            politics_allocator: self.politics_allocator.clone(),
            economics_allocator: self.economics_allocator.clone(),
            positions: self.positions.clone(),
            ingress_mode: self.ingress_mode.clone(),
            domain_ingress_mode: self.domain_ingress_mode.clone(),
            governance_policy: self.governance_policy.clone(),
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

    /// Register an agent and return its command receiver
    pub fn register_agent(
        &mut self,
        agent_id: String,
        domain: Domain,
        risk_params: AgentRiskParams,
    ) -> mpsc::Receiver<CoordinatorCommand> {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        self.agent_commands
            .insert(agent_id.clone(), AgentCommandChannel { domain, tx: cmd_tx });

        // Register with risk gate (fire-and-forget via spawn since we're not async here)
        let risk_gate = self.risk_gate.clone();
        let id = agent_id.clone();
        tokio::spawn(async move {
            risk_gate
                .register_agent_with_domain(&id, domain, risk_params)
                .await;
        });

        info!(agent_id, "agent registered with coordinator");
        cmd_rx
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

    fn domain_for_agent(&self, agent_id: &str) -> Option<Domain> {
        self.agent_commands.get(agent_id).map(|entry| entry.domain)
    }

    fn should_apply_domain_cmd(&self, entry: &AgentCommandChannel, target: Domain) -> bool {
        entry.domain == target
    }

    async fn set_domain_mode(&self, domain: Domain, mode: IngressMode) {
        let mut domain_modes = self.domain_ingress_mode.write().await;
        match mode {
            IngressMode::Running => {
                domain_modes.remove(&domain);
            }
            _ => {
                domain_modes.insert(domain, mode);
            }
        }
    }

    /// Pause all agents
    pub async fn pause_all(&self) {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Paused;
        }
        self.domain_ingress_mode.write().await.clear();
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::Pause).await {
                warn!(agent_id = %id, error = %e, "failed to send pause");
            }
        }
    }

    /// Resume all agents
    pub async fn resume_all(&self) {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Running;
        }
        self.domain_ingress_mode.write().await.clear();
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::Resume).await {
                warn!(agent_id = %id, error = %e, "failed to send resume");
            }
        }
    }

    /// Force-close all agents (best-effort)
    pub async fn force_close_all(&self) {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Halted;
        }
        self.domain_ingress_mode.write().await.clear();
        info!("coordinator: sending force-close to all agents");
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::ForceClose).await {
                warn!(agent_id = %id, error = %e, "failed to send force-close");
            }
        }
    }

    /// Shutdown all agents gracefully
    pub async fn shutdown(&self) {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Halted;
        }
        self.domain_ingress_mode.write().await.clear();
        info!("coordinator: sending shutdown to all agents");
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::Shutdown).await {
                warn!(agent_id = %id, error = %e, "failed to send shutdown");
            }
        }
    }

    /// Pause one domain
    pub async fn pause_domain(&self, domain: Domain) {
        self.set_domain_mode(domain, IngressMode::Paused).await;
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
        self.set_domain_mode(domain, IngressMode::Running).await;
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
        self.set_domain_mode(domain, IngressMode::Halted).await;
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
        self.set_domain_mode(domain, IngressMode::Halted).await;
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::Shutdown).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain shutdown");
                }
            }
        }
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

        loop {
            tokio::select! {
                // --- Control commands (pause/resume/force-close) ---
                Some(cmd) = self.control_rx.recv() => {
                    match cmd {
                        CoordinatorControlCommand::PauseAll => self.pause_all().await,
                        CoordinatorControlCommand::ResumeAll => self.resume_all().await,
                        CoordinatorControlCommand::ForceCloseAll => self.force_close_all().await,
                        CoordinatorControlCommand::ShutdownAll => self.shutdown().await,
                        CoordinatorControlCommand::PauseDomain(domain) => self.pause_domain(domain).await,
                        CoordinatorControlCommand::ResumeDomain(domain) => {
                            self.resume_domain(domain).await
                        }
                        CoordinatorControlCommand::ForceCloseDomain(domain) => {
                            self.force_close_domain(domain).await
                        }
                        CoordinatorControlCommand::ShutdownDomain(domain) => {
                            self.shutdown_domain(domain).await
                        }
                    }
                }

                // --- Incoming order intents ---
                Some(intent) = self.order_rx.recv() => {
                    self.handle_order_intent(intent).await;
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
                    self.shutdown().await;
                    break;
                }
            }
        }

        info!("coordinator: main loop exited");
    }

    /// Risk-check an incoming order intent and enqueue if passed
    async fn handle_order_intent(&self, intent: OrderIntent) {
        let agent_id = intent.agent_id.clone();
        let intent_id = intent.intent_id;

        if let Some(reason) = buy_intent_missing_deployment_reason(&intent) {
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked due to missing deployment identity"
            );
            return;
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

        if let Some(reason) = self.check_governance_policy(&intent).await {
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by global governance policy"
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

        match self.risk_gate.check_order(&intent).await {
            RiskCheckResult::Passed => {
                if let Some(reason) = self.reserve_domain_capital(&intent).await {
                    self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                        .await;
                    warn!(
                        %agent_id, %intent_id, reason = %reason,
                        "order blocked by domain allocator"
                    );
                    return;
                }

                self.persist_risk_decision(&intent, "PASSED", None, None)
                    .await;
                let mut queue = self.order_queue.write().await;
                match queue.enqueue(intent) {
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
            }
            RiskCheckResult::Blocked(reason) => {
                self.persist_risk_decision(&intent, "BLOCKED", Some(reason.to_string()), None)
                    .await;
                warn!(
                    %agent_id, %intent_id,
                    reason = ?reason,
                    "order blocked by risk gate"
                );
            }
            RiskCheckResult::Adjusted(suggestion) => {
                self.persist_risk_decision(
                    &intent,
                    "ADJUSTED",
                    None,
                    Some((suggestion.max_shares, suggestion.reason.clone())),
                )
                .await;
                info!(
                    %agent_id, %intent_id,
                    max_shares = suggestion.max_shares,
                    reason = %suggestion.reason,
                    "order adjusted by risk gate — dropping (agent should resubmit)"
                );
            }
        }
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
        let batch = {
            let mut queue = self.order_queue.write().await;
            queue.cleanup_expired();
            queue.dequeue_batch(self.config.batch_size)
        };

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

        let mut realized_pnl = Decimal::ZERO;
        let mut remaining = filled_shares;
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
        result: Option<&crate::strategy::executor::ExecutionResult>,
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
        result: Option<&crate::strategy::executor::ExecutionResult>,
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
        result: Option<&crate::strategy::executor::ExecutionResult>,
        queue_delay_ms: Option<i64>,
        config_hash: Option<String>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let expected_price = request.limit_price;
        let executed_price = result.and_then(|r| r.avg_fill_price);
        let execution_latency_ms = result.map(|r| r.elapsed_ms as i64);
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
        let status = result
            .map(|r| format!("{:?}", r.status))
            .unwrap_or_else(|| "Failed".to_string());

        let result = sqlx::query(
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

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist execution analysis"
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
        let circuit_breaker_events = self.risk_gate.circuit_breaker_events().await;
        let queue_stats = self.order_queue.read().await.stats();
        let total_realized = self.positions.total_realized_pnl().await;

        let mut state = self.global_state.write().await;
        state.portfolio = portfolio;
        state.positions = positions;
        state.risk_state = risk_state;
        state.daily_pnl = daily_pnl;
        state.daily_loss_limit = daily_loss_limit;
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

    fn stable_idempotency_key(account_id: &str, intent: &OrderIntent) -> String {
        if let Some(key) = intent
            .metadata
            .get("idempotency_key")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
        {
            return Self::sanitize_idempotency_component(key);
        }

        let deployment_id = intent_deployment_scope(intent);

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
            "acct:{account}|dep:{dep}|dom:{dom}|mkt:{mkt}|side:{side}|kind:{kind}|bucket:{bucket}",
            account = account_id,
            dep = deployment_id,
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

        let idempotency_key = Self::stable_idempotency_key(&self.account_id, intent);
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
    use crate::platform::{AgentStatus, Domain, OrderPriority, QueueStats};
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

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
        let reason = sell_reduce_only_violation_reason(&intent, 0);
        assert!(reason
            .unwrap_or_default()
            .contains("no tracked open shares"));
    }

    #[test]
    fn test_sell_reduce_only_violation_when_requested_exceeds_tracked() {
        let intent = make_intent(false, OrderPriority::Normal);
        let reason = sell_reduce_only_violation_reason(&intent, 30);
        assert!(reason
            .unwrap_or_default()
            .contains("requested shares 100 exceeds tracked open shares 30"));
    }

    #[test]
    fn test_sell_reduce_only_allows_with_sufficient_tracked_shares() {
        let intent = make_intent(false, OrderPriority::Normal);
        assert!(sell_reduce_only_violation_reason(&intent, 100).is_none());
        assert!(sell_reduce_only_violation_reason(&intent, 150).is_none());
    }

    #[test]
    fn test_duplicate_guard_blocks_repeated_buy_within_window() {
        let mut guard = IntentDuplicateGuard::new(1000, true);
        let now = Utc::now();
        let intent = make_intent(true, OrderPriority::Normal);

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(300))
            .is_some());
    }

    #[test]
    fn test_duplicate_guard_allows_after_window() {
        let mut guard = IntentDuplicateGuard::new(500, true);
        let now = Utc::now();
        let intent = make_intent(true, OrderPriority::Normal);

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(700))
            .is_none());
    }

    #[test]
    fn test_duplicate_guard_blocks_same_market_even_if_token_differs() {
        let mut guard = IntentDuplicateGuard::new(1_000, true);
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
        let mut guard = IntentDuplicateGuard::new(1_000, true);
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
        let mut guard = IntentDuplicateGuard::new(1_000, true);
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
    fn test_duplicate_guard_does_not_block_sells() {
        let mut guard = IntentDuplicateGuard::new(10_000, true);
        let now = Utc::now();
        let intent = make_intent(false, OrderPriority::Normal);

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(10))
            .is_none());
    }

    #[test]
    fn test_duplicate_guard_skips_critical_orders() {
        let mut guard = IntentDuplicateGuard::new(10_000, true);
        let now = Utc::now();
        let intent = make_intent(true, OrderPriority::Critical);

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(10))
            .is_none());
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

        let key = Coordinator::stable_idempotency_key("acct-main", &intent);

        assert_ne!(key, intent.intent_id.to_string());
        assert!(key.contains("acct-main"));
        assert!(key.contains("crypto.pm.btc.15m.patternmem"));
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

        let first_key = Coordinator::stable_idempotency_key("acct-main", &first);
        let second_key = Coordinator::stable_idempotency_key("acct-main", &second);

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

        let first_key = Coordinator::stable_idempotency_key("acct-main", &first);
        let second_key = Coordinator::stable_idempotency_key("acct-main", &second);
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
