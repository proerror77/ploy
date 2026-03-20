use super::{
    governance_domain_label, governance_domain_snapshot_label, parse_governance_domain,
    parse_ingress_mode, GovernancePolicy, GovernancePolicyHistoryEntry,
    GovernanceRuntimeStateSnapshot, IngressMode, PersistedGovernanceState, Utc,
};
use crate::error::{PloyError, Result};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};

fn governance_policy_blocked_domains_sorted(policy: &GovernancePolicy) -> Vec<String> {
    let mut blocked_domains = policy
        .blocked_domains
        .iter()
        .map(|d| governance_domain_label(*d).to_string())
        .collect::<Vec<_>>();
    blocked_domains.sort();
    blocked_domains
}

pub(in crate::coordinator) fn clamp_governance_history_limit(limit: usize) -> usize {
    limit.clamp(1, 500)
}

pub(in crate::coordinator) async fn persist_governance_policy(
    pool: &PgPool,
    account_id: &str,
    policy: &GovernancePolicy,
    runtime_state: &GovernanceRuntimeStateSnapshot,
) -> Result<()> {
    let blocked_domains = governance_policy_blocked_domains_sorted(policy);
    let domain_ingress_modes = runtime_state
        .domain_ingress_modes
        .iter()
        .map(|(domain, mode)| {
            (
                governance_domain_snapshot_label(*domain),
                mode.as_str().to_string(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut paused_agent_ids = runtime_state
        .paused_agent_ids
        .iter()
        .map(|agent_id| agent_id.trim().to_string())
        .filter(|agent_id| !agent_id.is_empty())
        .collect::<Vec<_>>();
    paused_agent_ids.sort();
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| PloyError::Internal(format!("begin governance policy tx: {}", e)))?;

    sqlx::query(
        r#"
        INSERT INTO coordinator_governance_policies (
            account_id,
            block_new_intents,
            blocked_domains,
            ingress_mode,
            domain_ingress_modes,
            paused_agent_ids,
            max_intent_notional_usd,
            max_total_notional_usd,
            updated_at,
            updated_by,
            reason
        )
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
        ON CONFLICT (account_id) DO UPDATE SET
            block_new_intents = EXCLUDED.block_new_intents,
            blocked_domains = EXCLUDED.blocked_domains,
            ingress_mode = EXCLUDED.ingress_mode,
            domain_ingress_modes = EXCLUDED.domain_ingress_modes,
            paused_agent_ids = EXCLUDED.paused_agent_ids,
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
    .bind(runtime_state.ingress_mode.as_str())
    .bind(sqlx::types::Json(domain_ingress_modes))
    .bind(sqlx::types::Json(paused_agent_ids))
    .bind(policy.max_intent_notional_usd)
    .bind(policy.max_total_notional_usd)
    .bind(policy.updated_at)
    .bind(policy.updated_by.clone())
    .bind(policy.reason.clone())
    .execute(&mut *tx)
    .await
    .map_err(|e| PloyError::Internal(format!("persist governance policy: {}", e)))?;

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
    .map_err(|e| PloyError::Internal(format!("append governance policy history entry: {}", e)))?;

    tx.commit()
        .await
        .map_err(|e| PloyError::Internal(format!("commit governance policy tx: {}", e)))?;

    Ok(())
}

pub(in crate::coordinator) async fn load_governance_policy_history(
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
    .map_err(|e| PloyError::Internal(format!("load governance policy history: {}", e)))?;

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

pub(in crate::coordinator) async fn load_governance_policy(
    pool: &PgPool,
    account_id: &str,
) -> Result<Option<PersistedGovernanceState>> {
    let row = sqlx::query_as::<
        _,
        (
            bool,
            sqlx::types::Json<Vec<String>>,
            String,
            sqlx::types::Json<HashMap<String, String>>,
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
            ingress_mode,
            domain_ingress_modes,
            paused_agent_ids,
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
    .map_err(|e| PloyError::Internal(format!("load governance policy: {}", e)))?;

    let Some((
        block_new_intents,
        sqlx::types::Json(raw_blocked_domains),
        raw_ingress_mode,
        sqlx::types::Json(raw_domain_ingress_modes),
        sqlx::types::Json(raw_paused_agent_ids),
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
        match parse_governance_domain(&raw) {
            Some(domain) => {
                blocked_domains.insert(domain);
            }
            None => unknown_domains.push(raw),
        }
    }
    if !unknown_domains.is_empty() {
        tracing::warn!(
            account_id = %account_id,
            domains = ?unknown_domains,
            "ignoring unknown governance blocked domains from DB"
        );
    }

    let max_intent_notional_usd = max_intent_notional_usd.filter(|v| *v > Decimal::ZERO);
    let max_total_notional_usd = max_total_notional_usd.filter(|v| *v > Decimal::ZERO);
    let ingress_mode = parse_ingress_mode(&raw_ingress_mode).unwrap_or_else(|| {
        tracing::warn!(
            account_id = %account_id,
            ingress_mode = %raw_ingress_mode,
            "ignoring unknown governance ingress mode from DB; defaulting to running"
        );
        IngressMode::Running
    });
    let mut domain_ingress_modes = HashMap::new();
    for (raw_domain, raw_mode) in raw_domain_ingress_modes {
        let Some(domain) = parse_governance_domain(&raw_domain) else {
            tracing::warn!(
                account_id = %account_id,
                domain = %raw_domain,
                "ignoring unknown governance ingress domain from DB"
            );
            continue;
        };
        let Some(mode) = parse_ingress_mode(&raw_mode) else {
            tracing::warn!(
                account_id = %account_id,
                domain = %raw_domain,
                mode = %raw_mode,
                "ignoring unknown governance ingress mode from DB"
            );
            continue;
        };
        if mode != IngressMode::Running {
            domain_ingress_modes.insert(domain, mode);
        }
    }
    let paused_agent_ids = raw_paused_agent_ids
        .into_iter()
        .map(|agent_id| agent_id.trim().to_string())
        .filter(|agent_id| !agent_id.is_empty())
        .collect::<HashSet<_>>();
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

    Ok(Some(PersistedGovernanceState {
        policy: GovernancePolicy {
            block_new_intents,
            blocked_domains,
            max_intent_notional_usd,
            max_total_notional_usd,
            updated_at,
            updated_by,
            reason,
            metadata: HashMap::new(),
        },
        runtime_state: GovernanceRuntimeStateSnapshot {
            ingress_mode,
            domain_ingress_modes,
            paused_agent_ids,
        },
    }))
}
