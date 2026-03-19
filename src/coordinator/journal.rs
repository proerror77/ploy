use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::str::FromStr;

use sqlx::PgPool;

use crate::coordinator::OrderIntent;
use crate::error::Result;

#[path = "journal/execution_writes.rs"]
mod execution_writes;
#[path = "journal/ingress_writes.rs"]
mod ingress_writes;
#[path = "journal/restore.rs"]
mod restore;

pub(super) use self::restore::{ExecutionRestoreData, PersistedRiskRuntimeState};

#[derive(Debug, Clone)]
pub(super) struct ExecutionJournal {
    account_id: String,
    pool: Option<PgPool>,
}

impl ExecutionJournal {
    pub(super) fn new(account_id: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            pool: None,
        }
    }

    pub(super) fn set_pool(&mut self, pool: PgPool) {
        self.pool = Some(pool);
    }

    pub(super) async fn load_risk_runtime_state(
        &self,
    ) -> Result<Option<PersistedRiskRuntimeState>> {
        let Some(pool) = self.pool.as_ref() else {
            return Ok(None);
        };

        restore::load_risk_runtime_state(pool, &self.account_id).await
    }

    pub(super) async fn load_execution_restore_data(
        &self,
        dry_run: bool,
        window_start: DateTime<Utc>,
        window_end: DateTime<Utc>,
    ) -> Result<Option<ExecutionRestoreData>> {
        let Some(pool) = self.pool.as_ref() else {
            return Ok(None);
        };

        restore::load_execution_restore_data(
            pool,
            &self.account_id,
            dry_run,
            window_start,
            window_end,
        )
        .await
    }
}

fn metadata_decimal(intent: &OrderIntent, key: &str) -> Option<Decimal> {
    intent
        .metadata
        .get(key)
        .and_then(|value| Decimal::from_str(value).ok())
}
