use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::domain::StrategyState;
use crate::error::{PloyError, Result};

use super::PostgresStore;

/// Persisted strategy state
#[derive(Debug, Clone)]
pub struct PersistedState {
    pub current_state: StrategyState,
    pub current_round_id: Option<i32>,
    pub current_cycle_id: Option<i32>,
    pub risk_state: String,
    pub last_updated: DateTime<Utc>,
}

impl PostgresStore {
    /// Get current strategy state
    pub async fn get_strategy_state(&self) -> Result<PersistedState> {
        let row = sqlx::query(
            r#"
            SELECT current_state, current_round_id, current_cycle_id, risk_state, last_updated
            FROM strategy_state WHERE id = 1
            "#,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(PersistedState {
            current_state: StrategyState::try_from(row.get::<&str, _>("current_state"))
                .map_err(PloyError::Internal)?,
            current_round_id: row.get("current_round_id"),
            current_cycle_id: row.get("current_cycle_id"),
            risk_state: row.get("risk_state"),
            last_updated: row.get("last_updated"),
        })
    }

    /// Update strategy state
    pub async fn update_strategy_state(
        &self,
        state: StrategyState,
        round_id: Option<i32>,
        cycle_id: Option<i32>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE strategy_state SET
                current_state = $1,
                current_round_id = $2,
                current_cycle_id = $3,
                last_updated = NOW()
            WHERE id = 1
            "#,
        )
        .bind(state.as_str())
        .bind(round_id)
        .bind(cycle_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
