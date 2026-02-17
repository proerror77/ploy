//! Platform Bootstrap — wires up Coordinator + Agents from config
//!
//! Entry point for `ploy platform start`. Creates shared infrastructure,
//! registers agents based on config flags, and runs the coordinator loop.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::adapters::polymarket_ws::PriceLevel;
use crate::adapters::{BinanceWebSocket, PolymarketClient, PolymarketWebSocket};
use crate::agent::PolymarketSportsClient;
use crate::agents::{
    AgentContext, CryptoLobMlAgent, CryptoLobMlConfig, CryptoTradingAgent, CryptoTradingConfig,
    PoliticsTradingAgent, PoliticsTradingConfig, SportsTradingAgent, SportsTradingConfig,
    TradingAgent,
};
use crate::config::AppConfig;
use crate::coordinator::{Coordinator, CoordinatorConfig, GlobalState};
use crate::domain::Side;
use crate::error::Result;
use crate::platform::Domain;
use crate::strategy::event_edge::core::EventEdgeCore;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::momentum::EventMatcher;
use chrono::Utc;
use futures_util::StreamExt;

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use tracing::instrument;

const CLOB_PERSIST_MIN_INTERVAL_SECS: i64 = 2;
const BINANCE_PERSIST_MIN_INTERVAL_SECS: i64 = 1;
const PM_COLLECTOR_REFRESH_SECS: u64 = 300;

