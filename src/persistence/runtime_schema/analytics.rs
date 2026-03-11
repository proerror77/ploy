use crate::error::Result;
use sqlx::{PgPool, Row};
use tracing::warn;

pub(crate) async fn ensure_pm_token_settlements_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS pm_token_settlements (
            token_id TEXT PRIMARY KEY,
            condition_id TEXT,
            market_id TEXT,
            market_slug TEXT,
            outcome TEXT,
            settled_price NUMERIC(10,6),
            resolved BOOLEAN NOT NULL DEFAULT FALSE,
            resolved_at TIMESTAMPTZ,
            fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            raw_market JSONB
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_token_settlements_condition ON pm_token_settlements(condition_id)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_token_settlements_market_slug ON pm_token_settlements(market_slug)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_token_settlements_resolved_at ON pm_token_settlements(resolved_at DESC) WHERE resolved_at IS NOT NULL",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_token_settlements_fetched_at ON pm_token_settlements(fetched_at DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn ensure_strategy_observability_tables(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS signal_history (
            id BIGSERIAL PRIMARY KEY,
            account_id TEXT NOT NULL DEFAULT 'default',
            recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            intent_id UUID,
            agent_id TEXT NOT NULL,
            strategy_id TEXT NOT NULL,
            domain TEXT NOT NULL,
            signal_type TEXT NOT NULL,
            market_slug TEXT,
            token_id TEXT,
            symbol TEXT,
            side TEXT,
            confidence NUMERIC(12,6),
            momentum_value NUMERIC(20,10),
            short_ma NUMERIC(20,10),
            long_ma NUMERIC(20,10),
            rolling_volatility NUMERIC(20,10),
            fair_value NUMERIC(12,6),
            market_price NUMERIC(12,6),
            edge NUMERIC(20,10),
            config_hash TEXT,
            context JSONB
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "ALTER TABLE signal_history ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default'",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_signal_history_time ON signal_history(recorded_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_signal_history_agent_time ON signal_history(agent_id, recorded_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_signal_history_strategy_time ON signal_history(strategy_id, recorded_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_signal_history_intent ON signal_history(intent_id)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_signal_history_account_time ON signal_history(account_id, recorded_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS risk_gate_decisions (
            id BIGSERIAL PRIMARY KEY,
            account_id TEXT NOT NULL DEFAULT 'default',
            decided_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            intent_id UUID NOT NULL UNIQUE,
            agent_id TEXT NOT NULL,
            domain TEXT NOT NULL,
            decision TEXT NOT NULL CHECK (decision IN ('PASSED','BLOCKED','ADJUSTED')),
            block_reason TEXT,
            suggestion_max_shares BIGINT,
            suggestion_reason TEXT,
            notional_value NUMERIC(20,10),
            config_hash TEXT,
            metadata JSONB
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "ALTER TABLE risk_gate_decisions ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default'",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_risk_gate_decisions_time ON risk_gate_decisions(decided_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_risk_gate_decisions_agent_time ON risk_gate_decisions(agent_id, decided_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_risk_gate_decisions_account_time ON risk_gate_decisions(account_id, decided_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS exit_reasons (
            id BIGSERIAL PRIMARY KEY,
            account_id TEXT NOT NULL DEFAULT 'default',
            recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            intent_id UUID NOT NULL UNIQUE,
            agent_id TEXT NOT NULL,
            domain TEXT NOT NULL,
            market_slug TEXT NOT NULL,
            token_id TEXT NOT NULL,
            market_side TEXT,
            reason_code TEXT NOT NULL,
            reason_detail TEXT,
            entry_price NUMERIC(12,6),
            exit_price NUMERIC(12,6),
            pnl_pct NUMERIC(20,10),
            status TEXT NOT NULL,
            config_hash TEXT,
            metadata JSONB
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "ALTER TABLE exit_reasons ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default'",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_exit_reasons_time ON exit_reasons(recorded_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_exit_reasons_reason_time ON exit_reasons(reason_code, recorded_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_exit_reasons_account_time ON exit_reasons(account_id, recorded_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS execution_analysis (
            id BIGSERIAL PRIMARY KEY,
            account_id TEXT NOT NULL DEFAULT 'default',
            recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            intent_id UUID NOT NULL UNIQUE,
            agent_id TEXT NOT NULL,
            domain TEXT NOT NULL,
            market_slug TEXT NOT NULL,
            token_id TEXT NOT NULL,
            is_buy BOOLEAN NOT NULL,
            expected_price NUMERIC(12,6) NOT NULL,
            executed_price NUMERIC(12,6),
            expected_slippage_bps NUMERIC(20,10),
            actual_slippage_bps NUMERIC(20,10),
            queue_delay_ms BIGINT,
            execution_latency_ms BIGINT,
            total_latency_ms BIGINT,
            status TEXT NOT NULL,
            dry_run BOOLEAN NOT NULL DEFAULT FALSE,
            config_hash TEXT,
            metadata JSONB
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "ALTER TABLE execution_analysis ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default'",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_execution_analysis_time ON execution_analysis(recorded_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_execution_analysis_agent_time ON execution_analysis(agent_id, recorded_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_execution_analysis_account_time ON execution_analysis(account_id, recorded_at DESC)",
    )
    .execute(pool)
    .await?;

    let strategy_evaluations_exists = sqlx::query(
        "SELECT to_regclass('public.strategy_evaluations') IS NOT NULL AS table_exists",
    )
    .fetch_one(pool)
    .await?
    .try_get::<bool, _>("table_exists")
    .unwrap_or(false);

    if strategy_evaluations_exists {
        sqlx::query(
            "ALTER TABLE strategy_evaluations ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default'",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_strategy_evaluations_account_time ON strategy_evaluations(account_id, evaluated_at DESC)",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_strategy_evaluations_strategy_stage_time ON strategy_evaluations(account_id, strategy_id, stage, evaluated_at DESC)",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_strategy_evaluations_status_time ON strategy_evaluations(account_id, status, evaluated_at DESC)",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_strategy_evaluations_evidence_hash ON strategy_evaluations(account_id, strategy_id, stage, evidence_hash) WHERE evidence_hash IS NOT NULL",
        )
        .execute(pool)
        .await?;
    } else {
        warn!(
            "strategy_evaluations table missing at startup; run migrations to enable deployment evidence gating"
        );
    }

    Ok(())
}
