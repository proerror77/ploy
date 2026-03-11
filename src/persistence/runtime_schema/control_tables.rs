use crate::error::Result;
use sqlx::PgPool;

pub(crate) async fn ensure_accounts_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS accounts (
            account_id TEXT PRIMARY KEY,
            wallet_address TEXT,
            label TEXT,
            metadata JSONB,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO accounts (account_id, label)
        VALUES ('default', 'Default')
        ON CONFLICT (account_id) DO NOTHING
        "#,
    )
    .execute(pool)
    .await?;

    let _ = sqlx::query(
        r#"
        DO $$
        BEGIN
            IF to_regclass('public.accounts') IS NULL THEN
                RETURN;
            END IF;

            BEGIN
                DROP TRIGGER IF EXISTS update_accounts_updated_at ON accounts;
                CREATE TRIGGER update_accounts_updated_at
                BEFORE UPDATE ON accounts
                FOR EACH ROW
                EXECUTE FUNCTION update_updated_at_column();
            EXCEPTION WHEN undefined_function THEN
                NULL;
            END;
        END $$;
        "#,
    )
    .execute(pool)
    .await;

    Ok(())
}

pub(crate) async fn upsert_account_from_config(
    pool: &PgPool,
    account_id: &str,
    cfg: &crate::config::AccountConfig,
) -> Result<()> {
    let metadata = serde_json::json!({
        "source": "ploy",
        "config_wallet_address": cfg.wallet_address.as_deref(),
        "config_label": cfg.label.as_deref(),
    });

    sqlx::query(
        r#"
        INSERT INTO accounts (account_id, wallet_address, label, metadata)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (account_id) DO UPDATE SET
            wallet_address = COALESCE(EXCLUDED.wallet_address, accounts.wallet_address),
            label = COALESCE(EXCLUDED.label, accounts.label),
            metadata = COALESCE(EXCLUDED.metadata, accounts.metadata),
            updated_at = NOW()
        "#,
    )
    .bind(account_id)
    .bind(cfg.wallet_address.as_deref())
    .bind(cfg.label.as_deref())
    .bind(sqlx::types::Json(metadata))
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn ensure_agent_order_executions_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS agent_order_executions (
            id BIGSERIAL PRIMARY KEY,
            account_id TEXT NOT NULL DEFAULT 'default',
            agent_id TEXT NOT NULL,
            intent_id UUID NOT NULL,
            domain TEXT NOT NULL,
            market_slug TEXT NOT NULL,
            token_id TEXT NOT NULL,
            market_side TEXT NOT NULL CHECK (market_side IN ('UP', 'DOWN')),
            is_buy BOOLEAN NOT NULL,
            shares BIGINT NOT NULL,
            limit_price NUMERIC(10,6) NOT NULL,
            order_id TEXT,
            status TEXT NOT NULL,
            filled_shares BIGINT NOT NULL DEFAULT 0,
            avg_fill_price NUMERIC(10,6),
            elapsed_ms BIGINT,
            dry_run BOOLEAN NOT NULL DEFAULT FALSE,
            error TEXT,
            intent_created_at TIMESTAMPTZ,
            metadata JSONB,
            executed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            UNIQUE(intent_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "ALTER TABLE agent_order_executions ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default'",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_agent_order_executions_time ON agent_order_executions(executed_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_agent_order_executions_agent_time ON agent_order_executions(agent_id, executed_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_agent_order_executions_token_time ON agent_order_executions(token_id, executed_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_agent_order_executions_account_time ON agent_order_executions(account_id, executed_at DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn ensure_coordinator_governance_policies_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS coordinator_governance_policies (
            account_id TEXT PRIMARY KEY,
            block_new_intents BOOLEAN NOT NULL DEFAULT FALSE,
            blocked_domains JSONB NOT NULL DEFAULT '[]'::jsonb,
            ingress_mode TEXT NOT NULL DEFAULT 'running',
            domain_ingress_modes JSONB NOT NULL DEFAULT '{}'::jsonb,
            paused_agent_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
            max_intent_notional_usd NUMERIC,
            max_total_notional_usd NUMERIC,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_by TEXT NOT NULL,
            reason TEXT
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "ALTER TABLE coordinator_governance_policies ADD COLUMN IF NOT EXISTS ingress_mode TEXT NOT NULL DEFAULT 'running'",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "ALTER TABLE coordinator_governance_policies ADD COLUMN IF NOT EXISTS domain_ingress_modes JSONB NOT NULL DEFAULT '{}'::jsonb",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "ALTER TABLE coordinator_governance_policies ADD COLUMN IF NOT EXISTS paused_agent_ids JSONB NOT NULL DEFAULT '[]'::jsonb",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_coordinator_governance_policies_updated_at ON coordinator_governance_policies(updated_at DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn ensure_coordinator_governance_policy_history_table(
    pool: &PgPool,
) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS coordinator_governance_policy_history (
            id BIGSERIAL PRIMARY KEY,
            account_id TEXT NOT NULL,
            block_new_intents BOOLEAN NOT NULL DEFAULT FALSE,
            blocked_domains JSONB NOT NULL DEFAULT '[]'::jsonb,
            max_intent_notional_usd NUMERIC,
            max_total_notional_usd NUMERIC,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_by TEXT NOT NULL,
            reason TEXT
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_coord_gov_policy_hist_account_time ON coordinator_governance_policy_history(account_id, updated_at DESC, id DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn ensure_risk_runtime_state_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS risk_runtime_state (
            account_id TEXT PRIMARY KEY,
            risk_state TEXT NOT NULL DEFAULT 'Normal',
            daily_date DATE,
            daily_pnl NUMERIC(18,8) NOT NULL DEFAULT 0,
            daily_loss_limit NUMERIC(18,8) NOT NULL DEFAULT 0,
            current_equity NUMERIC(18,8) NOT NULL DEFAULT 0,
            equity_peak NUMERIC(18,8) NOT NULL DEFAULT 0,
            current_drawdown NUMERIC(18,8) NOT NULL DEFAULT 0,
            max_drawdown_observed NUMERIC(18,8) NOT NULL DEFAULT 0,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_risk_runtime_state_updated_at ON risk_runtime_state(updated_at DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}
