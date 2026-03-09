use crate::error::Result;
use sqlx::{PgPool, Row};
use tracing::warn;

pub(crate) async fn ensure_clob_quote_ticks_table(pool: &PgPool) -> Result<()> {
    crate::platform::persistence_schema::ensure_clob_quote_ticks_table(pool).await
}

pub(crate) async fn ensure_binance_price_ticks_table(pool: &PgPool) -> Result<()> {
    crate::platform::persistence_schema::ensure_binance_price_ticks_table(pool).await
}

pub(crate) async fn ensure_binance_lob_ticks_table(pool: &PgPool) -> Result<()> {
    crate::platform::persistence_schema::ensure_binance_lob_ticks_table(pool).await
}

pub(crate) async fn ensure_clob_orderbook_snapshots_table(pool: &PgPool) -> Result<()> {
    crate::platform::persistence_schema::ensure_clob_orderbook_snapshots_table(pool).await
}

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

pub(crate) async fn ensure_pm_market_metadata_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS pm_market_metadata (
            market_slug TEXT PRIMARY KEY,
            price_to_beat NUMERIC(20,8) NOT NULL,
            start_time TIMESTAMPTZ,
            end_time TIMESTAMPTZ,
            horizon TEXT,
            symbol TEXT,
            raw_market JSONB,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_market_metadata_symbol_horizon ON pm_market_metadata(symbol, horizon)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_market_metadata_end_time ON pm_market_metadata(end_time DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_market_metadata_updated_at ON pm_market_metadata(updated_at DESC)",
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

pub(crate) async fn ensure_schema_repairs(pool: &PgPool) -> Result<()> {
    let result = sqlx::query(
        r#"
        DO $$
        BEGIN
            BEGIN
                IF to_regclass('public.orders') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_orders_cycle_leg';
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_orders_cycle_leg ON orders(cycle_id, leg, created_at DESC) WHERE cycle_id IS NOT NULL';
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.positions') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_positions_status_opened';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'positions'
                          AND column_name = 'opened_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_positions_status_opened ON positions(status, opened_at DESC) WHERE status = ''OPEN''';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.positions') IS NOT NULL THEN
                    EXECUTE 'ALTER TABLE positions ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT ''default''';
                    EXECUTE 'ALTER TABLE positions DROP CONSTRAINT IF EXISTS positions_event_id_token_id_key';
                    EXECUTE 'CREATE UNIQUE INDEX IF NOT EXISTS idx_positions_account_event_token_unique ON positions(account_id, event_id, token_id)';

                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'positions'
                          AND column_name = 'opened_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_positions_account_status_opened ON positions(account_id, status, opened_at DESC) WHERE status = ''OPEN''';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.position_reconciliation_log') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_reconciliation_log_created';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'position_reconciliation_log'
                          AND column_name = 'timestamp'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_reconciliation_log_created ON position_reconciliation_log(timestamp DESC)';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.position_discrepancies') IS NOT NULL THEN
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_discrepancies_severity_unresolved ON position_discrepancies(severity, created_at DESC) WHERE resolved = FALSE';
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.nonce_usage') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_nonce_usage_active';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'nonce_usage'
                          AND column_name = 'wallet_address'
                    ) AND EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'nonce_usage'
                          AND column_name = 'released_at'
                    ) AND EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'nonce_usage'
                          AND column_name = 'allocated_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_nonce_usage_active ON nonce_usage(wallet_address, allocated_at DESC) WHERE released_at IS NULL';
                    ELSIF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'nonce_usage'
                          AND column_name = 'wallet_address'
                    ) AND EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'nonce_usage'
                          AND column_name = 'released_at'
                    ) AND EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'nonce_usage'
                          AND column_name = 'used_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_nonce_usage_active ON nonce_usage(wallet_address, used_at DESC) WHERE released_at IS NULL';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.fills') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_fills_position_time';
                    EXECUTE 'DROP INDEX IF EXISTS idx_fills_order_time';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'fills'
                          AND column_name = 'timestamp'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_fills_position_time ON fills(position_id, timestamp DESC)';
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_fills_order_time ON fills(order_id, timestamp DESC)';
                    ELSIF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'fills'
                          AND column_name = 'filled_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_fills_position_time ON fills(position_id, filled_at DESC)';
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_fills_order_time ON fills(order_id, filled_at DESC)';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.balance_snapshots') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_balance_snapshots_latest';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'balance_snapshots'
                          AND column_name = 'timestamp'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_balance_snapshots_latest ON balance_snapshots(timestamp DESC)';
                    ELSIF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'balance_snapshots'
                          AND column_name = 'created_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_balance_snapshots_latest ON balance_snapshots(created_at DESC)';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.component_heartbeats') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_heartbeats_component_time';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'component_heartbeats'
                          AND column_name = 'component_name'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_heartbeats_component_time ON component_heartbeats(component_name, last_heartbeat DESC)';
                    ELSIF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'component_heartbeats'
                          AND column_name = 'component'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_heartbeats_component_time ON component_heartbeats(component, last_heartbeat DESC)';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.system_events') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_system_events_component_time';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'system_events'
                          AND column_name = 'component'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_system_events_component_time ON system_events(component, created_at DESC)';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.order_idempotency') IS NOT NULL THEN
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT ''default''';
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS request_hash TEXT';
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS response_data JSONB';
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS error_message TEXT';
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()';
                    EXECUTE 'ALTER TABLE order_idempotency DROP CONSTRAINT IF EXISTS order_idempotency_idempotency_key_key';

                    IF EXISTS (
                        SELECT 1
                        FROM pg_constraint c
                        JOIN unnest(c.conkey) WITH ORDINALITY AS x(attnum, ordinality) ON true
                        JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = x.attnum
                        WHERE c.conrelid = 'public.order_idempotency'::regclass
                          AND c.contype = 'p'
                        GROUP BY c.oid
                        HAVING array_agg(a.attname::text ORDER BY x.ordinality) = ARRAY['idempotency_key']::text[]
                    ) THEN
                        EXECUTE 'ALTER TABLE order_idempotency DROP CONSTRAINT order_idempotency_pkey';
                        EXECUTE 'ALTER TABLE order_idempotency ADD PRIMARY KEY (account_id, idempotency_key)';
                    ELSE
                        EXECUTE 'CREATE UNIQUE INDEX IF NOT EXISTS idx_order_idempotency_account_key ON order_idempotency(account_id, idempotency_key)';
                    END IF;

                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_order_idempotency_key ON order_idempotency(idempotency_key)';
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_order_idempotency_hash ON order_idempotency(request_hash)';
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_order_idempotency_status ON order_idempotency(status, created_at)';
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_order_idempotency_expires ON order_idempotency(expires_at)';
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_order_idempotency_account_expires ON order_idempotency(account_id, expires_at)';

                    IF EXISTS (
                        SELECT 1
                        FROM pg_proc
                        WHERE proname = 'update_updated_at_column'
                          AND pg_function_is_visible(oid)
                    ) THEN
                        EXECUTE 'DROP TRIGGER IF EXISTS update_order_idempotency_updated_at ON order_idempotency';
                        EXECUTE 'CREATE TRIGGER update_order_idempotency_updated_at BEFORE UPDATE ON order_idempotency FOR EACH ROW EXECUTE FUNCTION update_updated_at_column()';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.quote_freshness') IS NOT NULL THEN
                    IF NOT EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'quote_freshness'
                          AND column_name = 'is_stale'
                    ) THEN
                        EXECUTE 'ALTER TABLE quote_freshness ADD COLUMN is_stale BOOLEAN NOT NULL DEFAULT FALSE';
                    END IF;

                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'quote_freshness'
                          AND column_name = 'is_stale'
                          AND is_generated = 'NEVER'
                    ) AND EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'quote_freshness'
                          AND column_name = 'received_at'
                    ) THEN
                        EXECUTE 'UPDATE quote_freshness SET is_stale = (EXTRACT(EPOCH FROM (NOW() - received_at)) > 30) WHERE is_stale IS DISTINCT FROM (EXTRACT(EPOCH FROM (NOW() - received_at)) > 30)';
                    END IF;

                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_quote_freshness_stale ON quote_freshness(is_stale) WHERE is_stale = false';
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;
        END $$;
        "#,
    )
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(
            error = %e,
            "schema repair DDL skipped at startup (run migration 013 as postgres for full repair)"
        );
    }

    Ok(())
}
