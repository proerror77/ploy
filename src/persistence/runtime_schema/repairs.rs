use crate::error::Result;
use sqlx::PgPool;
use tracing::warn;

mod idempotency_repairs;
mod runtime_event_repairs;
mod trade_state_repairs;

pub(crate) async fn ensure_schema_repairs(pool: &PgPool) -> Result<()> {
    let ddl = build_repairs_sql();
    let result = sqlx::query(&ddl).execute(pool).await;

    if let Err(e) = result {
        warn!(
            error = %e,
            "schema repair DDL skipped at startup (run migration 013 as postgres for full repair)"
        );
    }

    Ok(())
}

fn build_repairs_sql() -> String {
    format!(
        r#"
        DO $$
        BEGIN
            {}
            {}
            {}
        END $$;
        "#,
        trade_state_repairs::SQL,
        runtime_event_repairs::SQL,
        idempotency_repairs::SQL,
    )
}
