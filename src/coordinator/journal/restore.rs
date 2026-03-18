use super::*;
use crate::coordinator::DrawdownSnapshot;
use crate::domain::{Domain, Side};
use chrono::NaiveDate;
use sqlx::Row;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub(in crate::coordinator) struct PersistedExecutionFill {
    pub(in crate::coordinator) intent_id: uuid::Uuid,
    pub(in crate::coordinator) agent_id: String,
    pub(in crate::coordinator) domain: Domain,
    pub(in crate::coordinator) market_slug: String,
    pub(in crate::coordinator) token_id: String,
    pub(in crate::coordinator) side: Side,
    pub(in crate::coordinator) is_buy: bool,
    pub(in crate::coordinator) filled_shares: u64,
    pub(in crate::coordinator) fill_price: Decimal,
    pub(in crate::coordinator) executed_at: DateTime<Utc>,
    pub(in crate::coordinator) metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub(in crate::coordinator) struct PersistedExecutionOutcome {
    pub(in crate::coordinator) agent_id: String,
    pub(in crate::coordinator) executed_at: DateTime<Utc>,
    pub(in crate::coordinator) is_failure: bool,
}

#[derive(Debug, Clone)]
pub(in crate::coordinator) struct PersistedRiskRuntimeState {
    pub(in crate::coordinator) drawdown: DrawdownSnapshot,
    pub(in crate::coordinator) daily_date: Option<NaiveDate>,
    pub(in crate::coordinator) daily_pnl: Decimal,
    pub(in crate::coordinator) risk_state_raw: String,
}

#[derive(Debug, Clone)]
pub(in crate::coordinator) struct ExecutionRestoreData {
    pub(in crate::coordinator) fills: Vec<PersistedExecutionFill>,
    pub(in crate::coordinator) outcomes_today: Vec<PersistedExecutionOutcome>,
}

pub(super) async fn load_risk_runtime_state(
    pool: &PgPool,
    account_id: &str,
) -> Result<Option<PersistedRiskRuntimeState>> {
    let row = sqlx::query(
        r#"
        SELECT
            current_equity,
            equity_peak,
            current_drawdown,
            max_drawdown_observed,
            daily_pnl,
            daily_date,
            risk_state
        FROM risk_runtime_state
        WHERE account_id = $1
        "#,
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    Ok(Some(PersistedRiskRuntimeState {
        drawdown: DrawdownSnapshot {
            current_equity: row.try_get("current_equity").unwrap_or(Decimal::ZERO),
            equity_peak: row.try_get("equity_peak").unwrap_or(Decimal::ZERO),
            current_drawdown: row.try_get("current_drawdown").unwrap_or(Decimal::ZERO),
            max_drawdown_observed: row
                .try_get("max_drawdown_observed")
                .unwrap_or(Decimal::ZERO),
        },
        daily_date: row.try_get("daily_date").ok(),
        daily_pnl: row.try_get("daily_pnl").unwrap_or(Decimal::ZERO),
        risk_state_raw: row.try_get("risk_state").unwrap_or_default(),
    }))
}

pub(super) async fn load_execution_restore_data(
    pool: &PgPool,
    account_id: &str,
    dry_run: bool,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> Result<Option<ExecutionRestoreData>> {
    let fills = load_execution_log_fills(pool, account_id, dry_run).await?;
    let outcomes_today =
        load_execution_log_outcomes(pool, account_id, dry_run, window_start, window_end).await?;

    Ok(Some(ExecutionRestoreData {
        fills,
        outcomes_today,
    }))
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
                .and_then(|value| value.strip_suffix(')'))
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
        if let Some(value) = value.as_str() {
            metadata.insert(key.clone(), value.to_string());
        } else {
            metadata.insert(key.clone(), value.to_string());
        }
    }

    metadata
}

fn execution_error_is_failure(error: Option<&str>) -> bool {
    error
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .is_some()
}

async fn load_execution_log_fills(
    pool: &PgPool,
    account_id: &str,
    dry_run: bool,
) -> Result<Vec<PersistedExecutionFill>> {
    let rows = sqlx::query_as::<
        _,
        (
            uuid::Uuid,
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
    .map_err(|error| {
        crate::error::PloyError::Internal(format!("load execution log fills: {}", error))
    })?;

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
            tracing::warn!(
                account_id = %account_id,
                intent_id = %intent_id,
                domain = %domain_raw,
                "skipping execution-log row with unknown domain during restore"
            );
            continue;
        };
        let Some(side) = parse_persisted_side(&side_raw) else {
            tracing::warn!(
                account_id = %account_id,
                intent_id = %intent_id,
                side = %side_raw,
                "skipping execution-log row with unknown side during restore"
            );
            continue;
        };
        let Ok(filled_shares) = u64::try_from(filled_shares_raw) else {
            tracing::warn!(
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
            tracing::warn!(
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
    .map_err(|error| {
        crate::error::PloyError::Internal(format!("load execution log outcomes: {}", error))
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

#[cfg(test)]
mod tests {
    use super::{
        execution_error_is_failure, parse_persisted_domain, parse_persisted_side,
        string_metadata_from_json,
    };
    use crate::domain::Domain;

    #[test]
    fn test_execution_error_is_failure_treats_blank_as_success() {
        assert!(execution_error_is_failure(Some("transport timeout")));
        assert!(!execution_error_is_failure(Some("   ")));
        assert!(!execution_error_is_failure(None));
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
}