async fn ensure_clob_quote_ticks_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS clob_quote_ticks (
            id BIGSERIAL PRIMARY KEY,
            token_id TEXT NOT NULL,
            side TEXT NOT NULL CHECK (side IN ('UP', 'DOWN')),
            best_bid NUMERIC(10,6),
            best_ask NUMERIC(10,6),
            bid_size NUMERIC(18,8),
            ask_size NUMERIC(18,8),
            source TEXT NOT NULL DEFAULT 'polymarket_ws',
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_quote_ticks_token_time ON clob_quote_ticks(token_id, received_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_quote_ticks_time ON clob_quote_ticks(received_at DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

async fn ensure_binance_price_ticks_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS binance_price_ticks (
            id BIGSERIAL PRIMARY KEY,
            symbol TEXT NOT NULL,
            price NUMERIC(20,10) NOT NULL,
            quantity NUMERIC(20,10),
            trade_time TIMESTAMPTZ NOT NULL,
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_binance_price_ticks_symbol_time ON binance_price_ticks(symbol, trade_time DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_binance_price_ticks_time ON binance_price_ticks(trade_time DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

async fn ensure_binance_lob_ticks_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS binance_lob_ticks (
            id BIGSERIAL PRIMARY KEY,
            symbol TEXT NOT NULL,
            update_id BIGINT,
            best_bid NUMERIC(20,10) NOT NULL,
            best_ask NUMERIC(20,10) NOT NULL,
            mid_price NUMERIC(20,10) NOT NULL,
            spread_bps NUMERIC(12,6) NOT NULL,
            obi_5 NUMERIC(12,8) NOT NULL,
            obi_10 NUMERIC(12,8) NOT NULL,
            bid_volume_5 NUMERIC(20,10) NOT NULL,
            ask_volume_5 NUMERIC(20,10) NOT NULL,
            bids JSONB,
            asks JSONB,
            event_time TIMESTAMPTZ NOT NULL,
            source TEXT NOT NULL DEFAULT 'binance_depth_ws',
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_binance_lob_ticks_symbol_time ON binance_lob_ticks(symbol, event_time DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_binance_lob_ticks_time ON binance_lob_ticks(event_time DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn ensure_clob_orderbook_snapshots_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS clob_orderbook_snapshots (
            id BIGSERIAL PRIMARY KEY,
            domain TEXT,
            token_id TEXT NOT NULL,
            market TEXT,
            bids JSONB NOT NULL,
            asks JSONB NOT NULL,
            book_timestamp TIMESTAMPTZ,
            hash TEXT,
            source TEXT NOT NULL DEFAULT 'polymarket_ws',
            context JSONB,
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_orderbook_snapshots_token_time ON clob_orderbook_snapshots(token_id, received_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_orderbook_snapshots_time ON clob_orderbook_snapshots(received_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_orderbook_snapshots_domain_time ON clob_orderbook_snapshots(domain, received_at DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

async fn ensure_accounts_table(pool: &PgPool) -> Result<()> {
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

    // updated_at trigger (best-effort; older DBs may lack update_updated_at_column())
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

async fn upsert_account_from_config(
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
    // Persist strategy signal calculations for audit/backtest attribution.
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

    sqlx::query("ALTER TABLE signal_history ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default'")
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

    // Persist every risk-gate decision (pass/adjust/block) with context.
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

    sqlx::query("ALTER TABLE risk_gate_decisions ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default'")
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

    // Persist position-exit reason attribution (take-profit / stop-loss / etc.).
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

    sqlx::query("ALTER TABLE exit_reasons ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default'")
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

    // Persist execution quality stats (slippage + latency breakdown).
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

    sqlx::query("ALTER TABLE execution_analysis ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default'")
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

    Ok(())
}

async fn ensure_schema_repairs(pool: &PgPool) -> Result<()> {
    // These repairs are intentionally startup-applied because `platform start`
    // does not run the sqlx migration runner.
    let result = sqlx::query(
        r#"
        DO $$
        BEGIN
            BEGIN
                -- orders(cycle_id, leg, created_at)
                IF to_regclass('public.orders') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_orders_cycle_leg';
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_orders_cycle_leg ON orders(cycle_id, leg, created_at DESC) WHERE cycle_id IS NOT NULL';
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                -- positions(status='OPEN', opened_at)
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
                -- position_reconciliation_log(timestamp)
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
                -- unresolved discrepancy severity index
                IF to_regclass('public.position_discrepancies') IS NOT NULL THEN
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_discrepancies_severity_unresolved ON position_discrepancies(severity, created_at DESC) WHERE resolved = FALSE';
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                -- nonce_usage active index (prefer allocated_at, fallback used_at)
                IF to_regclass('public.nonce_usage') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_nonce_usage_active';
                    IF EXISTS (
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
                          AND column_name = 'used_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_nonce_usage_active ON nonce_usage(wallet_address, used_at DESC) WHERE released_at IS NULL';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                -- fills(timestamp) indexes (fallback to filled_at for legacy schemas)
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
                -- balance snapshots latest (timestamp preferred, fallback created_at)
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
                -- component heartbeats by component_name/component
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
                -- system events by component
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
                -- Reconcile order_idempotency schema drift + multi-account scoping.
                IF to_regclass('public.order_idempotency') IS NOT NULL THEN
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT ''default''';
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS request_hash TEXT';
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS response_data JSONB';
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS error_message TEXT';
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()';

                    -- Drop global uniqueness constraints so idempotency keys can repeat across accounts.
                    EXECUTE 'ALTER TABLE order_idempotency DROP CONSTRAINT IF EXISTS order_idempotency_idempotency_key_key';

                    -- If the primary key is idempotency_key, replace it with a composite PK.
                    IF EXISTS (
                        SELECT 1
                        FROM pg_constraint c
                        JOIN unnest(c.conkey) WITH ORDINALITY AS x(attnum, ordinality) ON true
                        JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = x.attnum
                        WHERE c.conrelid = 'public.order_idempotency'::regclass
                          AND c.contype = 'p'
                        GROUP BY c.oid
                        HAVING array_agg(a.attname ORDER BY x.ordinality) = ARRAY['idempotency_key']
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
        // Older installs may have tables owned by postgres while services run as `ploy`.
        // In that case, startup DDL can't be applied by the app user.
        warn!(
            error = %e,
            "schema repair DDL skipped at startup (run migration 013 as postgres for full repair)"
        );
    }

    Ok(())
}

async fn ensure_clob_trade_ticks_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS clob_trade_ticks (
            id BIGSERIAL PRIMARY KEY,
            domain TEXT,
            condition_id TEXT NOT NULL,
            token_id TEXT NOT NULL,
            side TEXT NOT NULL CHECK (side IN ('BUY','SELL')),
            size NUMERIC(20,10) NOT NULL,
            price NUMERIC(10,6) NOT NULL,
            trade_ts TIMESTAMPTZ NOT NULL,
            trade_ts_unix BIGINT NOT NULL,
            transaction_hash TEXT NOT NULL,
            proxy_wallet TEXT,
            title TEXT,
            slug TEXT,
            outcome TEXT,
            outcome_index INTEGER,
            source TEXT NOT NULL DEFAULT 'polymarket_data_api',
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            UNIQUE (transaction_hash, token_id, side, size, price, trade_ts_unix)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_trade_ticks_token_time ON clob_trade_ticks(token_id, trade_ts DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_trade_ticks_market_time ON clob_trade_ticks(condition_id, trade_ts DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_trade_ticks_time ON clob_trade_ticks(trade_ts DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

async fn ensure_clob_trade_alerts_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS clob_trade_alerts (
            id BIGSERIAL PRIMARY KEY,
            alert_type TEXT NOT NULL CHECK (alert_type IN ('LARGE_TRADE','BURST')),
            domain TEXT,
            condition_id TEXT NOT NULL,
            token_id TEXT NOT NULL,
            side TEXT CHECK (side IN ('BUY','SELL')),
            size NUMERIC(20,10),
            notional NUMERIC(20,10),
            trade_ts TIMESTAMPTZ,
            trade_ts_unix BIGINT,
            transaction_hash TEXT,
            window_start TIMESTAMPTZ,
            window_end TIMESTAMPTZ,
            burst_bucket_unix BIGINT,
            metadata JSONB,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_trade_alerts_time ON clob_trade_alerts(created_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_trade_alerts_token_time ON clob_trade_alerts(token_id, created_at DESC)",
    )
    .execute(pool)
    .await?;

    // One alert per trade tick (idempotent when we overlap pages).
    sqlx::query(
        r#"
        CREATE UNIQUE INDEX IF NOT EXISTS idx_clob_trade_alerts_large_unique
        ON clob_trade_alerts(alert_type, transaction_hash, token_id)
        WHERE alert_type = 'LARGE_TRADE'
        "#,
    )
    .execute(pool)
    .await?;

    // Cooldown-bucketed burst alerts (idempotent within the same bucket).
    sqlx::query(
        r#"
        CREATE UNIQUE INDEX IF NOT EXISTS idx_clob_trade_alerts_burst_unique
        ON clob_trade_alerts(alert_type, token_id, burst_bucket_unix)
        WHERE alert_type = 'BURST'
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}

fn spawn_clob_quote_persistence(pm_ws: Arc<PolymarketWebSocket>, pool: PgPool, agent_id: String) {
    tokio::spawn(async move {
        if let Err(e) = ensure_clob_quote_ticks_table(&pool).await {
            warn!(
                agent = agent_id,
                error = %e,
                "failed to ensure clob_quote_ticks table; quote persistence disabled"
            );
            return;
        }

        let mut rx = pm_ws.subscribe_updates();
        let mut last_persisted: HashMap<
            String,
            (
                chrono::DateTime<Utc>,
                Option<rust_decimal::Decimal>,
                Option<rust_decimal::Decimal>,
                Option<rust_decimal::Decimal>,
                Option<rust_decimal::Decimal>,
            ),
        > = HashMap::new();
        let mut persisted_count: u64 = 0;

        loop {
            match rx.recv().await {
                Ok(update) => {
                    if update.quote.best_bid.is_none() && update.quote.best_ask.is_none() {
                        continue;
                    }

                    let now = Utc::now();
                    let should_persist = match last_persisted.get(&update.token_id) {
                        None => true,
                        Some((ts, prev_bid, prev_ask, prev_bid_size, prev_ask_size)) => {
                            let changed = *prev_bid != update.quote.best_bid
                                || *prev_ask != update.quote.best_ask
                                || *prev_bid_size != update.quote.bid_size
                                || *prev_ask_size != update.quote.ask_size;
                            let elapsed = now.signed_duration_since(*ts).num_seconds()
                                >= CLOB_PERSIST_MIN_INTERVAL_SECS;
                            changed && elapsed
                        }
                    };

                    if !should_persist {
                        continue;
                    }

                    let side = update.side.as_str();
                    if let Err(e) = sqlx::query(
                        r#"
                        INSERT INTO clob_quote_ticks
                            (token_id, side, best_bid, best_ask, bid_size, ask_size, source)
                        VALUES
                            ($1, $2, $3, $4, $5, $6, 'polymarket_ws')
                        "#,
                    )
                    .bind(&update.token_id)
                    .bind(side)
                    .bind(update.quote.best_bid)
                    .bind(update.quote.best_ask)
                    .bind(update.quote.bid_size)
                    .bind(update.quote.ask_size)
                    .execute(&pool)
                    .await
                    {
                        warn!(
                            agent = agent_id,
                            token_id = %update.token_id,
                            error = %e,
                            "failed to persist clob quote"
                        );
                        continue;
                    }

                    last_persisted.insert(
                        update.token_id.clone(),
                        (
                            now,
                            update.quote.best_bid,
                            update.quote.best_ask,
                            update.quote.bid_size,
                            update.quote.ask_size,
                        ),
                    );
                    persisted_count = persisted_count.saturating_add(1);

                    if persisted_count % 1000 == 0 {
                        info!(
                            agent = agent_id,
                            persisted_count, "persisted clob quote ticks"
                        );
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        agent = agent_id,
                        lagged = n,
                        "clob persistence receiver lagged"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    warn!(agent = agent_id, "clob persistence receiver closed");
                    break;
                }
            }
        }
    });
}

fn spawn_binance_price_persistence(
    binance_ws: Arc<BinanceWebSocket>,
    pool: PgPool,
    agent_id: String,
) {
    tokio::spawn(async move {
        if let Err(e) = ensure_binance_price_ticks_table(&pool).await {
            warn!(
                agent = agent_id,
                error = %e,
                "failed to ensure binance_price_ticks table; Binance persistence disabled"
            );
            return;
        }

        let mut rx = binance_ws.subscribe();
        let mut last_persisted: HashMap<
            String,
            (
                chrono::DateTime<Utc>,
                Option<rust_decimal::Decimal>,
                Option<rust_decimal::Decimal>,
            ),
        > = HashMap::new();
        let mut persisted_count: u64 = 0;

        loop {
            match rx.recv().await {
                Ok(update) => {
                    let now = Utc::now();
                    let should_persist = match last_persisted.get(&update.symbol) {
                        None => true,
                        Some((ts, prev_price, prev_qty)) => {
                            let changed =
                                *prev_price != Some(update.price) || *prev_qty != update.quantity;
                            let elapsed = now.signed_duration_since(*ts).num_seconds()
                                >= BINANCE_PERSIST_MIN_INTERVAL_SECS;
                            changed && elapsed
                        }
                    };

                    if !should_persist {
                        continue;
                    }

                    if let Err(e) = sqlx::query(
                        r#"
                        INSERT INTO binance_price_ticks
                            (symbol, price, quantity, trade_time)
                        VALUES
                            ($1, $2, $3, $4)
                        "#,
                    )
                    .bind(&update.symbol)
                    .bind(update.price)
                    .bind(update.quantity)
                    .bind(update.timestamp)
                    .execute(&pool)
                    .await
                    {
                        warn!(
                            agent = agent_id,
                            symbol = %update.symbol,
                            error = %e,
                            "failed to persist Binance price tick"
                        );
                        continue;
                    }

                    last_persisted.insert(
                        update.symbol.clone(),
                        (now, Some(update.price), update.quantity),
                    );
                    persisted_count = persisted_count.saturating_add(1);

                    if persisted_count % 10_000 == 0 {
                        info!(
                            agent = agent_id,
                            persisted_count, "persisted Binance price ticks"
                        );
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        agent = agent_id,
                        lagged = n,
                        "binance persistence receiver lagged"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    warn!(agent = agent_id, "binance persistence receiver closed");
                    break;
                }
            }
        }
    });
}

fn lob_levels_json(
    state: &crate::collector::OrderBookState,
    is_bids: bool,
    max_levels: usize,
) -> Vec<(String, String)> {
    let max_levels = max_levels.max(1);

    if is_bids {
        state
            .bids
            .iter()
            .rev()
            .take(max_levels)
            .map(|(price_cents, qty)| {
                let price =
                    rust_decimal::Decimal::from(*price_cents) / rust_decimal::Decimal::from(100);
                (price.to_string(), qty.to_string())
            })
            .collect()
    } else {
        state
            .asks
            .iter()
            .take(max_levels)
            .map(|(price_cents, qty)| {
                let price =
                    rust_decimal::Decimal::from(*price_cents) / rust_decimal::Decimal::from(100);
                (price.to_string(), qty.to_string())
            })
            .collect()
    }
}

fn spawn_binance_lob_persistence(
    depth_stream: Arc<crate::collector::BinanceDepthStream>,
    pool: PgPool,
    agent_id: String,
) {
    tokio::spawn(async move {
        if let Err(e) = ensure_binance_lob_ticks_table(&pool).await {
            warn!(
                agent = agent_id,
                error = %e,
                "failed to ensure binance_lob_ticks table; Binance LOB persistence disabled"
            );
            return;
        }

        let snapshot_interval_ms = env_u64("BN_LOB_SNAPSHOT_MS", 1000).max(100);
        let max_levels = env_usize("BN_LOB_LEVELS", 20).clamp(0, 200);

        let mut rx = depth_stream.subscribe();
        let mut last_persisted: HashMap<String, chrono::DateTime<Utc>> = HashMap::new();
        let mut last_update_id: HashMap<String, i64> = HashMap::new();
        let mut persisted_count: u64 = 0;

        loop {
            match rx.recv().await {
                Ok(update) => {
                    let now = Utc::now();
                    let symbol = update.symbol.clone();

                    let should_persist =
                        match (last_persisted.get(&symbol), last_update_id.get(&symbol)) {
                            (None, _) => true,
                            (Some(ts), Some(prev_id)) => {
                                let elapsed_ms =
                                    now.signed_duration_since(*ts).num_milliseconds().max(0) as u64;
                                elapsed_ms >= snapshot_interval_ms
                                    && *prev_id != update.snapshot.update_id
                            }
                            (Some(ts), None) => {
                                let elapsed_ms =
                                    now.signed_duration_since(*ts).num_milliseconds().max(0) as u64;
                                elapsed_ms >= snapshot_interval_ms
                            }
                        };

                    if !should_persist {
                        continue;
                    }

                    let (bids, asks) = if max_levels == 0 {
                        (Vec::new(), Vec::new())
                    } else {
                        (
                            lob_levels_json(&update.raw_state, true, max_levels),
                            lob_levels_json(&update.raw_state, false, max_levels),
                        )
                    };

                    if let Err(e) = sqlx::query(
                        r#"
                        INSERT INTO binance_lob_ticks (
                            symbol, update_id,
                            best_bid, best_ask, mid_price, spread_bps,
                            obi_5, obi_10,
                            bid_volume_5, ask_volume_5,
                            bids, asks,
                            event_time
                        ) VALUES (
                            $1, $2,
                            $3, $4, $5, $6,
                            $7, $8,
                            $9, $10,
                            $11, $12,
                            $13
                        )
                        "#,
                    )
                    .bind(&symbol)
                    .bind(update.snapshot.update_id)
                    .bind(update.snapshot.best_bid)
                    .bind(update.snapshot.best_ask)
                    .bind(update.snapshot.mid_price)
                    .bind(update.snapshot.spread_bps)
                    .bind(update.snapshot.obi_5)
                    .bind(update.snapshot.obi_10)
                    .bind(update.snapshot.bid_volume_5)
                    .bind(update.snapshot.ask_volume_5)
                    .bind(sqlx::types::Json(&bids))
                    .bind(sqlx::types::Json(&asks))
                    .bind(update.snapshot.timestamp)
                    .execute(&pool)
                    .await
                    {
                        warn!(
                            agent = agent_id,
                            symbol = %symbol,
                            error = %e,
                            "failed to persist Binance LOB tick"
                        );
                        continue;
                    }

                    last_persisted.insert(symbol.clone(), now);
                    last_update_id.insert(symbol.clone(), update.snapshot.update_id);
                    persisted_count = persisted_count.saturating_add(1);

                    if persisted_count % 10_000 == 0 {
                        info!(
                            agent = agent_id,
                            persisted_count,
                            snapshot_interval_ms,
                            max_levels,
                            "persisted Binance LOB ticks"
                        );
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        agent = agent_id,
                        lagged = n,
                        "binance lob persistence receiver lagged"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    warn!(agent = agent_id, "binance lob persistence receiver closed");
                    break;
                }
            }
        }
    });
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DataApiTrade {
    proxy_wallet: Option<String>,
    side: String,
    asset: String,
    #[serde(rename = "conditionId")]
    condition_id: String,
    #[serde(deserialize_with = "deserialize_decimal")]
    size: rust_decimal::Decimal,
    #[serde(deserialize_with = "deserialize_decimal")]
    price: rust_decimal::Decimal,
    timestamp: i64,
    transaction_hash: String,
    title: Option<String>,
    slug: Option<String>,
    outcome: Option<String>,
    outcome_index: Option<i32>,
}

fn deserialize_decimal<'de, D>(
    deserializer: D,
) -> std::result::Result<rust_decimal::Decimal, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let value: serde_json::Value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::String(s) => s
            .parse::<rust_decimal::Decimal>()
            .map_err(serde::de::Error::custom),
        serde_json::Value::Number(n) => n
            .to_string()
            .parse::<rust_decimal::Decimal>()
            .map_err(serde::de::Error::custom),
        other => Err(serde::de::Error::custom(format!(
            "invalid decimal value: {other:?}"
        ))),
    }
}

type InsertedTradeTickRow = (
    String,                // token_id
    String,                // side
    rust_decimal::Decimal, // size
    rust_decimal::Decimal, // price
    chrono::DateTime<Utc>, // trade_ts
    i64,                   // trade_ts_unix
    String,                // transaction_hash
);

#[derive(Debug, Clone)]
struct TradeAlertConfig {
    min_size: rust_decimal::Decimal,
    min_notional: rust_decimal::Decimal,
    burst_window_secs: i64,
    burst_min_size: rust_decimal::Decimal,
    burst_min_notional: rust_decimal::Decimal,
    burst_min_trades: usize,
    burst_cooldown_secs: i64,
}

impl TradeAlertConfig {
    fn from_env() -> Self {
        let min_size = env_decimal("PM_TRADE_ALERT_MIN_SIZE", rust_decimal::Decimal::ZERO);
        let min_notional = env_decimal("PM_TRADE_ALERT_MIN_NOTIONAL", rust_decimal::Decimal::ZERO);
        let burst_window_secs = env_i64("PM_TRADE_BURST_WINDOW_SECS", 60).max(1);
        let burst_min_size = env_decimal("PM_TRADE_BURST_MIN_SIZE", rust_decimal::Decimal::ZERO);
        let burst_min_notional =
            env_decimal("PM_TRADE_BURST_MIN_NOTIONAL", rust_decimal::Decimal::ZERO);
        let burst_min_trades = env_usize("PM_TRADE_BURST_MIN_TRADES", 0);
        let burst_cooldown_secs = env_i64("PM_TRADE_BURST_COOLDOWN_SECS", burst_window_secs).max(1);

        Self {
            min_size,
            min_notional,
            burst_window_secs,
            burst_min_size,
            burst_min_notional,
            burst_min_trades,
            burst_cooldown_secs,
        }
    }

    fn disabled() -> Self {
        Self {
            min_size: rust_decimal::Decimal::ZERO,
            min_notional: rust_decimal::Decimal::ZERO,
            burst_window_secs: 60,
            burst_min_size: rust_decimal::Decimal::ZERO,
            burst_min_notional: rust_decimal::Decimal::ZERO,
            burst_min_trades: 0,
            burst_cooldown_secs: 60,
        }
    }

    fn enabled(&self) -> bool {
        self.min_size > rust_decimal::Decimal::ZERO
            || self.min_notional > rust_decimal::Decimal::ZERO
            || self.burst_enabled()
    }

    fn burst_enabled(&self) -> bool {
        self.burst_min_size > rust_decimal::Decimal::ZERO
            || self.burst_min_notional > rust_decimal::Decimal::ZERO
    }
}

#[derive(Debug, Default)]
struct TradeAlertState {
    by_token: HashMap<String, TokenBurstState>,
}

#[derive(Debug, Default)]
struct TokenBurstState {
    trades: VecDeque<(i64, rust_decimal::Decimal, rust_decimal::Decimal)>,
    sum_size: rust_decimal::Decimal,
    sum_notional: rust_decimal::Decimal,
    last_burst_bucket_unix: Option<i64>,
}

#[derive(Debug, Clone)]
struct TradeBurstAlert {
    token_id: String,
    condition_id: String,
    window_start_unix: i64,
    window_end_unix: i64,
    burst_bucket_unix: i64,
    sum_size: rust_decimal::Decimal,
    sum_notional: rust_decimal::Decimal,
    n_trades: usize,
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

#[instrument(skip(http, pool, last_seen_by_market))]
async fn collect_trades_for_market(
    http: &reqwest::Client,
    pool: &PgPool,
    condition_id: &str,
    domain: &str,
    page_limit: usize,
    max_pages: usize,
    overlap_secs: i64,
    last_seen_by_market: &tokio::sync::RwLock<HashMap<String, i64>>,
    alert_cfg: TradeAlertConfig,
    alert_state: Option<Arc<tokio::sync::Mutex<TradeAlertState>>>,
) {
    use chrono::TimeZone as _;

    let last_seen_ts = {
        let map = last_seen_by_market.read().await;
        map.get(condition_id).copied()
    };

    // Seed per-market high-water mark from the DB so restarts don't trigger expensive
    // backfills (max_pages * markets) and to keep near-real-time trade capture.
    let last_seen_ts: i64 = match last_seen_ts {
        Some(ts) => ts,
        None => {
            let seeded = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(trade_ts_unix), 0) FROM clob_trade_ticks WHERE condition_id = $1",
            )
            .bind(condition_id)
            .fetch_one(pool)
            .await
            .unwrap_or(0);

            // If we have no history for this market, start "now" (best-effort, real-time focus).
            let seeded = if seeded <= 0 {
                Utc::now().timestamp()
            } else {
                seeded
            };

            let mut map = last_seen_by_market.write().await;
            *map.entry(condition_id.to_string()).or_insert(seeded)
        }
    };
    let target_min_ts = last_seen_ts.saturating_sub(overlap_secs.max(0));

    let mut max_ts_seen: i64 = last_seen_ts;

    for page in 0..max_pages {
        let offset = page.saturating_mul(page_limit);

        let resp = http
            .get("https://data-api.polymarket.com/trades")
            .query(&[
                ("market", condition_id),
                ("limit", &page_limit.to_string()),
                ("offset", &offset.to_string()),
            ])
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    condition_id,
                    error = %e,
                    "failed to fetch polymarket data-api trades"
                );
                return;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            warn!(
                condition_id,
                status = %status,
                body = %text,
                "polymarket data-api trades request failed"
            );
            return;
        }

        let trades: Vec<DataApiTrade> = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    condition_id,
                    error = %e,
                    "failed to parse polymarket data-api trades response"
                );
                return;
            }
        };

        if trades.is_empty() {
            break;
        }

        let mut min_ts_in_page: i64 = i64::MAX;
        let mut max_ts_in_page: i64 = i64::MIN;

        // Prepare rows for insertion (filter to a time window to avoid spamming duplicates).
        let mut rows: Vec<&DataApiTrade> = Vec::with_capacity(trades.len());
        for t in &trades {
            min_ts_in_page = min_ts_in_page.min(t.timestamp);
            max_ts_in_page = max_ts_in_page.max(t.timestamp);

            if t.timestamp >= target_min_ts {
                rows.push(t);
            }
        }

        max_ts_seen = max_ts_seen.max(max_ts_in_page);

        if !rows.is_empty() {
            let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
                r#"
                INSERT INTO clob_trade_ticks (
                    domain,
                    condition_id,
                    token_id,
                    side,
                    size,
                    price,
                    trade_ts,
                    trade_ts_unix,
                    transaction_hash,
                    proxy_wallet,
                    title,
                    slug,
                    outcome,
                    outcome_index,
                    source
                )
                "#,
            );

            qb.push_values(rows.into_iter(), |mut b, t| {
                let trade_ts = Utc.timestamp_opt(t.timestamp, 0).single();

                b.push_bind(domain)
                    .push_bind(&t.condition_id)
                    .push_bind(&t.asset)
                    .push_bind(&t.side)
                    .push_bind(t.size)
                    .push_bind(t.price)
                    .push_bind(trade_ts.unwrap_or_else(Utc::now))
                    .push_bind(t.timestamp)
                    .push_bind(&t.transaction_hash)
                    .push_bind(&t.proxy_wallet)
                    .push_bind(&t.title)
                    .push_bind(&t.slug)
                    .push_bind(&t.outcome)
                    .push_bind(t.outcome_index)
                    .push_bind("polymarket_data_api");
            });

            if alert_cfg.enabled() {
                qb.push(
                    " ON CONFLICT DO NOTHING RETURNING token_id, side, size, price, trade_ts, trade_ts_unix, transaction_hash",
                );

                match qb
                    .build_query_as::<InsertedTradeTickRow>()
                    .fetch_all(pool)
                    .await
                {
                    Ok(mut inserted) => {
                        if !inserted.is_empty() {
                            inserted.sort_by_key(|r| r.5);
                            maybe_emit_trade_alerts(
                                pool,
                                domain,
                                condition_id,
                                &inserted,
                                &alert_cfg,
                                alert_state.as_ref(),
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        warn!(
                            condition_id,
                            error = %e,
                            "failed to persist polymarket trade ticks (returning)"
                        );
                    }
                }
            } else {
                qb.push(" ON CONFLICT DO NOTHING");

                if let Err(e) = qb.build().execute(pool).await {
                    warn!(
                        condition_id,
                        error = %e,
                        "failed to persist polymarket trade ticks"
                    );
                }
            }
        }

        // We paged far enough back to cover our overlap window.
        if min_ts_in_page < target_min_ts {
            break;
        }

        // Last page (fewer than requested).
        if trades.len() < page_limit {
            break;
        }
    }

    // Update high-water mark.
    if max_ts_seen > last_seen_ts {
        let mut map = last_seen_by_market.write().await;
        map.insert(condition_id.to_string(), max_ts_seen);
    }
}

#[instrument(skip(pool, inserted, alert_state))]
async fn maybe_emit_trade_alerts(
    pool: &PgPool,
    domain: &str,
    condition_id: &str,
    inserted: &[InsertedTradeTickRow],
    alert_cfg: &TradeAlertConfig,
    alert_state: Option<&Arc<tokio::sync::Mutex<TradeAlertState>>>,
) {
    use rust_decimal::Decimal;

    if inserted.is_empty() || !alert_cfg.enabled() {
        return;
    }

    // Per-trade alerts.
    for (token_id, side, size, price, trade_ts, trade_ts_unix, tx_hash) in inserted {
        let notional: Decimal = *size * *price;
        let size_trigger = alert_cfg.min_size > Decimal::ZERO && *size >= alert_cfg.min_size;
        let notional_trigger =
            alert_cfg.min_notional > Decimal::ZERO && notional >= alert_cfg.min_notional;

        if !(size_trigger || notional_trigger) {
            continue;
        }

        warn!(
            condition_id,
            token_id,
            side,
            size = %size,
            price = %price,
            notional = %notional,
            trade_ts = %trade_ts,
            trade_ts_unix,
            transaction_hash = %tx_hash,
            "large trade tick detected"
        );

        let meta = json!({
            "min_size": alert_cfg.min_size.to_string(),
            "min_notional": alert_cfg.min_notional.to_string(),
        });

        if let Err(e) = sqlx::query(
            r#"
            INSERT INTO clob_trade_alerts (
                alert_type,
                domain,
                condition_id,
                token_id,
                side,
                size,
                notional,
                trade_ts,
                trade_ts_unix,
                transaction_hash,
                metadata
            )
            VALUES (
                'LARGE_TRADE',
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10
            )
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(domain)
        .bind(condition_id)
        .bind(token_id)
        .bind(side)
        .bind(*size)
        .bind(notional)
        .bind(*trade_ts)
        .bind(*trade_ts_unix)
        .bind(tx_hash)
        .bind(sqlx::types::Json(meta))
        .execute(pool)
        .await
        {
            warn!(
                condition_id,
                token_id,
                error = %e,
                "failed to persist large trade alert"
            );
        }
    }

    // Sliding-window burst alerts.
    if !alert_cfg.burst_enabled() {
        return;
    }
    let Some(state) = alert_state else {
        return;
    };

    let mut burst_events: Vec<TradeBurstAlert> = Vec::new();
    {
        let mut guard = state.lock().await;

        for (token_id, _side, size, price, _trade_ts, trade_ts_unix, _tx_hash) in inserted {
            let notional: Decimal = *size * *price;

            let token_state = guard.by_token.entry(token_id.clone()).or_default();
            token_state
                .trades
                .push_back((*trade_ts_unix, *size, notional));
            token_state.sum_size += *size;
            token_state.sum_notional += notional;

            let cutoff = trade_ts_unix.saturating_sub(alert_cfg.burst_window_secs.max(1));
            while let Some((front_ts, front_size, front_notional)) =
                token_state.trades.front().cloned()
            {
                if front_ts < cutoff {
                    token_state.trades.pop_front();
                    token_state.sum_size -= front_size;
                    token_state.sum_notional -= front_notional;
                } else {
                    break;
                }
            }

            let n = token_state.trades.len();
            let enough_trades = alert_cfg.burst_min_trades == 0 || n >= alert_cfg.burst_min_trades;
            if !enough_trades {
                continue;
            }

            let size_trigger = alert_cfg.burst_min_size > Decimal::ZERO
                && token_state.sum_size >= alert_cfg.burst_min_size;
            let notional_trigger = alert_cfg.burst_min_notional > Decimal::ZERO
                && token_state.sum_notional >= alert_cfg.burst_min_notional;

            if !(size_trigger || notional_trigger) {
                continue;
            }

            let bucket_unix =
                (*trade_ts_unix / alert_cfg.burst_cooldown_secs) * alert_cfg.burst_cooldown_secs;
            if token_state.last_burst_bucket_unix == Some(bucket_unix) {
                continue;
            }
            token_state.last_burst_bucket_unix = Some(bucket_unix);

            let window_start_unix = token_state
                .trades
                .front()
                .map(|(ts, _, _)| *ts)
                .unwrap_or(*trade_ts_unix);

            burst_events.push(TradeBurstAlert {
                token_id: token_id.clone(),
                condition_id: condition_id.to_string(),
                window_start_unix,
                window_end_unix: *trade_ts_unix,
                burst_bucket_unix: bucket_unix,
                sum_size: token_state.sum_size,
                sum_notional: token_state.sum_notional,
                n_trades: n,
            });
        }
    }

    if burst_events.is_empty() {
        return;
    }

    use chrono::TimeZone as _;
    for ev in burst_events {
        let window_start_ts = Utc.timestamp_opt(ev.window_start_unix, 0).single();
        let window_end_ts = Utc.timestamp_opt(ev.window_end_unix, 0).single();

        warn!(
            condition_id = %ev.condition_id,
            token_id = %ev.token_id,
            n_trades = ev.n_trades,
            sum_size = %ev.sum_size,
            sum_notional = %ev.sum_notional,
            window_start_unix = ev.window_start_unix,
            window_end_unix = ev.window_end_unix,
            burst_bucket_unix = ev.burst_bucket_unix,
            "trade burst detected"
        );

        let meta = json!({
            "window_secs": alert_cfg.burst_window_secs,
            "min_size": alert_cfg.burst_min_size.to_string(),
            "min_notional": alert_cfg.burst_min_notional.to_string(),
            "min_trades": alert_cfg.burst_min_trades,
        });

        if let Err(e) = sqlx::query(
            r#"
            INSERT INTO clob_trade_alerts (
                alert_type,
                domain,
                condition_id,
                token_id,
                size,
                notional,
                trade_ts,
                trade_ts_unix,
                window_start,
                window_end,
                burst_bucket_unix,
                metadata
            )
            VALUES (
                'BURST',
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11
            )
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(domain)
        .bind(&ev.condition_id)
        .bind(&ev.token_id)
        .bind(ev.sum_size)
        .bind(ev.sum_notional)
        .bind(window_end_ts.unwrap_or_else(Utc::now))
        .bind(ev.window_end_unix)
        .bind(window_start_ts)
        .bind(window_end_ts)
        .bind(ev.burst_bucket_unix)
        .bind(sqlx::types::Json(meta))
        .execute(pool)
        .await
        {
            warn!(
                condition_id = %ev.condition_id,
                token_id = %ev.token_id,
                error = %e,
                "failed to persist trade burst alert"
            );
        }
    }
}

fn spawn_polymarket_trade_persistence(
    event_matcher: Arc<EventMatcher>,
    pool: PgPool,
    agent_id: String,
    coins: Vec<String>,
    domain: Domain,
) {
    tokio::spawn(async move {
        let agent_label = agent_id.clone();

        if let Err(e) = ensure_clob_trade_ticks_table(&pool).await {
            warn!(
                agent = agent_label,
                error = %e,
                "failed to ensure clob_trade_ticks table; trade persistence disabled"
            );
            return;
        }

        let http = match reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("Mozilla/5.0 (ploy)")
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    agent = agent_label,
                    error = %e,
                    "failed to build reqwest client for polymarket data-api; trade persistence disabled"
                );
                return;
            }
        };

        let poll_secs = env_u64("PM_TRADES_POLL_SECS", 10).max(1);
        let page_limit = env_usize("PM_TRADES_PAGE_LIMIT", 200).clamp(1, 1000);
        let max_pages = env_usize("PM_TRADES_MAX_PAGES", 10).clamp(1, 100);
        let overlap_secs = env_i64("PM_TRADES_OVERLAP_SECS", 120).max(0);
        let max_concurrency = env_usize("PM_TRADES_CONCURRENCY", 4).clamp(1, 32);

        let mut alert_cfg = TradeAlertConfig::from_env();
        let mut alert_state: Option<Arc<tokio::sync::Mutex<TradeAlertState>>> =
            if alert_cfg.burst_enabled() {
                Some(Arc::new(
                    tokio::sync::Mutex::new(TradeAlertState::default()),
                ))
            } else {
                None
            };

        if alert_cfg.enabled() {
            if let Err(e) = ensure_clob_trade_alerts_table(&pool).await {
                warn!(
                    agent = agent_label,
                    error = %e,
                    "failed to ensure clob_trade_alerts table; trade alerting disabled"
                );
                alert_cfg = TradeAlertConfig::disabled();
                alert_state = None;
            }
        }

        // High-water mark per market to keep polling bounded. We overlap by N seconds and rely
        // on ON CONFLICT DO NOTHING to dedupe safely.
        let last_seen_by_market: Arc<tokio::sync::RwLock<HashMap<String, i64>>> =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        // Data collection should keep capturing trades through the end of the market and
        // for a short grace period afterwards (late blocks, indexer delays, etc.).
        let end_grace_secs = env_i64("PM_TRADES_END_GRACE_SECS", 600).max(0);
        let min_remaining_for_collection = env_i64("PM_TRADES_MIN_REMAINING_SECS", 0)
            .max(-86400)
            .min(86400);
        let mut tracked_markets: HashMap<String, i64> = HashMap::new(); // condition_id -> expires_at_unix

        let mut tick = tokio::time::interval(Duration::from_secs(poll_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tick.tick().await;

            // Refresh the tracked market set from cached Gamma snapshots (EventMatcher).
            // Keep markets until `end_time + grace`, even after they fall out of the Gamma window.
            let now_unix = Utc::now().timestamp();
            for coin in &coins {
                let symbol = format!("{}USDT", coin.to_uppercase());
                for ev in event_matcher
                    .get_events_with_min_remaining(&symbol, min_remaining_for_collection)
                    .await
                {
                    let cid = ev.condition_id.trim();
                    if cid.is_empty() {
                        continue;
                    }
                    let expires_at = ev.end_time.timestamp().saturating_add(end_grace_secs);
                    tracked_markets.insert(cid.to_string(), expires_at);
                }
            }

            tracked_markets.retain(|_, expires_at| *expires_at >= now_unix);
            let mut markets: Vec<String> = tracked_markets.keys().cloned().collect();
            markets.sort();

            if markets.is_empty() {
                continue;
            }

            let domain_str = domain.to_string();
            let pool_ref = pool.clone();
            let http_ref = http.clone();
            let last_seen = last_seen_by_market.clone();
            let alert_cfg_ref = alert_cfg.clone();
            let alert_state_ref = alert_state.clone();

            futures_util::stream::iter(markets)
                .for_each_concurrent(max_concurrency, |condition_id| {
                    let pool = pool_ref.clone();
                    let http = http_ref.clone();
                    let domain = domain_str.clone();
                    let last_seen = last_seen.clone();
                    let alert_cfg = alert_cfg_ref.clone();
                    let alert_state = alert_state_ref.clone();
                    async move {
                        collect_trades_for_market(
                            &http,
                            &pool,
                            &condition_id,
                            &domain,
                            page_limit,
                            max_pages,
                            overlap_secs,
                            &last_seen,
                            alert_cfg,
                            alert_state,
                        )
                        .await;
                    }
                })
                .await;
        }
    });
}

fn spawn_polymarket_trade_persistence_from_collector_targets(
    pool: PgPool,
    agent_id: String,
    domain: Domain,
) {
    tokio::spawn(async move {
        let agent_label = agent_id.clone();

        if let Err(e) = ensure_clob_trade_ticks_table(&pool).await {
            warn!(
                agent = agent_label,
                error = %e,
                "failed to ensure clob_trade_ticks table; trade persistence disabled"
            );
            return;
        }

        let http = match reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("Mozilla/5.0 (ploy)")
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    agent = agent_label,
                    error = %e,
                    "failed to build reqwest client for polymarket data-api; trade persistence disabled"
                );
                return;
            }
        };

        let poll_secs = env_u64("PM_TRADES_POLL_SECS", 10).max(1);
        let page_limit = env_usize("PM_TRADES_PAGE_LIMIT", 200).clamp(1, 1000);
        let max_pages = env_usize("PM_TRADES_MAX_PAGES", 10).clamp(1, 100);
        let overlap_secs = env_i64("PM_TRADES_OVERLAP_SECS", 120).max(0);
        let max_concurrency = env_usize("PM_TRADES_CONCURRENCY", 4).clamp(1, 32);
        let targets_limit = env_usize("PM_TRADES_TARGETS_LIMIT", 200).clamp(1, 5000);

        let mut alert_cfg = TradeAlertConfig::from_env();
        let mut alert_state: Option<Arc<tokio::sync::Mutex<TradeAlertState>>> =
            if alert_cfg.burst_enabled() {
                Some(Arc::new(
                    tokio::sync::Mutex::new(TradeAlertState::default()),
                ))
            } else {
                None
            };

        if alert_cfg.enabled() {
            if let Err(e) = ensure_clob_trade_alerts_table(&pool).await {
                warn!(
                    agent = agent_label,
                    error = %e,
                    "failed to ensure clob_trade_alerts table; trade alerting disabled"
                );
                alert_cfg = TradeAlertConfig::disabled();
                alert_state = None;
            }
        }

        // High-water mark per market to keep polling bounded. We overlap by N seconds and rely
        // on ON CONFLICT DO NOTHING to dedupe safely.
        let last_seen_by_market: Arc<tokio::sync::RwLock<HashMap<String, i64>>> =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        let mut tick = tokio::time::interval(Duration::from_secs(poll_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tick.tick().await;

            let markets: Vec<String> = match sqlx::query_scalar::<_, String>(
                r#"
                SELECT DISTINCT metadata->>'condition_id'
                FROM collector_token_targets
                WHERE domain = 'SPORTS_NBA'
                  AND target_date BETWEEN (CURRENT_DATE - 1) AND (CURRENT_DATE + 1)
                  AND (expires_at IS NULL OR expires_at > NOW())
                  AND (metadata ? 'condition_id')
                  AND COALESCE(metadata->>'condition_id','') <> ''
                ORDER BY 1
                LIMIT $1
                "#,
            )
            .bind(targets_limit as i64)
            .fetch_all(&pool)
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        agent = agent_label,
                        error = %e,
                        "failed to query sports trade targets from collector_token_targets"
                    );
                    continue;
                }
            };

            if markets.is_empty() {
                continue;
            }

            let domain_str = domain.to_string();
            let pool_ref = pool.clone();
            let http_ref = http.clone();
            let last_seen = last_seen_by_market.clone();
            let alert_cfg_ref = alert_cfg.clone();
            let alert_state_ref = alert_state.clone();

            futures_util::stream::iter(markets)
                .for_each_concurrent(max_concurrency, |condition_id| {
                    let pool = pool_ref.clone();
                    let http = http_ref.clone();
                    let domain = domain_str.clone();
                    let last_seen = last_seen.clone();
                    let alert_cfg = alert_cfg_ref.clone();
                    let alert_state = alert_state_ref.clone();
                    async move {
                        collect_trades_for_market(
                            &http,
                            &pool,
                            &condition_id,
                            &domain,
                            page_limit,
                            max_pages,
                            overlap_secs,
                            &last_seen,
                            alert_cfg,
                            alert_state,
                        )
                        .await;
                    }
                })
                .await;
        }
    });
}

#[derive(Debug, Clone, serde::Serialize)]
struct DepthLevelJson {
    price: String,
    size: String,
}

fn parse_depth_levels(
    levels: &[PriceLevel],
    is_bid: bool,
    max_levels: usize,
) -> Vec<DepthLevelJson> {
    use rust_decimal::Decimal;
    let mut parsed: Vec<(Decimal, Decimal)> = Vec::with_capacity(levels.len());

    for lvl in levels {
        let Ok(price) = lvl.price.parse::<Decimal>() else {
            continue;
        };
        let Ok(size) = lvl.size.parse::<Decimal>() else {
            continue;
        };
        parsed.push((price, size));
    }

    if is_bid {
        parsed.sort_by(|a, b| b.0.cmp(&a.0));
    } else {
        parsed.sort_by(|a, b| a.0.cmp(&b.0));
    }

    parsed
        .into_iter()
        .take(max_levels)
        .map(|(price, size)| DepthLevelJson {
            price: price.to_string(),
            size: size.to_string(),
        })
        .collect()
}

fn parse_book_timestamp(ts: &Option<String>) -> Option<chrono::DateTime<Utc>> {
    let raw = ts.as_ref()?;
    let parsed = chrono::DateTime::parse_from_rfc3339(raw).ok()?;
    Some(parsed.with_timezone(&Utc))
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(default)
}

fn env_decimal(name: &str, default: rust_decimal::Decimal) -> rust_decimal::Decimal {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<rust_decimal::Decimal>().ok())
        .unwrap_or(default)
}

fn spawn_clob_orderbook_persistence(
    pm_ws: Arc<PolymarketWebSocket>,
    pool: PgPool,
    agent_id: String,
    domain: Domain,
) {
    tokio::spawn(async move {
        let agent_label = agent_id.clone();
        let context_base = json!({
            "agent_id": agent_id,
        });

        if let Err(e) = ensure_clob_orderbook_snapshots_table(&pool).await {
            warn!(
                agent = agent_label,
                error = %e,
                "failed to ensure clob_orderbook_snapshots table; orderbook persistence disabled"
            );
            return;
        }

        let mut rx = pm_ws.subscribe_books();
        let max_levels = env_usize("PM_ORDERBOOK_LEVELS", 20).clamp(1, 200);
        let min_interval_secs = env_i64("PM_ORDERBOOK_SNAPSHOT_SECS", 60).max(1);

        let mut last_persisted: HashMap<String, (chrono::DateTime<Utc>, Option<String>)> =
            HashMap::new();
        let mut persisted_count: u64 = 0;

        loop {
            match rx.recv().await {
                Ok(book) => {
                    let now = Utc::now();
                    let token_id = book.asset_id.clone();

                    let should_persist = match last_persisted.get(&token_id) {
                        None => true,
                        Some((ts, prev_hash)) => {
                            let elapsed =
                                now.signed_duration_since(*ts).num_seconds() >= min_interval_secs;
                            let changed = match (prev_hash.as_deref(), book.hash.as_deref()) {
                                (Some(a), Some(b)) => a != b,
                                _ => true,
                            };
                            elapsed && changed
                        }
                    };

                    if !should_persist {
                        continue;
                    }

                    let bids = parse_depth_levels(&book.bids, true, max_levels);
                    let asks = parse_depth_levels(&book.asks, false, max_levels);
                    let book_ts = parse_book_timestamp(&book.timestamp);

                    let context = context_base.clone();

                    if let Err(e) = sqlx::query(
                        r#"
                        INSERT INTO clob_orderbook_snapshots
                            (domain, token_id, market, bids, asks, book_timestamp, hash, source, context)
                        VALUES
                            ($1, $2, $3, $4, $5, $6, $7, 'polymarket_ws', $8)
                        "#,
                    )
                    .bind(domain.to_string())
                    .bind(&token_id)
                    .bind(&book.market)
                    .bind(sqlx::types::Json(&bids))
                    .bind(sqlx::types::Json(&asks))
                    .bind(book_ts)
                    .bind(&book.hash)
                    .bind(sqlx::types::Json(context))
                    .execute(&pool)
                    .await
                    {
                        warn!(
                            agent = %agent_label,
                            token_id = %token_id,
                            error = %e,
                            "failed to persist clob orderbook snapshot"
                        );
                        continue;
                    }

                    last_persisted.insert(token_id, (now, book.hash.clone()));
                    persisted_count = persisted_count.saturating_add(1);

                    if persisted_count % 100 == 0 {
                        info!(
                            agent = %agent_label,
                            persisted_count,
                            max_levels,
                            min_interval_secs,
                            "persisted clob orderbook snapshots"
                        );
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(agent = %agent_label, lagged = n, "clob orderbook persistence receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    warn!(agent = %agent_label, "clob orderbook persistence receiver closed");
                    break;
                }
            }
        }
    });
}

/// Top-level config for the platform bootstrap
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformBootstrapConfig {
    pub coordinator: CoordinatorConfig,
    pub enable_crypto: bool,
    pub enable_crypto_lob_ml: bool,
    pub enable_sports: bool,
    pub enable_politics: bool,
    pub dry_run: bool,
    pub crypto: CryptoTradingConfig,
    pub crypto_lob_ml: CryptoLobMlConfig,
    pub sports: SportsTradingConfig,
    pub politics: PoliticsTradingConfig,
}

impl Default for PlatformBootstrapConfig {
    fn default() -> Self {
        Self {
            coordinator: CoordinatorConfig::default(),
            enable_crypto: true,
            enable_crypto_lob_ml: false,
            enable_sports: false,
            enable_politics: false,
            dry_run: true,
            crypto: CryptoTradingConfig::default(),
            crypto_lob_ml: CryptoLobMlConfig::default(),
            sports: SportsTradingConfig::default(),
            politics: PoliticsTradingConfig::default(),
        }
    }
}

impl PlatformBootstrapConfig {
    /// Build from AppConfig, enabling agents based on their config sections
    pub fn from_app_config(app: &AppConfig) -> Self {
        let mut cfg = Self::default();
        cfg.dry_run = app.dry_run.enabled;
        cfg.sports.account_id = app.account.id.clone();

        // Coordinator risk from app config
        cfg.coordinator.risk = crate::platform::RiskConfig {
            max_platform_exposure: app.risk.max_single_exposure_usd,
            max_consecutive_failures: app.risk.max_consecutive_failures,
            daily_loss_limit: app.risk.daily_loss_limit_usd,
            max_spread_bps: 500,
            critical_bypass_exposure: true,
        };

        // Map legacy [strategy]/[risk] values into crypto-agent defaults so
        // platform mode follows deployed config instead of hardcoded defaults.
        cfg.crypto.default_shares = app.strategy.shares.max(1);
        let effective_threshold = app.strategy.effective_sum_target();
        if effective_threshold > rust_decimal::Decimal::ZERO {
            cfg.crypto.sum_threshold = effective_threshold;
        } else if app.strategy.sum_target > rust_decimal::Decimal::ZERO {
            cfg.crypto.sum_threshold = app.strategy.sum_target;
        }
        cfg.crypto.take_profit = app.strategy.profit_buffer.max(rust_decimal::Decimal::ZERO);
        cfg.crypto.risk_params.max_order_value = app.risk.max_single_exposure_usd;
        let max_positions = if app.risk.max_positions > 0 {
            app.risk.max_positions
        } else {
            3
        };
        cfg.crypto.risk_params.max_total_exposure =
            app.risk.max_single_exposure_usd * rust_decimal::Decimal::from(max_positions);
        cfg.crypto.risk_params.max_daily_loss = app.risk.daily_loss_limit_usd;
        cfg.crypto.risk_params.max_unhedged_positions = app.risk.max_positions_per_symbol.max(1);

        // Environment overrides for crypto agent tuning (service-level).
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__COINS") {
            let coins: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_uppercase())
                .collect();
            if !coins.is_empty() {
                cfg.crypto.coins = coins;
            }
        }
        cfg.crypto.sum_threshold =
            env_decimal("PLOY_CRYPTO_AGENT__SUM_THRESHOLD", cfg.crypto.sum_threshold);
        cfg.crypto.default_shares = env_u64(
            "PLOY_CRYPTO_AGENT__DEFAULT_SHARES",
            cfg.crypto.default_shares,
        )
        .max(1);
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__MIN_MOMENTUM_1S") {
            if let Ok(v) = raw.parse::<f64>() {
                if v.is_finite() && v >= 0.0 {
                    cfg.crypto.min_momentum_1s = v;
                }
            }
        }
        cfg.crypto.event_refresh_secs = env_u64(
            "PLOY_CRYPTO_AGENT__EVENT_REFRESH_SECS",
            cfg.crypto.event_refresh_secs,
        )
        .max(1);
        cfg.crypto.min_time_remaining_secs = env_u64(
            "PLOY_CRYPTO_AGENT__MIN_TIME_REMAINING_SECS",
            cfg.crypto.min_time_remaining_secs,
        );
        cfg.crypto.max_time_remaining_secs = env_u64(
            "PLOY_CRYPTO_AGENT__MAX_TIME_REMAINING_SECS",
            cfg.crypto.max_time_remaining_secs,
        );
        if cfg.crypto.max_time_remaining_secs < cfg.crypto.min_time_remaining_secs {
            cfg.crypto.max_time_remaining_secs = cfg.crypto.min_time_remaining_secs;
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__PREFER_CLOSE_TO_END") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto.prefer_close_to_end = true,
                "0" | "false" | "no" | "off" => cfg.crypto.prefer_close_to_end = false,
                _ => {}
            }
        }
        cfg.crypto.take_profit =
            env_decimal("PLOY_CRYPTO_AGENT__TAKE_PROFIT", cfg.crypto.take_profit);
        cfg.crypto.stop_loss = env_decimal("PLOY_CRYPTO_AGENT__STOP_LOSS", cfg.crypto.stop_loss);
        cfg.crypto.heartbeat_interval_secs = env_u64(
            "PLOY_CRYPTO_AGENT__HEARTBEAT_INTERVAL_SECS",
            cfg.crypto.heartbeat_interval_secs,
        )
        .max(1);
        cfg.crypto.risk_params.max_order_value = env_decimal(
            "PLOY_CRYPTO_AGENT__MAX_ORDER_VALUE_USD",
            cfg.crypto.risk_params.max_order_value,
        );
        cfg.crypto.risk_params.max_total_exposure = env_decimal(
            "PLOY_CRYPTO_AGENT__MAX_TOTAL_EXPOSURE_USD",
            cfg.crypto.risk_params.max_total_exposure,
        );
        cfg.crypto.risk_params.max_daily_loss = env_decimal(
            "PLOY_CRYPTO_AGENT__MAX_DAILY_LOSS_USD",
            cfg.crypto.risk_params.max_daily_loss,
        );
        cfg.crypto.risk_params.max_unhedged_positions = env_u64(
            "PLOY_CRYPTO_AGENT__MAX_UNHEDGED_POSITIONS",
            cfg.crypto.risk_params.max_unhedged_positions as u64,
        )
        .max(1) as u32;

        // Optional LOB+ML crypto agent (disabled by default).
        // Default to the same risk envelope as the momentum agent unless overridden.
        cfg.crypto_lob_ml.default_shares = cfg.crypto.default_shares;
        cfg.crypto_lob_ml.take_profit = cfg.crypto.take_profit;
        cfg.crypto_lob_ml.stop_loss = cfg.crypto.stop_loss;
        cfg.crypto_lob_ml.risk_params = cfg.crypto.risk_params.clone();

        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__ENABLED") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.enable_crypto_lob_ml = true,
                "0" | "false" | "no" | "off" => cfg.enable_crypto_lob_ml = false,
                _ => {}
            }
        }

        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__COINS") {
            let coins: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_uppercase())
                .collect();
            if !coins.is_empty() {
                cfg.crypto_lob_ml.coins = coins;
            }
        }

        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_TYPE") {
            let v = raw.trim().to_ascii_lowercase();
            if !v.is_empty() {
                cfg.crypto_lob_ml.model_type = v;
            }
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_PATH") {
            let v = raw.trim();
            if !v.is_empty() {
                cfg.crypto_lob_ml.model_path = Some(v.to_string());
            }
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_VERSION") {
            let v = raw.trim();
            if !v.is_empty() {
                cfg.crypto_lob_ml.model_version = Some(v.to_string());
            }
        }

        cfg.crypto_lob_ml.default_shares = env_u64(
            "PLOY_CRYPTO_LOB_ML__DEFAULT_SHARES",
            cfg.crypto_lob_ml.default_shares,
        )
        .max(1);
        cfg.crypto_lob_ml.min_edge =
            env_decimal("PLOY_CRYPTO_LOB_ML__MIN_EDGE", cfg.crypto_lob_ml.min_edge);
        cfg.crypto_lob_ml.max_entry_price = env_decimal(
            "PLOY_CRYPTO_LOB_ML__MAX_ENTRY_PRICE",
            cfg.crypto_lob_ml.max_entry_price,
        );
        cfg.crypto_lob_ml.event_refresh_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__EVENT_REFRESH_SECS",
            cfg.crypto_lob_ml.event_refresh_secs,
        )
        .max(1);
        cfg.crypto_lob_ml.min_time_remaining_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__MIN_TIME_REMAINING_SECS",
            cfg.crypto_lob_ml.min_time_remaining_secs,
        );
        cfg.crypto_lob_ml.max_time_remaining_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__MAX_TIME_REMAINING_SECS",
            cfg.crypto_lob_ml.max_time_remaining_secs,
        );
        if cfg.crypto_lob_ml.max_time_remaining_secs < cfg.crypto_lob_ml.min_time_remaining_secs {
            cfg.crypto_lob_ml.max_time_remaining_secs = cfg.crypto_lob_ml.min_time_remaining_secs;
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__PREFER_CLOSE_TO_END") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto_lob_ml.prefer_close_to_end = true,
                "0" | "false" | "no" | "off" => cfg.crypto_lob_ml.prefer_close_to_end = false,
                _ => {}
            }
        }
        cfg.crypto_lob_ml.cooldown_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__COOLDOWN_SECS",
            cfg.crypto_lob_ml.cooldown_secs,
        );
        cfg.crypto_lob_ml.max_lob_snapshot_age_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__MAX_LOB_SNAPSHOT_AGE_SECS",
            cfg.crypto_lob_ml.max_lob_snapshot_age_secs,
        )
        .max(1);

        // Weight overrides (baseline logistic model).
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__W_BIAS") {
            if let Ok(v) = raw.parse::<f64>() {
                if v.is_finite() {
                    cfg.crypto_lob_ml.weights.bias = v;
                }
            }
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__W_OBI_5") {
            if let Ok(v) = raw.parse::<f64>() {
                if v.is_finite() {
                    cfg.crypto_lob_ml.weights.w_obi_5 = v;
                }
            }
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__W_OBI_10") {
            if let Ok(v) = raw.parse::<f64>() {
                if v.is_finite() {
                    cfg.crypto_lob_ml.weights.w_obi_10 = v;
                }
            }
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__W_MOMENTUM_1S") {
            if let Ok(v) = raw.parse::<f64>() {
                if v.is_finite() {
                    cfg.crypto_lob_ml.weights.w_momentum_1s = v;
                }
            }
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__W_MOMENTUM_5S") {
            if let Ok(v) = raw.parse::<f64>() {
                if v.is_finite() {
                    cfg.crypto_lob_ml.weights.w_momentum_5s = v;
                }
            }
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__W_SPREAD_BPS") {
            if let Ok(v) = raw.parse::<f64>() {
                if v.is_finite() {
                    cfg.crypto_lob_ml.weights.w_spread_bps = v;
                }
            }
        }

        // Enable sports if NBA comeback config is present and enabled
        if let Some(ref nba) = app.nba_comeback {
            if nba.enabled {
                cfg.enable_sports = true;
                // Keep the agent poll cadence aligned with the NBA comeback config.
                cfg.sports.poll_interval_secs = nba.espn_poll_interval_secs;
            }
        }

        // Enable politics if event edge config is present and enabled
        if let Some(ref ee) = app.event_edge_agent {
            if ee.enabled {
                cfg.enable_politics = true;
            }
        }

        cfg
    }
}

/// Optional control commands to apply immediately after platform startup.
#[derive(Debug, Clone, Default)]
pub struct PlatformStartControl {
    pub pause: Option<String>,
    pub resume: Option<String>,
}

/// Start the multi-agent platform
///
/// Creates shared infrastructure, registers configured agents,
/// and runs the coordinator loop until shutdown.
pub async fn start_platform(
    config: PlatformBootstrapConfig,
    pm_client: PolymarketClient,
    app_config: &AppConfig,
    control: PlatformStartControl,
) -> Result<()> {
    let account_id = if app_config.account.id.trim().is_empty() {
        "default".to_string()
    } else {
        app_config.account.id.clone()
    };

    info!(
        account_id = %account_id,
        crypto = config.enable_crypto,
        sports = config.enable_sports,
        politics = config.enable_politics,
        dry_run = config.dry_run,
        "starting multi-agent platform"
    );

    // 1. Create shared executor
    let exec_config = crate::config::ExecutionConfig::default();
    let executor = Arc::new(OrderExecutor::new(pm_client.clone(), exec_config));

    // Optional shared DB pool used for (a) coordinator execution logs and (b) market data persistence.
    // Crypto agents can run without DB; sports agent requires DB for calendar/stats.
    let shared_pool = match PgPoolOptions::new()
        .max_connections(app_config.database.max_connections)
        .connect(&app_config.database.url)
        .await
    {
        Ok(pool) => Some(pool),
        Err(e) => {
            warn!(
                error = %e,
                "failed to connect DB at startup; continuing without shared pool"
            );
            None
        }
    };

    // 2. Create coordinator
    let mut coordinator =
        Coordinator::new(config.coordinator.clone(), executor, account_id.clone());
    if let Some(pool) = shared_pool.as_ref() {
        ensure_schema_repairs(pool).await?;
        if let Err(e) = ensure_accounts_table(pool).await {
            warn!(error = %e, "failed to ensure accounts table");
        } else if let Err(e) =
            upsert_account_from_config(pool, &account_id, &app_config.account).await
        {
            warn!(error = %e, "failed to upsert account metadata");
        }
        if let Err(e) = ensure_agent_order_executions_table(pool).await {
            warn!(error = %e, "failed to ensure agent_order_executions table; execution logging disabled");
        } else {
            coordinator.set_execution_log_pool(pool.clone());
        }
        if let Err(e) = ensure_strategy_observability_tables(pool).await {
            warn!(error = %e, "failed to ensure strategy observability tables");
        }
        if let Err(e) = ensure_pm_token_settlements_table(pool).await {
            warn!(error = %e, "failed to ensure pm_token_settlements table");
        }
        if config.enable_crypto {
            if let Err(e) = ensure_clob_trade_alerts_table(pool).await {
                warn!(
                    error = %e,
                    "failed to ensure clob_trade_alerts table at startup"
                );
            }
        }
    }
    let handle = coordinator.handle();
    let _global_state = coordinator.global_state();

    // 3. Shutdown broadcast channel
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // 4. Spawn agents
    let mut agent_handles = Vec::new();

    if config.enable_crypto {
        let crypto_cfg = config.crypto.clone();
        let risk_params = crypto_cfg.risk_params.clone();
        let cmd_rx = coordinator.register_agent(crypto_cfg.agent_id.clone(), risk_params);
        let lob_cfg = config.crypto_lob_ml.clone();
        let lob_agent_enabled = config.enable_crypto_lob_ml;

        // Discover active crypto events and token IDs (Gamma API) via EventMatcher
        let event_matcher = Arc::new(EventMatcher::new(pm_client.clone()));
        if let Err(e) = event_matcher.refresh().await {
            warn!(error = %e, "crypto event matcher refresh failed (continuing)");
        }

        // Build a unified coin set across all enabled crypto strategies.
        let mut all_coins = crypto_cfg.coins.clone();
        if lob_agent_enabled {
            for coin in &lob_cfg.coins {
                if !all_coins.contains(coin) {
                    all_coins.push(coin.clone());
                }
            }
        }

        // Create WebSocket feeds
        let symbols: Vec<String> = all_coins.iter().map(|c| format!("{}USDT", c)).collect();
        let binance_ws = Arc::new(BinanceWebSocket::new(symbols));
        let pm_ws = Arc::new(PolymarketWebSocket::new(&app_config.market.ws_url));

        // Seed PM token → side mapping for data collection, so QuoteUpdates carry the correct
        // UP/DOWN side and can be persisted to Postgres.
        //
        // IMPORTANT: Keep the collector subscription set bounded. The trading agent only adds
        // tokens; without pruning, the WS subscription grows forever and can overwhelm the box.
        let collector_min_remaining_secs = env_i64("PM_COLLECTOR_MIN_REMAINING_SECS", 0)
            .max(-86400)
            .min(86400);
        let mut desired: HashMap<String, Side> = HashMap::new();
        let mut collector_targets: Vec<crate::collector::CollectorTokenTarget> = Vec::new();
        for coin in &all_coins {
            let symbol = format!("{}USDT", coin.to_uppercase());
            for ev in event_matcher
                .get_events_with_min_remaining(&symbol, collector_min_remaining_secs)
                .await
            {
                desired.insert(ev.up_token_id.clone(), Side::Up);
                desired.insert(ev.down_token_id.clone(), Side::Down);

                // Feed the L2 orderbook-history collector with an explicit token target list.
                // This prevents "collect everything" behavior when other markets become active.
                let expires_at = Some(ev.end_time + chrono::Duration::hours(1));
                collector_targets.push(
                    crate::collector::CollectorTokenTarget::new(ev.up_token_id.clone(), "CRYPTO")
                        .with_expires_at(expires_at)
                        .with_metadata(serde_json::json!({
                            "symbol": symbol.as_str(),
                            "side": "UP",
                            "condition_id": ev.condition_id.as_str(),
                            "slug": ev.slug.as_str(),
                            "title": ev.title.as_str(),
                        })),
                );
                collector_targets.push(
                    crate::collector::CollectorTokenTarget::new(ev.down_token_id.clone(), "CRYPTO")
                        .with_expires_at(expires_at)
                        .with_metadata(serde_json::json!({
                            "symbol": symbol.as_str(),
                            "side": "DOWN",
                            "condition_id": ev.condition_id.as_str(),
                            "slug": ev.slug.as_str(),
                            "title": ev.title.as_str(),
                        })),
                );
            }
        }
        let (_added, _removed, _updated, total) = pm_ws.reconcile_token_sides(&desired).await;
        info!(
            agent = %crypto_cfg.agent_id,
            token_count = total,
            "seeded PM token mappings for crypto data collection"
        );

        if let Some(pool) = shared_pool.as_ref() {
            if let Err(e) = crate::collector::ensure_collector_token_targets_table(pool).await {
                warn!(
                    agent = %crypto_cfg.agent_id,
                    error = %e,
                    "failed to ensure collector_token_targets table"
                );
            }

            if let Err(e) =
                crate::collector::upsert_collector_token_targets(pool, &collector_targets).await
            {
                warn!(
                    agent = %crypto_cfg.agent_id,
                    error = %e,
                    "failed to upsert collector token targets (crypto)"
                );
            }
        }

        // Keep refreshing the subscription token set over time so 5m + 15m markets continue
        // to be recorded throughout the day, independent of which single market the agent
        // is currently trading.
        let pm_ws_collector = pm_ws.clone();
        let matcher_collector = event_matcher.clone();
        let coins_collector = all_coins.clone();
        let agent_id_collector = crypto_cfg.agent_id.clone();
        let pool_collector = shared_pool.clone();
        tokio::spawn(async move {
            let refresh_secs =
                env_u64("PM_COLLECTOR_REFRESH_SECS", PM_COLLECTOR_REFRESH_SECS).max(10);
            let mut tick = tokio::time::interval(Duration::from_secs(refresh_secs));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                tick.tick().await;

                if let Err(e) = matcher_collector.refresh().await {
                    warn!(agent = %agent_id_collector, error = %e, "pm token collector refresh failed");
                    continue;
                }

                let mut desired: HashMap<String, Side> = HashMap::new();
                let mut collector_targets: Vec<crate::collector::CollectorTokenTarget> = Vec::new();
                for coin in &coins_collector {
                    let symbol = format!("{}USDT", coin.to_uppercase());
                    for ev in matcher_collector
                        .get_events_with_min_remaining(&symbol, collector_min_remaining_secs)
                        .await
                    {
                        desired.insert(ev.up_token_id.clone(), Side::Up);
                        desired.insert(ev.down_token_id.clone(), Side::Down);

                        let expires_at = Some(ev.end_time + chrono::Duration::hours(1));
                        collector_targets.push(
                            crate::collector::CollectorTokenTarget::new(
                                ev.up_token_id.clone(),
                                "CRYPTO",
                            )
                            .with_expires_at(expires_at)
                            .with_metadata(serde_json::json!({
                                "symbol": symbol.as_str(),
                                "side": "UP",
                                "condition_id": ev.condition_id.as_str(),
                                "slug": ev.slug.as_str(),
                                "title": ev.title.as_str(),
                            })),
                        );
                        collector_targets.push(
                            crate::collector::CollectorTokenTarget::new(
                                ev.down_token_id.clone(),
                                "CRYPTO",
                            )
                            .with_expires_at(expires_at)
                            .with_metadata(serde_json::json!({
                                "symbol": symbol.as_str(),
                                "side": "DOWN",
                                "condition_id": ev.condition_id.as_str(),
                                "slug": ev.slug.as_str(),
                                "title": ev.title.as_str(),
                            })),
                        );
                    }
                }

                let (added, removed, updated, total) =
                    pm_ws_collector.reconcile_token_sides(&desired).await;
                if added > 0 || removed > 0 {
                    pm_ws_collector.request_resubscribe();
                    info!(
                        agent = %agent_id_collector,
                        added,
                        removed,
                        updated,
                        token_count = total,
                        "pm token collector reconciled token set; resubscribe requested"
                    );
                }

                if let Some(pool) = pool_collector.as_ref() {
                    // Table may not exist if migrations were not applied; ensure it.
                    let ensured =
                        crate::collector::ensure_collector_token_targets_table(pool).await;
                    if let Err(e) = ensured {
                        warn!(
                            agent = %agent_id_collector,
                            error = %e,
                            "failed to ensure collector_token_targets table"
                        );
                    }

                    if let Err(e) =
                        crate::collector::upsert_collector_token_targets(pool, &collector_targets)
                            .await
                    {
                        warn!(
                            agent = %agent_id_collector,
                            error = %e,
                            "failed to upsert collector token targets (crypto)"
                        );
                    }
                }
            }
        });

        // Optional persistence pipeline for CLOB quotes (best-effort).
        // Do not block agent startup if DB is temporarily unavailable.
        if let Some(pool) = shared_pool.as_ref() {
            spawn_clob_quote_persistence(pm_ws.clone(), pool.clone(), crypto_cfg.agent_id.clone());
            spawn_clob_orderbook_persistence(
                pm_ws.clone(),
                pool.clone(),
                crypto_cfg.agent_id.clone(),
                Domain::Crypto,
            );
            spawn_binance_price_persistence(
                binance_ws.clone(),
                pool.clone(),
                crypto_cfg.agent_id.clone(),
            );
            spawn_polymarket_trade_persistence(
                event_matcher.clone(),
                pool.clone(),
                crypto_cfg.agent_id.clone(),
                crypto_cfg.coins.clone(),
                Domain::Crypto,
            );
            info!(
                agent = crypto_cfg.agent_id,
                "market data persistence tasks started"
            );
        }

        // Optional Binance LOB depth stream (for ML/RL feature generation).
        let mut enable_binance_lob = lob_agent_enabled;
        if let Ok(raw) = std::env::var("PLOY_BINANCE_LOB__ENABLED") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => enable_binance_lob = true,
                "0" | "false" | "no" | "off" => enable_binance_lob = false,
                _ => {}
            }
        }

        let mut lob_cache_opt: Option<crate::collector::LobCache> = None;
        if enable_binance_lob {
            let depth_symbols: Vec<String> = match std::env::var("PLOY_BINANCE_LOB__SYMBOLS") {
                Ok(raw) => raw
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_ascii_uppercase())
                    .collect(),
                Err(_) => all_coins.iter().map(|c| format!("{}USDT", c)).collect(),
            };

            let depth_stream = Arc::new(crate::collector::BinanceDepthStream::new(depth_symbols));
            let lob_cache = depth_stream.cache().clone();
            lob_cache_opt = Some(lob_cache.clone());

            if let Some(pool) = shared_pool.as_ref() {
                spawn_binance_lob_persistence(
                    depth_stream.clone(),
                    pool.clone(),
                    crypto_cfg.agent_id.clone(),
                );
            }

            let ds = depth_stream.clone();
            tokio::spawn(async move {
                if let Err(e) = ds.run().await {
                    error!(error = %e, "binance depth stream error");
                }
            });

            info!(
                agent = crypto_cfg.agent_id,
                "binance LOB depth stream started"
            );
        }

        // Spawn Binance WS in background
        let bws = binance_ws.clone();
        tokio::spawn(async move {
            if let Err(e) = bws.run().await {
                error!(error = %e, "binance websocket error");
            }
        });

        // Spawn PM WS in background
        let pws = pm_ws.clone();
        tokio::spawn(async move {
            if let Err(e) = pws.run(Vec::new()).await {
                error!(error = %e, "polymarket websocket error");
            }
        });

        let agent = CryptoTradingAgent::new(
            crypto_cfg.clone(),
            binance_ws.clone(),
            pm_ws.clone(),
            event_matcher.clone(),
        );
        let ctx = AgentContext::new(
            crypto_cfg.agent_id.clone(),
            Domain::Crypto,
            handle.clone(),
            cmd_rx,
        );

        let jh = tokio::spawn(async move {
            if let Err(e) = agent.run(ctx).await {
                error!(agent = "crypto", error = %e, "agent exited with error");
            }
        });
        agent_handles.push(jh);
        info!("crypto agent spawned");

        if lob_agent_enabled {
            if let Some(lob_cache) = lob_cache_opt.clone() {
                let risk_params = lob_cfg.risk_params.clone();
                let cmd_rx = coordinator.register_agent(lob_cfg.agent_id.clone(), risk_params);

                let agent = CryptoLobMlAgent::new(
                    lob_cfg.clone(),
                    binance_ws.clone(),
                    pm_ws.clone(),
                    event_matcher.clone(),
                    lob_cache,
                );
                let ctx = AgentContext::new(
                    lob_cfg.agent_id.clone(),
                    Domain::Crypto,
                    handle.clone(),
                    cmd_rx,
                );

                let jh = tokio::spawn(async move {
                    if let Err(e) = agent.run(ctx).await {
                        error!(agent = "crypto_lob_ml", error = %e, "agent exited with error");
                    }
                });
                agent_handles.push(jh);
                info!("crypto lob-ml agent spawned");
            } else {
                warn!(
                    agent = lob_cfg.agent_id,
                    "lob agent enabled but binance depth stream is disabled; skipping agent spawn"
                );
            }
        }
    }

    if config.enable_sports {
        if let Some(ref nba_cfg) = app_config.nba_comeback {
            let sports_cfg = config.sports.clone();
            let risk_params = sports_cfg.risk_params.clone();
            let cmd_rx = coordinator.register_agent(sports_cfg.agent_id.clone(), risk_params);

            let pool = match shared_pool.as_ref() {
                Some(pool) => pool.clone(),
                None => {
                    PgPoolOptions::new()
                        .max_connections(app_config.database.max_connections)
                        .connect(&app_config.database.url)
                        .await?
                }
            };
            if let Err(e) = ensure_clob_orderbook_snapshots_table(&pool).await {
                warn!(agent = sports_cfg.agent_id, error = %e, "failed to ensure clob_orderbook_snapshots table");
            }
            spawn_polymarket_trade_persistence_from_collector_targets(
                pool.clone(),
                sports_cfg.agent_id.clone(),
                Domain::Sports,
            );

            let espn = crate::strategy::nba_comeback::espn::EspnClient::new();
            let stats = crate::strategy::nba_comeback::ComebackStatsProvider::new(
                pool.clone(),
                nba_cfg.season.clone(),
            );
            let core =
                crate::strategy::nba_comeback::NbaComebackCore::new(espn, stats, nba_cfg.clone());
            let mut agent =
                SportsTradingAgent::new(sports_cfg.clone(), core).with_observation_pool(pool);
            match PolymarketSportsClient::new() {
                Ok(pm_sports) => {
                    agent = agent.with_pm_sports(pm_sports);
                }
                Err(e) => {
                    warn!(
                        agent = sports_cfg.agent_id,
                        error = %e,
                        "failed to initialize PolymarketSportsClient; continuing without PM market observations"
                    );
                }
            }
            if nba_cfg.grok_enabled {
                match crate::agent::grok::GrokClient::from_env() {
                    Ok(grok) if grok.is_configured() => {
                        info!(
                            agent = sports_cfg.agent_id,
                            "grok live search enabled for sports agent"
                        );
                        agent = agent.with_grok(grok);
                    }
                    Ok(_) => {
                        warn!(
                            agent = sports_cfg.agent_id,
                            "grok_enabled=true but GROK_API_KEY not set; continuing without Grok"
                        );
                    }
                    Err(e) => {
                        warn!(
                            agent = sports_cfg.agent_id,
                            error = %e,
                            "failed to initialize GrokClient; continuing without Grok"
                        );
                    }
                }
            }
            let ctx = AgentContext::new(
                sports_cfg.agent_id.clone(),
                Domain::Sports,
                handle.clone(),
                cmd_rx,
            );

            let jh = tokio::spawn(async move {
                if let Err(e) = agent.run(ctx).await {
                    error!(agent = "sports", error = %e, "agent exited with error");
                }
            });
            agent_handles.push(jh);
            info!("sports agent spawned");
        }
    }

    if config.enable_politics {
        if let Some(ref ee_cfg) = app_config.event_edge_agent {
            let politics_cfg = config.politics.clone();
            let risk_params = politics_cfg.risk_params.clone();
            let cmd_rx = coordinator.register_agent(politics_cfg.agent_id.clone(), risk_params);

            let core = EventEdgeCore::new(pm_client.clone(), ee_cfg.clone());
            let agent = PoliticsTradingAgent::new(politics_cfg.clone(), core);
            let ctx = AgentContext::new(
                politics_cfg.agent_id.clone(),
                Domain::Politics,
                handle.clone(),
                cmd_rx,
            );

            let jh = tokio::spawn(async move {
                if let Err(e) = agent.run(ctx).await {
                    error!(agent = "politics", error = %e, "agent exited with error");
                }
            });
            agent_handles.push(jh);
            info!("politics agent spawned");
        }
    }

    info!(
        agents = agent_handles.len(),
        "all agents spawned, starting coordinator"
    );

    // 4b. Apply initial control commands (pause/resume)
    if let Some(agent_id) = control.pause.as_deref() {
        if agent_id == "all" {
            coordinator.pause_all().await;
        } else if let Err(e) = coordinator
            .send_command(agent_id, crate::coordinator::CoordinatorCommand::Pause)
            .await
        {
            warn!(agent_id, error = %e, "failed to pause agent at startup");
        }
    } else if let Some(agent_id) = control.resume.as_deref() {
        if agent_id == "all" {
            coordinator.resume_all().await;
        } else if let Err(e) = coordinator
            .send_command(agent_id, crate::coordinator::CoordinatorCommand::Resume)
            .await
        {
            warn!(agent_id, error = %e, "failed to resume agent at startup");
        }
    }

    // 5. Run coordinator (blocks until shutdown signal)
    let shutdown_rx = shutdown_tx.subscribe();

    // Spawn Ctrl+C handler
    let stx = shutdown_tx.clone();
    tokio::spawn(async move {
        if let Ok(()) = tokio::signal::ctrl_c().await {
            info!("Ctrl+C received, initiating shutdown");
            let _ = stx.send(());
        }
    });

    coordinator.run(shutdown_rx).await;

    // 6. Wait for agents to finish (with timeout)
    info!("waiting for agents to finish...");
    let timeout = tokio::time::Duration::from_secs(10);
    for jh in agent_handles {
        let _ = tokio::time::timeout(timeout, jh).await;
    }

    info!("platform shutdown complete");
    Ok(())
}

/// Print the current global state (for `ploy platform status`)
pub fn print_platform_status(state: &GlobalState) {
    println!("=== Platform Status ===");
    println!(
        "Started: {} | Last refresh: {}",
        state.started_at.format("%H:%M:%S"),
        state.last_refresh.format("%H:%M:%S")
    );
    println!("Risk state: {:?}", state.risk_state);
    println!(
        "Portfolio: exposure={} unrealized_pnl={} realized_pnl={}",
        state.total_exposure(),
        state.total_unrealized_pnl(),
        state.total_realized_pnl
    );
    println!(
        "Queue: size={} enqueued={} dequeued={}",
        state.queue_stats.current_size,
        state.queue_stats.enqueued_total,
        state.queue_stats.dequeued_total
    );
    println!("\n--- Agents ({}) ---", state.agents.len());
    for (id, agent) in &state.agents {
        println!(
            "  {} [{}] {:?} | pos={} exp={} pnl={} | hb={}",
            id,
            agent.name,
            agent.status,
            agent.position_count,
            agent.exposure,
            agent.daily_pnl,
            agent.last_heartbeat.format("%H:%M:%S")
        );
    }
}
