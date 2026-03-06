//! Platform Bootstrap — wires up Coordinator + Agents from config
//!
//! Entry point for `ploy platform start`. Creates shared infrastructure,
//! registers agents based on config flags, and runs the coordinator loop.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, trace, warn};

use crate::adapters::polymarket_clob::POLYGON_CHAIN_ID;
use crate::adapters::{BinanceWebSocket, PolymarketClient, PolymarketWebSocket, PostgresStore};
use crate::agents::crypto::CryptoEntryMode;
use crate::agents::sports::SportsTradingAgent;
use crate::agents::{
    AgentContext, CryptoLobMlAgent, CryptoLobMlConfig, CryptoLobMlEntrySidePolicy,
    CryptoLobMlExitMode, CryptoTradingConfig, OpenClawAgent, OpenClawConfig, PoliticsTradingConfig,
    SportsTradingConfig, TradingAgent,
};
#[cfg(feature = "rl")]
use crate::agents::{CryptoRlPolicyAgent, CryptoRlPolicyConfig};
use crate::ai_clients::PolymarketSportsClient;
use crate::config::AppConfig;
use crate::coordinator::config::DuplicateGuardScope;
use crate::coordinator::{
    Coordinator, CoordinatorConfig, CoordinatorHandle, GlobalState,
};
use crate::domain::{OrderStatus, Side};
use crate::error::Result;
use crate::exchange::{build_exchange_client, parse_exchange_kind, ExchangeKind};
use crate::platform::{
    AgentRiskParams, BinanceDataPlaneHandle, CryptoDataPlaneHandle, DataPlaneConfig, Domain,
    MarketSelector, PlatformDataPlane, StrategyDeployment,
};
use crate::signing::Wallet;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::idempotency::IdempotencyManager;
use crate::strategy::momentum::EventMatcher;
use crate::strategy::{DataFeed, StrategyAction, StrategyManager};
use chrono::Utc;
use futures_util::StreamExt;
use polymarket_client_sdk::data::types::request::TradesRequest as DataTradesRequest;
use polymarket_client_sdk::data::types::MarketFilter as DataMarketFilter;
use polymarket_client_sdk::data::Client as DataApiClient;

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::instrument;

use super::strategy_runtime::{
    run_managed_strategy_runtime as run_managed_strategy_runtime_module,
    ManagedStrategyRuntimeConfig,
};

const CLOB_PERSIST_MIN_INTERVAL_SECS: i64 = 2;
const BINANCE_PERSIST_MIN_INTERVAL_SECS: i64 = 1;
const PM_COLLECTOR_REFRESH_SECS: u64 = 300;

async fn ensure_clob_quote_ticks_table(pool: &PgPool) -> Result<()> {
    crate::platform::persistence_schema::ensure_clob_quote_ticks_table(pool).await
}

async fn ensure_binance_price_ticks_table(pool: &PgPool) -> Result<()> {
    crate::platform::persistence_schema::ensure_binance_price_ticks_table(pool).await
}

async fn ensure_binance_lob_ticks_table(pool: &PgPool) -> Result<()> {
    crate::platform::persistence_schema::ensure_binance_lob_ticks_table(pool).await
}

pub(crate) async fn ensure_clob_orderbook_snapshots_table(pool: &PgPool) -> Result<()> {
    crate::platform::persistence_schema::ensure_clob_orderbook_snapshots_table(pool).await
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

    // strategy_evaluations is migration-owned; only run lightweight startup repairs when present.
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
        warn!("strategy_evaluations table missing at startup; run migrations to enable deployment evidence gating");
    }

    Ok(())
}

async fn ensure_schema_repairs(pool: &PgPool) -> Result<()> {
    // These repairs remain startup-applied to harden mixed-version upgrades.
    // `platform start` also runs the sqlx migration runner before this step.
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
                -- positions multi-account scoping
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
                -- fills(timestamp) indexes (fallback to filled_at for older schemas)
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
                -- Reconcile quote_freshness drift from partial/older migrations.
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

pub(crate) fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

#[instrument(skip(data_client, pool, last_seen_by_market))]
async fn collect_trades_for_market(
    data_client: &DataApiClient,
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
    let page_limit_i32 = i32::try_from(page_limit).unwrap_or(1000);

    for page in 0..max_pages {
        let offset = page.saturating_mul(page_limit);
        if offset > 10_000 {
            debug!(
                condition_id,
                offset, "stopping data-api trades pagination at offset > 10000 (SDK bound)"
            );
            break;
        }
        let offset_i32 = match i32::try_from(offset) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    condition_id,
                    error = %e,
                    offset,
                    "failed to convert pagination offset for data-api trades"
                );
                return;
            }
        };

        let cid_b256: alloy::primitives::B256 = condition_id.parse().unwrap_or_default();
        let req_builder =
            DataTradesRequest::builder().filter(DataMarketFilter::markets([cid_b256]));
        let req_builder = match req_builder.limit(page_limit_i32) {
            Ok(builder) => builder,
            Err(e) => {
                warn!(
                    condition_id,
                    error = %e,
                    limit = page_limit_i32,
                    "invalid data-api trades limit"
                );
                return;
            }
        };
        let req_builder = match req_builder.offset(offset_i32) {
            Ok(builder) => builder,
            Err(e) => {
                warn!(
                    condition_id,
                    error = %e,
                    offset = offset_i32,
                    "invalid data-api trades offset"
                );
                return;
            }
        };
        let req = req_builder.build();

        let trades =
            match tokio::time::timeout(Duration::from_secs(15), data_client.trades(&req)).await {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    warn!(
                        condition_id,
                        error = %e,
                        "failed to fetch polymarket data-api trades via SDK"
                    );
                    return;
                }
                Err(_) => {
                    warn!(
                        condition_id,
                        "timed out fetching polymarket data-api trades via SDK"
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
        let mut rows: Vec<&polymarket_client_sdk::data::types::response::Trade> =
            Vec::with_capacity(trades.len());
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
                let side = t.side.to_string();
                let proxy_wallet = format!("{:#x}", t.proxy_wallet);
                let cond_id_str = t.condition_id.to_string();
                let asset_str = t.asset.to_string();
                let tx_hash_str = t.transaction_hash.to_string();

                b.push_bind(domain)
                    .push_bind(cond_id_str)
                    .push_bind(asset_str)
                    .push_bind(side)
                    .push_bind(t.size)
                    .push_bind(t.price)
                    .push_bind(trade_ts.unwrap_or_else(Utc::now))
                    .push_bind(t.timestamp)
                    .push_bind(tx_hash_str)
                    .push_bind(proxy_wallet)
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

        let data_client = Arc::new(DataApiClient::default());

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
            let data_client_ref = data_client.clone();
            let last_seen = last_seen_by_market.clone();
            let alert_cfg_ref = alert_cfg.clone();
            let alert_state_ref = alert_state.clone();

            futures_util::stream::iter(markets)
                .for_each_concurrent(max_concurrency, |condition_id| {
                    let pool = pool_ref.clone();
                    let data_client = data_client_ref.clone();
                    let domain = domain_str.clone();
                    let last_seen = last_seen.clone();
                    let alert_cfg = alert_cfg_ref.clone();
                    let alert_state = alert_state_ref.clone();
                    async move {
                        collect_trades_for_market(
                            data_client.as_ref(),
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

        let data_client = Arc::new(DataApiClient::default());

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
            let data_client_ref = data_client.clone();
            let last_seen = last_seen_by_market.clone();
            let alert_cfg_ref = alert_cfg.clone();
            let alert_state_ref = alert_state.clone();

            futures_util::stream::iter(markets)
                .for_each_concurrent(max_concurrency, |condition_id| {
                    let pool = pool_ref.clone();
                    let data_client = data_client_ref.clone();
                    let domain = domain_str.clone();
                    let last_seen = last_seen.clone();
                    let alert_cfg = alert_cfg_ref.clone();
                    let alert_state = alert_state_ref.clone();
                    async move {
                        collect_trades_for_market(
                            data_client.as_ref(),
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

#[derive(Debug, Default, Clone, Copy)]
struct SettlementRefreshStats {
    targeted_tokens: usize,
    refreshed_markets: usize,
    upserted_rows: usize,
    resolved_markets: usize,
}

fn spawn_pm_token_settlement_persistence(
    pm_client: PolymarketClient,
    pool: PgPool,
    agent_id: String,
    collector_domains: Vec<&'static str>,
) {
    tokio::spawn(async move {
        if let Err(e) = ensure_pm_token_settlements_table(&pool).await {
            warn!(
                agent = %agent_id,
                error = %e,
                "failed to ensure pm_token_settlements table; settlement persistence disabled"
            );
            return;
        }

        let poll_secs = env_u64("PM_SETTLEMENT_POLL_SECS", 120).max(10);
        let targets_limit = env_usize("PM_SETTLEMENT_TARGETS_LIMIT", 1000).clamp(1, 10000);
        let unresolved_limit = env_usize("PM_SETTLEMENT_UNRESOLVED_LIMIT", 1000).clamp(1, 10000);
        let lookback_secs = env_i64("PM_SETTLEMENT_TARGET_LOOKBACK_SECS", 86400).max(0);
        let max_tokens_per_cycle =
            env_usize("PM_SETTLEMENT_MAX_TOKENS_PER_CYCLE", 200).clamp(1, 5000);
        let max_concurrency = env_usize("PM_SETTLEMENT_CONCURRENCY", 2).clamp(1, 32);

        let collector_domains_label = collector_domains.join(",");

        let mut tick = tokio::time::interval(Duration::from_secs(poll_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tick.tick().await;

            match refresh_pm_token_settlements_for_domains(
                &pm_client,
                &pool,
                &collector_domains,
                targets_limit,
                unresolved_limit,
                lookback_secs,
                max_tokens_per_cycle,
                max_concurrency,
            )
            .await
            {
                Ok(stats) => {
                    if stats.targeted_tokens > 0
                        && (stats.resolved_markets > 0 || stats.upserted_rows > 0)
                    {
                        info!(
                            agent = %agent_id,
                            collector_domains = %collector_domains_label,
                            targeted_tokens = stats.targeted_tokens,
                            refreshed_markets = stats.refreshed_markets,
                            upserted_rows = stats.upserted_rows,
                            resolved_markets = stats.resolved_markets,
                            "pm settlement persistence cycle complete"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        agent = %agent_id,
                        collector_domains = %collector_domains_label,
                        error = %e,
                        "pm settlement persistence cycle failed"
                    );
                }
            }
        }
    });
}

async fn refresh_pm_token_settlements_for_domains(
    pm_client: &PolymarketClient,
    pool: &PgPool,
    collector_domains: &[&str],
    targets_limit: usize,
    unresolved_limit: usize,
    lookback_secs: i64,
    max_tokens_per_cycle: usize,
    max_concurrency: usize,
) -> Result<SettlementRefreshStats> {
    use std::collections::BTreeSet;

    let mut token_ids: BTreeSet<String> = BTreeSet::new();

    // 1) Active/recent collector targets (seed for upcoming or just-ended markets).
    for domain in collector_domains {
        let scoped_targets = sqlx::query_scalar::<_, String>(
            r#"
            SELECT token_id
            FROM collector_token_targets
            WHERE domain = $1
              AND (
                    expires_at IS NULL
                 OR expires_at > NOW() - ($2::bigint * INTERVAL '1 second')
              )
            ORDER BY updated_at DESC
            LIMIT $3
            "#,
        )
        .bind(*domain)
        .bind(lookback_secs)
        .bind(targets_limit as i64)
        .fetch_all(pool)
        .await?;
        for token_id in scoped_targets {
            if !token_id.trim().is_empty() {
                token_ids.insert(token_id);
            }
        }
    }

    // 2) Keep refreshing unresolved outcomes until they finalize.
    let unresolved_targets = sqlx::query_scalar::<_, String>(
        r#"
        SELECT token_id
        FROM pm_token_settlements
        WHERE resolved = FALSE
        ORDER BY fetched_at DESC
        LIMIT $1
        "#,
    )
    .bind(unresolved_limit as i64)
    .fetch_all(pool)
    .await?;
    for token_id in unresolved_targets {
        if !token_id.trim().is_empty() {
            token_ids.insert(token_id);
        }
    }

    let mut token_ids: Vec<String> = token_ids.into_iter().collect();
    if token_ids.is_empty() {
        return Ok(SettlementRefreshStats::default());
    }
    if token_ids.len() > max_tokens_per_cycle {
        token_ids.truncate(max_tokens_per_cycle);
    }

    let seen_conditions: Arc<tokio::sync::Mutex<HashSet<String>>> =
        Arc::new(tokio::sync::Mutex::new(HashSet::new()));
    let stats: Arc<tokio::sync::Mutex<SettlementRefreshStats>> =
        Arc::new(tokio::sync::Mutex::new(SettlementRefreshStats {
            targeted_tokens: token_ids.len(),
            ..SettlementRefreshStats::default()
        }));

    futures_util::stream::iter(token_ids)
        .for_each_concurrent(max_concurrency, |token_id| {
            let seen_conditions = seen_conditions.clone();
            let stats = stats.clone();
            async move {
                let market = match pm_client.get_gamma_market_by_token_id(&token_id).await {
                    Ok(market) => market,
                    Err(e) => {
                        warn!(
                            token_id = %token_id,
                            error = %e,
                            "failed to fetch gamma market for settlement refresh"
                        );
                        return;
                    }
                };

                let condition_key = market
                    .condition_id
                    .map(|b| b.to_string())
                    .filter(|v| !v.trim().is_empty())
                    .unwrap_or_else(|| format!("market:{}", market.id));

                {
                    let mut seen = seen_conditions.lock().await;
                    if !seen.insert(condition_key) {
                        return;
                    }
                }

                match upsert_pm_token_settlement_rows(pool, &market).await {
                    Ok((upserted_rows, resolved_market)) => {
                        let mut guard = stats.lock().await;
                        guard.refreshed_markets += 1;
                        guard.upserted_rows += upserted_rows;
                        if resolved_market {
                            guard.resolved_markets += 1;
                        }
                        drop(guard);

                        // Backfill pm_market_metadata from settlement raw_market JSONB
                        if let Err(e) =
                            backfill_pm_market_metadata_from_settlement(pool, &market).await
                        {
                            debug!(
                                market_id = %market.id,
                                error = %e,
                                "pm_market_metadata backfill skipped"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            token_id = %token_id,
                            market_id = %market.id,
                            error = %e,
                            "failed to upsert pm settlement rows"
                        );
                    }
                }
            }
        })
        .await;

    let snapshot = { *stats.lock().await };
    Ok(snapshot)
}

async fn upsert_pm_token_settlement_rows(
    pool: &PgPool,
    market: &polymarket_client_sdk::gamma::types::response::Market,
) -> Result<(usize, bool)> {
    let clob_token_ids: Vec<String> = market
        .clob_token_ids
        .as_ref()
        .map(|ids| ids.iter().map(|id| id.to_string()).collect())
        .unwrap_or_default();
    let outcomes: Vec<String> = market.outcomes.clone().unwrap_or_default();
    let outcome_prices: Vec<String> = market
        .outcome_prices
        .as_ref()
        .map(|ps| ps.iter().map(|d| d.to_string()).collect())
        .unwrap_or_default();

    if clob_token_ids.is_empty() || outcome_prices.is_empty() {
        return Ok((0, false));
    }

    let parsed_prices: Vec<rust_decimal::Decimal> = outcome_prices
        .iter()
        .filter_map(|v| v.parse::<rust_decimal::Decimal>().ok())
        .collect();
    let resolved_market = market.closed.unwrap_or(false) && is_market_resolved(&parsed_prices);
    let resolved_at: Option<chrono::DateTime<Utc>> = resolved_market.then(Utc::now);
    let raw_market = serde_json::to_value(market).unwrap_or_else(|_| serde_json::json!({}));

    let mut upserted_rows = 0usize;
    for (idx, token_id) in clob_token_ids.iter().enumerate() {
        let outcome = outcomes.get(idx).cloned();
        let settled_price = outcome_prices
            .get(idx)
            .and_then(|v| v.parse::<rust_decimal::Decimal>().ok());

        sqlx::query(
            r#"
            INSERT INTO pm_token_settlements (
                token_id,
                condition_id,
                market_id,
                market_slug,
                outcome,
                settled_price,
                resolved,
                resolved_at,
                fetched_at,
                raw_market
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,NOW(),$9)
            ON CONFLICT (token_id) DO UPDATE SET
                condition_id = EXCLUDED.condition_id,
                market_id = EXCLUDED.market_id,
                market_slug = EXCLUDED.market_slug,
                outcome = EXCLUDED.outcome,
                settled_price = EXCLUDED.settled_price,
                resolved = EXCLUDED.resolved,
                resolved_at = COALESCE(pm_token_settlements.resolved_at, EXCLUDED.resolved_at),
                fetched_at = NOW(),
                raw_market = EXCLUDED.raw_market
            "#,
        )
        .bind(token_id)
        .bind(market.condition_id.map(|b| b.to_string()))
        .bind(&market.id)
        .bind(market.slug.as_deref())
        .bind(outcome.as_deref())
        .bind(settled_price)
        .bind(resolved_market)
        .bind(resolved_at)
        .bind(sqlx::types::Json(raw_market.clone()))
        .execute(pool)
        .await?;

        upserted_rows += 1;
    }

    Ok((upserted_rows, resolved_market))
}

/// Backfill `pm_market_metadata` from a Gamma market's raw fields.
///
/// Called after every settlement upsert so that training scripts (which JOIN
/// sync_records → pm_market_metadata) can always find the metadata row.
/// Uses ON CONFLICT DO UPDATE to keep the latest values.
async fn backfill_pm_market_metadata_from_settlement(
    pool: &PgPool,
    market: &polymarket_client_sdk::gamma::types::response::Market,
) -> Result<()> {
    let slug = match market.slug.as_deref() {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(()), // No slug — can't populate metadata
    };

    // Extract threshold from group_item_threshold or upper/lower bound midpoint
    let raw_market = serde_json::to_value(market).unwrap_or_else(|_| serde_json::json!({}));

    // Parse start/end times — prefer eventStartTime over startDate (market creation time)
    let end_time: Option<chrono::DateTime<Utc>> = market
        .end_date_iso
        .map(|d| d.and_hms_opt(23, 59, 59).unwrap_or_default())
        .map(|dt| chrono::DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
    let start_time: Option<chrono::DateTime<Utc>> = raw_market
        .get("eventStartTime")
        .or_else(|| raw_market.get("startDate"))
        .or_else(|| raw_market.get("start_date"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());

    // Infer symbol from slug prefix
    let symbol = if slug.starts_with("btc-") {
        Some("BTCUSDT")
    } else if slug.starts_with("eth-") {
        Some("ETHUSDT")
    } else if slug.starts_with("sol-") {
        Some("SOLUSDT")
    } else {
        None
    };

    // Infer horizon from slug pattern (most reliable)
    let horizon = if slug.contains("-5m-") {
        Some("5m")
    } else if slug.contains("-15m-") {
        Some("15m")
    } else if slug.contains("-60m-") {
        Some("60m")
    } else {
        // Fallback to duration
        match (start_time, end_time) {
            (Some(s), Some(e)) => {
                let secs = (e - s).num_seconds();
                if secs <= 360 {
                    Some("5m")
                } else if secs <= 1080 {
                    Some("15m")
                } else {
                    Some("60m")
                }
            }
            _ => None,
        }
    };

    let threshold: Option<rust_decimal::Decimal> = market
        .group_item_threshold
        .as_deref()
        .and_then(|s| s.parse().ok())
        .or_else(|| {
            let upper: Option<f64> = raw_market
                .get("upperBound")
                .or_else(|| raw_market.get("upper_bound"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok());
            let lower: Option<f64> = raw_market
                .get("lowerBound")
                .or_else(|| raw_market.get("lower_bound"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok());
            match (upper, lower) {
                (Some(u), Some(l)) => rust_decimal::Decimal::try_from((u + l) / 2.0).ok(),
                _ => None,
            }
        });

    let price_to_beat = match threshold {
        Some(p) if !p.is_zero() => p,
        _ => {
            // For up/down markets groupItemThreshold is "0" — the real
            // threshold is the Binance spot price at eventStartTime.
            // Try to look it up; fall back to Decimal::ZERO if unavailable.
            match (symbol, start_time) {
                (Some(sym), Some(st)) => {
                    let row = sqlx::query_scalar::<_, rust_decimal::Decimal>(
                        "SELECT price FROM binance_price_ticks WHERE symbol = $1 AND trade_time <= $2 ORDER BY trade_time DESC LIMIT 1"
                    )
                    .bind(sym)
                    .bind(st)
                    .fetch_optional(pool)
                    .await
                    .unwrap_or(None);
                    row.unwrap_or(rust_decimal::Decimal::ZERO)
                }
                _ => rust_decimal::Decimal::ZERO,
            }
        }
    };

    sqlx::query(
        r#"
        INSERT INTO pm_market_metadata (market_slug, price_to_beat, start_time, end_time, horizon, symbol, raw_market, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
        ON CONFLICT (market_slug) DO UPDATE SET
            price_to_beat = EXCLUDED.price_to_beat,
            start_time    = COALESCE(EXCLUDED.start_time, pm_market_metadata.start_time),
            end_time      = COALESCE(EXCLUDED.end_time, pm_market_metadata.end_time),
            horizon       = COALESCE(EXCLUDED.horizon, pm_market_metadata.horizon),
            symbol        = COALESCE(EXCLUDED.symbol, pm_market_metadata.symbol),
            raw_market    = EXCLUDED.raw_market,
            updated_at    = NOW()
        "#,
    )
    .bind(slug)
    .bind(price_to_beat)
    .bind(start_time)
    .bind(end_time)
    .bind(horizon)
    .bind(symbol)
    .bind(sqlx::types::Json(raw_market))
    .execute(pool)
    .await?;

    Ok(())
}

#[allow(dead_code)]
fn parse_json_array_strings_relaxed(
    input: &str,
) -> std::result::Result<Vec<String>, serde_json::Error> {
    let s = input.trim();
    if s.is_empty() || s == "null" {
        return Ok(Vec::new());
    }

    if let Ok(v) = serde_json::from_str::<Vec<String>>(s) {
        return Ok(v);
    }

    let vals = serde_json::from_str::<Vec<serde_json::Value>>(s)?;
    Ok(vals
        .into_iter()
        .map(|v| match v {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        })
        .collect())
}

fn is_market_resolved(prices: &[rust_decimal::Decimal]) -> bool {
    if prices.is_empty() {
        return false;
    }

    let winners = prices
        .iter()
        .filter(|p| **p >= rust_decimal_macros::dec!(0.99))
        .count();
    let losers = prices
        .iter()
        .filter(|p| **p <= rust_decimal_macros::dec!(0.01))
        .count();

    winners == 1 && losers == prices.len().saturating_sub(1)
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

fn env_decimal_opt(name: &str) -> Option<rust_decimal::Decimal> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<rust_decimal::Decimal>().ok())
}

fn deployments_state_path() -> PathBuf {
    if let Ok(path) = std::env::var("PLOY_DEPLOYMENTS_FILE") {
        return PathBuf::from(path);
    }
    let container_data_root = Path::new("/opt/ploy/data");
    if container_data_root.exists() {
        return container_data_root.join("state/deployments.json");
    }
    let repo_state_deployment = Path::new("data/state/deployments.json");
    if repo_state_deployment.exists() {
        return repo_state_deployment.to_path_buf();
    }
    let repo_root_deployment = Path::new("deployment/deployments.json");
    if repo_root_deployment.exists() {
        return repo_root_deployment.to_path_buf();
    }
    let container_deployment = Path::new("/opt/ploy/deployment/deployments.json");
    if container_deployment.exists() {
        return container_deployment.to_path_buf();
    }
    PathBuf::from("data/state/deployments.json")
}

fn parse_strategy_deployments(raw: &str) -> Vec<StrategyDeployment> {
    let mut out = Vec::new();
    match serde_json::from_str::<Vec<StrategyDeployment>>(raw) {
        Ok(items) => {
            for mut dep in items {
                if dep.id.trim().is_empty() {
                    continue;
                }
                dep.normalize_account_ids_in_place();
                out.push(dep);
            }
        }
        Err(e) => {
            warn!(error = %e, "failed to parse strategy deployments JSON");
        }
    }
    out
}

fn load_strategy_deployments() -> Vec<StrategyDeployment> {
    let raw = std::env::var("PLOY_STRATEGY_DEPLOYMENTS_JSON")
        .or_else(|_| std::env::var("PLOY_DEPLOYMENTS_JSON"))
        .unwrap_or_default();
    if !raw.trim().is_empty() {
        return parse_strategy_deployments(&raw);
    }

    let repo_state_path = Path::new("data/state/deployments.json");
    let container_data_path = Path::new("/opt/ploy/data/state/deployments.json");
    let deployment_file_candidates = [
        deployments_state_path(),
        repo_state_path.to_path_buf(),
        container_data_path.to_path_buf(),
        Path::new("deployment/deployments.json").to_path_buf(),
        Path::new("/opt/ploy/deployment/deployments.json").to_path_buf(),
    ];

    for path in deployment_file_candidates {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let items = parse_strategy_deployments(&contents);
            if !items.is_empty() {
                return items;
            }
        }
    }
    Vec::new()
}

fn add_coin_from_text(raw: &str, coins: &mut HashSet<String>) {
    let upper = raw.trim().to_ascii_uppercase();
    if upper.is_empty() {
        return;
    }

    for known in ["BTC", "ETH", "SOL", "XRP"] {
        if upper.contains(known) {
            coins.insert(known.to_string());
        }
    }

    for token in upper.split(|c: char| !c.is_ascii_alphanumeric()) {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        let base = t.strip_suffix("USDT").unwrap_or(t);
        if (2..=8).contains(&base.len()) && base.chars().all(|c| c.is_ascii_alphabetic()) {
            coins.insert(base.to_string());
        }
    }
}

fn add_coins_from_selector(selector: &MarketSelector, coins: &mut HashSet<String>) {
    match selector {
        MarketSelector::Static {
            symbol,
            series_id,
            market_slug,
        } => {
            if let Some(raw) = symbol.as_deref() {
                add_coin_from_text(raw, coins);
            }
            if let Some(raw) = series_id.as_deref() {
                add_coin_from_text(raw, coins);
            }
            if let Some(raw) = market_slug.as_deref() {
                add_coin_from_text(raw, coins);
            }
        }
        MarketSelector::Dynamic { query, .. } => {
            if let Some(raw) = query.as_deref() {
                add_coin_from_text(raw, coins);
            }
        }
    }
}

fn normalize_strategy_key(strategy: &str) -> String {
    strategy.to_ascii_lowercase().replace(['-', '_', ' '], "")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CryptoStrategyKind {
    Momentum,
    PatternMemory,
    SplitArb,
    LobMl,
    #[cfg(feature = "rl")]
    RlPolicy,
    Unknown,
}

fn classify_crypto_strategy(strategy: &str) -> CryptoStrategyKind {
    let key = normalize_strategy_key(strategy);

    if key.contains("momentum")
        || key == "mom"
        || key == "directional"
        || key == "directionalmomentum"
    {
        return CryptoStrategyKind::Momentum;
    }
    if key.contains("pattern") || key.contains("memory") || key.contains("pattenmem") {
        return CryptoStrategyKind::PatternMemory;
    }
    if key.contains("splitarb")
        || (key.contains("split") && key.contains("arb"))
        || key.contains("staggeredarb")
        || key.contains("gammascalping")
    {
        return CryptoStrategyKind::SplitArb;
    }
    if key.contains("lob")
        || key.contains("ml")
        || key.contains("dl")
        || key.contains("deep")
        || key.contains("learning")
    {
        return CryptoStrategyKind::LobMl;
    }
    #[cfg(feature = "rl")]
    if key.contains("rl") || key.contains("policy") {
        return CryptoStrategyKind::RlPolicy;
    }

    CryptoStrategyKind::Unknown
}

fn normalize_horizon(value: &str) -> Option<&'static str> {
    let key = value.to_ascii_lowercase().replace(['-', '_', ' '], "");
    if key == "5m" || key == "5min" || key == "5minute" {
        return Some("5m");
    }
    if key == "15m" || key == "15min" || key == "15minute" {
        return Some("15m");
    }
    None
}

fn crypto_series_id_for(coin: &str, horizon: &str) -> Option<&'static str> {
    let c = coin.to_ascii_uppercase();
    match (c.as_str(), horizon) {
        ("BTC", "5m") => Some("10684"),
        ("ETH", "5m") => Some("10683"),
        ("SOL", "5m") => Some("10686"),
        ("XRP", "5m") => Some("10685"),
        ("BTC", "15m") => Some("10192"),
        ("ETH", "15m") => Some("10191"),
        ("SOL", "15m") => Some("10423"),
        ("XRP", "15m") => Some("10422"),
        _ => None,
    }
}

fn coin_symbol_for(coin: &str) -> Option<String> {
    let c = coin.to_ascii_uppercase();
    if c.is_empty() {
        return None;
    }
    Some(format!("{}USDT", c))
}

fn symbol_for_crypto_series_id(series_id: &str) -> Option<&'static str> {
    match series_id {
        "10684" | "10192" => Some("BTCUSDT"),
        "10683" | "10191" => Some("ETHUSDT"),
        "10686" | "10423" => Some("SOLUSDT"),
        "10685" | "10422" => Some("XRPUSDT"),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct RuntimeCryptoStrategyTargets {
    momentum_coins: HashSet<String>,
    pattern_memory_coins: HashSet<String>,
    split_arb_coins: HashSet<String>,
    split_arb_horizons: HashSet<String>,
}

fn collect_runtime_crypto_strategy_targets(
    runtime_account_id: &str,
    runtime_dry_run: bool,
) -> RuntimeCryptoStrategyTargets {
    let deployments = load_strategy_deployments();
    let mut out = RuntimeCryptoStrategyTargets::default();

    for dep in deployments
        .iter()
        .filter(|d| d.enabled)
        .filter(|d| d.matches_account(runtime_account_id))
        .filter(|d| d.matches_execution_mode(runtime_dry_run))
    {
        if !matches!(dep.domain, Domain::Crypto) {
            continue;
        }

        match classify_crypto_strategy(&dep.strategy) {
            CryptoStrategyKind::Momentum => {
                add_coins_from_selector(&dep.market_selector, &mut out.momentum_coins);
            }
            CryptoStrategyKind::PatternMemory => {
                add_coins_from_selector(&dep.market_selector, &mut out.pattern_memory_coins);
            }
            CryptoStrategyKind::SplitArb => {
                add_coins_from_selector(&dep.market_selector, &mut out.split_arb_coins);
                if let Some(h) = normalize_horizon(dep.timeframe.as_str()) {
                    out.split_arb_horizons.insert(h.to_string());
                }
            }
            _ => {}
        }
    }

    out
}

fn build_pattern_memory_runtime_config(coins: &[String]) -> Result<String> {
    let mut selected: Vec<String> = coins
        .iter()
        .filter_map(|c| {
            c.strip_suffix("USDT")
                .map(|s| s.to_string())
                .or_else(|| Some(c.clone()))
        })
        .map(|c| c.to_ascii_uppercase())
        .collect();
    selected.sort();
    selected.dedup();

    let mut markets_block = String::new();
    for coin in selected {
        if let (Some(symbol), Some(series_id)) =
            (coin_symbol_for(&coin), crypto_series_id_for(&coin, "5m"))
        {
            markets_block.push_str("\n[[markets]]\n");
            markets_block.push_str(&format!("symbol = \"{}\"\n", symbol));
            markets_block.push_str(&format!("series_id = \"{}\"\n", series_id));
        }
    }

    if markets_block.trim().is_empty() {
        return Err(crate::error::PloyError::Validation(
            "pattern_memory runtime has no recognized crypto coins/series ids".to_string(),
        ));
    }

    Ok(format!(
        r#"# Auto-generated by platform bootstrap
[strategy]
name = "pattern_memory"
enabled = true
{markets}
[pattern]
corr_threshold = 0.70
alpha = 1.0
beta = 1.0
min_matches = 3
min_n_eff = 2.0
min_confidence = 0.60

[filter_15m]
enabled = true
min_confidence = 0.55
min_n_eff = 1.0

[timing]
target_remaining_secs = 300
tolerance_secs = 45
min_remaining_secs = 60

[trade]
shares = 100
max_entry_price = 0.55
min_net_ev = 0.0
cooldown_secs = 30
"#,
        markets = markets_block
    ))
}

fn insert_toml_float(table: &mut toml::value::Table, key: &str, value: f64) {
    table.insert(key.to_string(), toml::Value::Float(value));
}

fn insert_toml_int(table: &mut toml::value::Table, key: &str, value: i64) {
    table.insert(key.to_string(), toml::Value::Integer(value));
}

fn insert_toml_bool(table: &mut toml::value::Table, key: &str, value: bool) {
    table.insert(key.to_string(), toml::Value::Boolean(value));
}

fn render_momentum_runtime_config(
    mut config: toml::Value,
    symbols: &[String],
    crypto_cfg: &CryptoTradingConfig,
) -> String {
    let root = config
        .as_table_mut()
        .expect("momentum runtime config must be a table");
    let strategy = root
        .entry("strategy")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[strategy] must be a table");
    insert_toml_bool(strategy, "enabled", true);

    let entry = root
        .entry("entry")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[entry] must be a table");
    entry.insert(
        "symbols".to_string(),
        toml::Value::Array(symbols.iter().cloned().map(toml::Value::String).collect()),
    );
    insert_toml_float(
        entry,
        "min_move",
        (crypto_cfg.min_momentum_1s * 100.0).max(0.0),
    );
    insert_toml_float(
        entry,
        "min_edge",
        (crypto_cfg.min_edge * Decimal::from(100))
            .to_f64()
            .unwrap_or(2.0),
    );
    insert_toml_bool(
        entry,
        "require_mtf_agreement",
        crypto_cfg.require_mtf_agreement,
    );
    insert_toml_bool(
        entry,
        "directional_mode",
        matches!(crypto_cfg.entry_mode, CryptoEntryMode::Directional),
    );
    insert_toml_float(
        entry,
        "directional_entry_threshold",
        (crypto_cfg.min_edge * Decimal::from(100))
            .to_f64()
            .unwrap_or(2.0),
    );

    let timing = root
        .entry("timing")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[timing] must be a table");
    insert_toml_int(
        timing,
        "min_time_remaining",
        crypto_cfg.min_time_remaining_secs as i64,
    );
    insert_toml_int(
        timing,
        "max_time_remaining",
        crypto_cfg.max_time_remaining_secs as i64,
    );
    insert_toml_int(
        timing,
        "cooldown_secs",
        crypto_cfg.entry_cooldown_secs as i64,
    );

    let risk = root
        .entry("risk")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[risk] must be a table");
    insert_toml_int(risk, "shares", crypto_cfg.default_shares as i64);
    insert_toml_int(
        risk,
        "max_positions",
        crypto_cfg.risk_params.max_unhedged_positions as i64,
    );
    insert_toml_float(
        risk,
        "max_window_exposure",
        crypto_cfg
            .risk_params
            .max_total_exposure
            .to_f64()
            .unwrap_or(25.0),
    );

    let exit = root
        .entry("exit")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[exit] must be a table");
    insert_toml_float(
        exit,
        "exit_edge_floor_pct",
        (crypto_cfg.exit_edge_floor * Decimal::from(100))
            .to_f64()
            .unwrap_or(20.0),
    );
    insert_toml_float(
        exit,
        "exit_price_band_pct",
        (crypto_cfg.exit_price_band * Decimal::from(100))
            .to_f64()
            .unwrap_or(12.0),
    );

    format!(
        "# Auto-generated by platform bootstrap — momentum\n{}",
        toml::to_string(&config).expect("runtime config must serialize to TOML")
    )
}

fn load_momentum_config_file(symbols: &[String], crypto_cfg: &CryptoTradingConfig) -> Option<String> {
    let candidates = [
        std::env::var("PLOY_MOMENTUM_CONFIG").ok(),
        Some("config/strategies/momentum.toml".to_string()),
        Some("/root/ploy/config/strategies/momentum.toml".to_string()),
        Some("/opt/ploy/config/strategies/momentum.toml".to_string()),
    ];
    for candidate in candidates.iter().flatten() {
        if let Ok(contents) = std::fs::read_to_string(candidate) {
            if let Ok(val) = toml::from_str::<toml::Value>(&contents) {
                if val.get("strategy").is_some() {
                    info!(path = %candidate, "loaded momentum config from external file");
                    return Some(render_momentum_runtime_config(val, symbols, crypto_cfg));
                }
            }
            warn!(path = %candidate, "momentum config file found but invalid TOML");
        }
    }
    None
}

fn build_momentum_runtime_config(symbols: &[String], crypto_cfg: &CryptoTradingConfig) -> String {
    if let Some(cfg) = load_momentum_config_file(symbols, crypto_cfg) {
        return cfg;
    }

    let config: toml::Value = toml::from_str(include_str!("../../config/strategies/momentum.toml"))
        .expect("embedded momentum runtime config must stay valid TOML");
    render_momentum_runtime_config(config, symbols, crypto_cfg)
}

fn build_event_edge_runtime_config(
    rest_url: &str,
    cfg: &crate::config::EventEdgeAgentConfig,
) -> String {
    let mut root = toml::value::Table::new();

    let mut strategy = toml::value::Table::new();
    strategy.insert("name".to_string(), toml::Value::String("event_edge".to_string()));
    strategy.insert("enabled".to_string(), toml::Value::Boolean(cfg.enabled));
    strategy.insert("trade".to_string(), toml::Value::Boolean(cfg.trade));
    root.insert("strategy".to_string(), toml::Value::Table(strategy));

    let mut events = toml::value::Table::new();
    events.insert(
        "event_ids".to_string(),
        toml::Value::Array(
            cfg.event_ids
                .iter()
                .cloned()
                .map(toml::Value::String)
                .collect(),
        ),
    );
    events.insert(
        "titles".to_string(),
        toml::Value::Array(
            cfg.titles
                .iter()
                .cloned()
                .map(toml::Value::String)
                .collect(),
        ),
    );
    root.insert("events".to_string(), toml::Value::Table(events));

    let mut entry = toml::value::Table::new();
    insert_toml_float(&mut entry, "min_edge", cfg.min_edge.to_f64().unwrap_or(0.08));
    insert_toml_float(&mut entry, "max_entry", cfg.max_entry.to_f64().unwrap_or(0.75));
    insert_toml_int(&mut entry, "shares", cfg.shares as i64);
    root.insert("entry".to_string(), toml::Value::Table(entry));

    let mut timing = toml::value::Table::new();
    insert_toml_int(&mut timing, "poll_interval_secs", cfg.interval_secs as i64);
    root.insert("timing".to_string(), toml::Value::Table(timing));

    let mut risk = toml::value::Table::new();
    insert_toml_int(&mut risk, "cooldown_secs", cfg.cooldown_secs as i64);
    insert_toml_float(
        &mut risk,
        "max_daily_spend_usd",
        cfg.max_daily_spend_usd.to_f64().unwrap_or(100.0),
    );
    root.insert("risk".to_string(), toml::Value::Table(risk));

    let mut polymarket = toml::value::Table::new();
    polymarket.insert(
        "rest_url".to_string(),
        toml::Value::String(rest_url.to_string()),
    );
    root.insert("polymarket".to_string(), toml::Value::Table(polymarket));

    format!(
        "# Auto-generated by platform bootstrap — event edge\n{}",
        toml::to_string(&toml::Value::Table(root))
            .expect("event edge runtime config must serialize to TOML")
    )
}

#[derive(Debug, Clone)]
struct ManagedStrategyBootstrapSpec {
    strategy_label: &'static str,
    agent_id: String,
    domain: Domain,
    strategy_config_toml: String,
}

fn build_momentum_managed_runtime_spec(
    symbols: &[String],
    crypto_cfg: &CryptoTradingConfig,
) -> ManagedStrategyBootstrapSpec {
    ManagedStrategyBootstrapSpec {
        strategy_label: "momentum",
        agent_id: crypto_cfg.agent_id.clone(),
        domain: Domain::Crypto,
        strategy_config_toml: build_momentum_runtime_config(symbols, crypto_cfg),
    }
}

fn build_pattern_memory_managed_runtime_spec(
    coins: &[String],
) -> Result<ManagedStrategyBootstrapSpec> {
    Ok(ManagedStrategyBootstrapSpec {
        strategy_label: "pattern_memory",
        agent_id: "pattern_memory".to_string(),
        domain: Domain::Crypto,
        strategy_config_toml: build_pattern_memory_runtime_config(coins)?,
    })
}

fn build_event_edge_managed_runtime_spec(
    rest_url: &str,
    politics_cfg: &PoliticsTradingConfig,
    ee_cfg: &crate::config::EventEdgeAgentConfig,
) -> ManagedStrategyBootstrapSpec {
    ManagedStrategyBootstrapSpec {
        strategy_label: "event_edge",
        agent_id: politics_cfg.agent_id.clone(),
        domain: Domain::Politics,
        strategy_config_toml: build_event_edge_runtime_config(rest_url, ee_cfg),
    }
}

#[allow(dead_code)]
fn build_nba_comeback_runtime_config(
    database_url: &str,
    cfg: &crate::config::NbaComebackConfig,
) -> String {
    let mut root = toml::value::Table::new();

    let mut strategy = toml::value::Table::new();
    strategy.insert(
        "name".to_string(),
        toml::Value::String("nba_comeback".to_string()),
    );
    strategy.insert("enabled".to_string(), toml::Value::Boolean(cfg.enabled));
    root.insert("strategy".to_string(), toml::Value::Table(strategy));

    let mut entry = toml::value::Table::new();
    insert_toml_float(&mut entry, "min_edge", cfg.min_edge.to_f64().unwrap_or(0.05));
    insert_toml_float(
        &mut entry,
        "max_entry_price",
        cfg.max_entry_price.to_f64().unwrap_or(0.75),
    );
    insert_toml_int(&mut entry, "shares", cfg.shares as i64);
    root.insert("entry".to_string(), toml::Value::Table(entry));

    let mut timing = toml::value::Table::new();
    insert_toml_int(
        &mut timing,
        "poll_interval_secs",
        cfg.espn_poll_interval_secs as i64,
    );
    root.insert("timing".to_string(), toml::Value::Table(timing));

    let mut risk = toml::value::Table::new();
    insert_toml_int(&mut risk, "cooldown_secs", cfg.cooldown_secs as i64);
    insert_toml_float(
        &mut risk,
        "max_daily_spend_usd",
        cfg.max_daily_spend_usd.to_f64().unwrap_or(100.0),
    );
    insert_toml_float(
        &mut risk,
        "min_reward_risk_ratio",
        cfg.min_reward_risk_ratio,
    );
    insert_toml_float(&mut risk, "min_expected_value", cfg.min_expected_value);
    insert_toml_float(&mut risk, "kelly_fraction_cap", cfg.kelly_fraction_cap);
    root.insert("risk".to_string(), toml::Value::Table(risk));

    let mut scan = toml::value::Table::new();
    insert_toml_int(&mut scan, "min_deficit", cfg.min_deficit as i64);
    insert_toml_int(&mut scan, "max_deficit", cfg.max_deficit as i64);
    insert_toml_int(&mut scan, "target_quarter", cfg.target_quarter as i64);
    insert_toml_float(&mut scan, "min_comeback_rate", cfg.min_comeback_rate);
    scan.insert("season".to_string(), toml::Value::String(cfg.season.clone()));
    root.insert("scan".to_string(), toml::Value::Table(scan));

    let mut database = toml::value::Table::new();
    database.insert(
        "url".to_string(),
        toml::Value::String(database_url.to_string()),
    );
    root.insert("database".to_string(), toml::Value::Table(database));

    let mut grok = toml::value::Table::new();
    grok.insert("enabled".to_string(), toml::Value::Boolean(cfg.grok_enabled));
    insert_toml_int(&mut grok, "interval_secs", cfg.grok_interval_secs as i64);
    insert_toml_float(
        &mut grok,
        "min_edge",
        cfg.grok_min_edge.to_f64().unwrap_or(0.08),
    );
    insert_toml_float(&mut grok, "min_confidence", cfg.grok_min_confidence);
    insert_toml_int(
        &mut grok,
        "decision_cooldown_secs",
        cfg.grok_decision_cooldown_secs as i64,
    );
    grok.insert(
        "fallback_enabled".to_string(),
        toml::Value::Boolean(cfg.grok_fallback_enabled),
    );
    root.insert("grok".to_string(), toml::Value::Table(grok));

    let mut performance = toml::value::Table::new();
    insert_toml_float(
        &mut performance,
        "daily_loss_limit_usd",
        cfg.performance_daily_loss_limit_usd.to_f64().unwrap_or(30.0),
    );
    insert_toml_int(
        &mut performance,
        "min_settled_trades",
        cfg.performance_min_settled_trades as i64,
    );
    insert_toml_float(
        &mut performance,
        "min_win_rate",
        cfg.performance_min_win_rate,
    );
    insert_toml_float(
        &mut performance,
        "low_winrate_multiplier",
        cfg.performance_low_winrate_multiplier,
    );
    insert_toml_int(
        &mut performance,
        "loss_streak_threshold",
        cfg.performance_loss_streak_threshold as i64,
    );
    insert_toml_float(
        &mut performance,
        "loss_streak_multiplier",
        cfg.performance_loss_streak_multiplier,
    );
    root.insert("performance".to_string(), toml::Value::Table(performance));

    let mut scaling = toml::value::Table::new();
    scaling.insert(
        "enabled".to_string(),
        toml::Value::Boolean(cfg.scaling_enabled),
    );
    insert_toml_int(&mut scaling, "max_adds", cfg.scaling_max_adds as i64);
    insert_toml_float(
        &mut scaling,
        "min_price_drop_pct",
        cfg.scaling_min_price_drop_pct,
    );
    insert_toml_float(
        &mut scaling,
        "max_game_exposure_usd",
        cfg.scaling_max_game_exposure_usd.to_f64().unwrap_or(50.0),
    );
    insert_toml_float(
        &mut scaling,
        "min_comeback_retention",
        cfg.scaling_min_comeback_retention,
    );
    insert_toml_float(
        &mut scaling,
        "min_time_remaining_mins",
        cfg.scaling_min_time_remaining_mins,
    );
    root.insert("scaling".to_string(), toml::Value::Table(scaling));

    let mut exit = toml::value::Table::new();
    exit.insert(
        "enabled".to_string(),
        toml::Value::Boolean(cfg.early_exit_enabled),
    );
    insert_toml_float(
        &mut exit,
        "take_profit_pct",
        cfg.early_exit_take_profit_pct,
    );
    insert_toml_float(
        &mut exit,
        "stop_loss_pct",
        cfg.early_exit_stop_loss_pct,
    );
    root.insert("exit".to_string(), toml::Value::Table(exit));

    format!(
        "# Auto-generated by platform bootstrap — nba comeback\n{}",
        toml::to_string(&toml::Value::Table(root))
            .expect("nba comeback runtime config must serialize to TOML")
    )
}

fn build_nba_comeback_managed_runtime_spec(
    database_url: &str,
    sports_cfg: &SportsTradingConfig,
    nba_cfg: &crate::config::NbaComebackConfig,
) -> Option<ManagedStrategyBootstrapSpec> {
    if nba_cfg.grok_enabled {
        return None;
    }

    Some(ManagedStrategyBootstrapSpec {
        strategy_label: "nba_comeback",
        agent_id: sports_cfg.agent_id.clone(),
        domain: Domain::Sports,
        strategy_config_toml: build_nba_comeback_runtime_config(database_url, nba_cfg),
    })
}

fn render_split_arb_runtime_config(
    mut config: toml::Value,
    symbols: &[String],
    series_ids: &[String],
) -> String {
    let root = config
        .as_table_mut()
        .expect("staggered_arb runtime config must be a table");
    let strategy = root
        .entry("strategy")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[strategy] must be a table");
    strategy.insert("enabled".to_string(), toml::Value::Boolean(true));

    let entry = root
        .entry("entry")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[entry] must be a table");
    entry.insert(
        "symbols".to_string(),
        toml::Value::Array(symbols.iter().cloned().map(toml::Value::String).collect()),
    );

    let markets = root
        .entry("markets")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[markets] must be a table");
    markets.insert(
        "series_ids".to_string(),
        toml::Value::Array(
            series_ids
                .iter()
                .cloned()
                .map(toml::Value::String)
                .collect(),
        ),
    );

    format!(
        "# Auto-generated by platform bootstrap — staggered arb (時間差套利)\n{}",
        toml::to_string(&config).expect("runtime config must serialize to TOML")
    )
}

/// Try to load staggered_arb config from an external TOML file and render it
/// with deployment-scoped symbols/series overrides.
fn load_split_arb_config_file(symbols: &[String], series_ids: &[String]) -> Option<String> {
    let candidates = [
        std::env::var("PLOY_STAGGERED_ARB_CONFIG").ok(),
        Some("config/strategies/staggered_arb.toml".to_string()),
        Some("/root/ploy/config/strategies/staggered_arb.toml".to_string()),
        Some("/opt/ploy/config/strategies/staggered_arb.toml".to_string()),
    ];
    for candidate in candidates.iter().flatten() {
        if let Ok(contents) = std::fs::read_to_string(candidate) {
            if let Ok(val) = toml::from_str::<toml::Value>(&contents) {
                if val.get("strategy").is_some() {
                    info!(
                        path = %candidate,
                        "loaded staggered_arb config from external file"
                    );
                    return Some(render_split_arb_runtime_config(val, symbols, series_ids));
                }
            }
            warn!(path = %candidate, "staggered_arb config file found but invalid TOML");
        }
    }
    None
}

fn build_split_arb_runtime_config(symbols: &[String], series_ids: &[String]) -> String {
    if let Some(cfg) = load_split_arb_config_file(symbols, series_ids) {
        return cfg;
    }

    let config: toml::Value =
        toml::from_str(include_str!("../../config/strategies/staggered_arb.toml"))
            .expect("embedded staggered_arb runtime config must stay valid TOML");
    render_split_arb_runtime_config(config, symbols, series_ids)
}

fn build_split_arb_managed_runtime_spec(
    symbols: &[String],
    series_ids: &[String],
) -> ManagedStrategyBootstrapSpec {
    ManagedStrategyBootstrapSpec {
        strategy_label: "split_arb",
        agent_id: "split_arb".to_string(),
        domain: Domain::Crypto,
        strategy_config_toml: build_split_arb_runtime_config(symbols, series_ids),
    }
}

fn apply_strategy_deployments(
    cfg: &mut PlatformBootstrapConfig,
    deployments: &[StrategyDeployment],
    runtime_account_id: &str,
    runtime_dry_run: bool,
) {
    if deployments.is_empty() {
        return;
    }

    let runtime_scoped: Vec<&StrategyDeployment> = deployments
        .iter()
        .filter(|d| d.matches_account(runtime_account_id))
        .filter(|d| d.matches_execution_mode(runtime_dry_run))
        .collect();
    let enabled: Vec<&StrategyDeployment> = runtime_scoped
        .iter()
        .copied()
        .filter(|d| d.enabled)
        .collect();

    cfg.enable_crypto = false;
    cfg.enable_crypto_momentum = false;
    cfg.enable_crypto_pattern_memory = false;
    cfg.enable_crypto_split_arb = false;
    cfg.enable_crypto_lob_ml = false;
    #[cfg(feature = "rl")]
    {
        cfg.enable_crypto_rl_policy = false;
    }
    cfg.enable_sports = false;
    cfg.enable_politics = false;
    cfg.enable_economics = false;

    let mut coins: HashSet<String> = HashSet::new();
    let mut timeframe_summary: HashMap<String, usize> = HashMap::new();
    let mut custom_domains: HashSet<String> = HashSet::new();

    for dep in enabled.iter().copied() {
        *timeframe_summary
            .entry(dep.timeframe.as_str().to_string())
            .or_insert(0) += 1;

        match dep.domain {
            Domain::Crypto => {
                let mapped = match classify_crypto_strategy(&dep.strategy) {
                    CryptoStrategyKind::Momentum => {
                        cfg.enable_crypto_momentum = true;
                        true
                    }
                    CryptoStrategyKind::PatternMemory => {
                        cfg.enable_crypto_pattern_memory = true;
                        true
                    }
                    CryptoStrategyKind::SplitArb => {
                        cfg.enable_crypto_split_arb = true;
                        true
                    }
                    CryptoStrategyKind::LobMl => {
                        cfg.enable_crypto_lob_ml = true;
                        true
                    }
                    #[cfg(feature = "rl")]
                    CryptoStrategyKind::RlPolicy => {
                        cfg.enable_crypto_rl_policy = true;
                        true
                    }
                    CryptoStrategyKind::Unknown => {
                        warn!(
                            deployment_id = %dep.id,
                            strategy = %dep.strategy,
                            "unknown crypto strategy in deployment matrix; skipping built-in mapping"
                        );
                        false
                    }
                };

                if mapped {
                    cfg.enable_crypto = true;
                    add_coins_from_selector(&dep.market_selector, &mut coins);
                }
            }
            Domain::Sports => cfg.enable_sports = true,
            Domain::Politics => cfg.enable_politics = true,
            Domain::Economics => cfg.enable_economics = true,
            Domain::Custom(ref custom_domain) => {
                custom_domains.insert(format!("custom:{}", custom_domain));
            }
        }
    }

    if !coins.is_empty() {
        let mut sorted: Vec<String> = coins.into_iter().collect();
        sorted.sort();
        cfg.crypto.coins = sorted.clone();
        cfg.crypto_lob_ml.coins = sorted.clone();
        #[cfg(feature = "rl")]
        {
            cfg.crypto_rl_policy.coins = sorted.clone();
        }
    }

    let mut tf: Vec<String> = timeframe_summary
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect();
    tf.sort();
    if !custom_domains.is_empty() {
        let mut custom: Vec<String> = custom_domains.into_iter().collect();
        custom.sort();
        warn!(
            domains = ?custom,
            "custom deployments detected without built-in runtime agent registration"
        );
    }
    #[cfg(feature = "rl")]
    let crypto_rl_policy_enabled = cfg.enable_crypto_rl_policy;
    #[cfg(not(feature = "rl"))]
    let crypto_rl_policy_enabled = false;

    info!(
        total = deployments.len(),
        scoped = runtime_scoped.len(),
        enabled = enabled.len(),
        runtime_account_id = runtime_account_id,
        runtime_dry_run = runtime_dry_run,
        crypto = cfg.enable_crypto,
        crypto_momentum = cfg.enable_crypto_momentum,
        crypto_pattern_memory = cfg.enable_crypto_pattern_memory,
        crypto_split_arb = cfg.enable_crypto_split_arb,
        crypto_lob_ml = cfg.enable_crypto_lob_ml,
        crypto_rl_policy = crypto_rl_policy_enabled,
        sports = cfg.enable_sports,
        politics = cfg.enable_politics,
        economics = cfg.enable_economics,
        coins = ?cfg.crypto.coins,
        timeframes = ?tf,
        "applied strategy deployment matrix to platform runtime"
    );
}

/// Top-level config for the platform bootstrap
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformBootstrapConfig {
    pub coordinator: CoordinatorConfig,
    pub enable_crypto: bool,
    #[serde(default)]
    pub enable_crypto_momentum: bool,
    #[serde(default)]
    pub enable_crypto_pattern_memory: bool,
    #[serde(default)]
    pub enable_crypto_split_arb: bool,
    #[serde(default)]
    pub enable_crypto_lob_ml: bool,
    #[serde(default)]
    #[cfg(feature = "rl")]
    pub enable_crypto_rl_policy: bool,
    pub enable_sports: bool,
    pub enable_politics: bool,
    #[serde(default)]
    pub enable_economics: bool,
    /// Enable OpenClaw meta-agent (Layer 3 orchestrator)
    #[serde(default)]
    pub enable_openclaw: bool,
    pub dry_run: bool,
    pub crypto: CryptoTradingConfig,
    pub crypto_lob_ml: CryptoLobMlConfig,
    #[serde(default)]
    #[cfg(feature = "rl")]
    pub crypto_rl_policy: CryptoRlPolicyConfig,
    pub sports: SportsTradingConfig,
    pub politics: PoliticsTradingConfig,
    /// OpenClaw meta-agent configuration
    #[serde(default)]
    pub openclaw: OpenClawConfig,
}

impl Default for PlatformBootstrapConfig {
    fn default() -> Self {
        Self {
            coordinator: CoordinatorConfig::default(),
            enable_crypto: true,
            enable_crypto_momentum: true,
            enable_crypto_pattern_memory: false,
            enable_crypto_split_arb: false,
            enable_crypto_lob_ml: false,
            #[cfg(feature = "rl")]
            enable_crypto_rl_policy: false,
            enable_sports: false,
            enable_politics: false,
            enable_economics: false,
            enable_openclaw: false,
            dry_run: true,
            crypto: CryptoTradingConfig::default(),
            crypto_lob_ml: CryptoLobMlConfig::default(),
            #[cfg(feature = "rl")]
            crypto_rl_policy: CryptoRlPolicyConfig::default(),
            sports: SportsTradingConfig::default(),
            politics: PoliticsTradingConfig::default(),
            openclaw: OpenClawConfig::default(),
        }
    }
}

impl PlatformBootstrapConfig {
    /// Re-evaluate deployment matrix against the current runtime account + dry-run mode.
    pub fn reapply_strategy_deployments_for_runtime(&mut self, app: &AppConfig) {
        let strategy_deployments = load_strategy_deployments();
        if strategy_deployments.is_empty() {
            return;
        }

        let runtime_account_id = if app.account.id.trim().is_empty() {
            "default".to_string()
        } else {
            app.account.id.clone()
        };
        apply_strategy_deployments(
            self,
            &strategy_deployments,
            &runtime_account_id,
            self.dry_run,
        );
    }

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
            critical_bypass_exposure: false,
            ..Default::default()
        };
        cfg.coordinator.risk.max_drawdown_limit = env_decimal_opt("PLOY_RISK__MAX_DRAWDOWN_USD")
            .map(|v| v.max(rust_decimal::Decimal::ZERO));
        cfg.coordinator.risk.circuit_breaker_auto_recover = env_bool(
            "PLOY_RISK__CIRCUIT_BREAKER_AUTO_RECOVER",
            cfg.coordinator.risk.circuit_breaker_auto_recover,
        );
        cfg.coordinator.risk.circuit_breaker_cooldown_secs = env_u64(
            "PLOY_RISK__CIRCUIT_BREAKER_COOLDOWN_SECS",
            cfg.coordinator.risk.circuit_breaker_cooldown_secs,
        );

        // Optional domain-level risk splits.
        // Example:
        // - PLOY_RISK__ACCOUNT_RESERVE_PCT=0.15
        // - PLOY_RISK__ACCOUNT_DEPLOYABLE_PCT=0.85
        // - PLOY_RISK__CRYPTO_ALLOCATION_PCT=0.5
        // - PLOY_RISK__SPORTS_ALLOCATION_PCT=0.5
        // - PLOY_RISK__CRYPTO_DAILY_LOSS_LIMIT_USD=45
        // - PLOY_RISK__SPORTS_DAILY_LOSS_LIMIT_USD=45
        let normalize_pct = |v: rust_decimal::Decimal| {
            if v >= rust_decimal::Decimal::ZERO && v <= rust_decimal::Decimal::ONE {
                Some(v)
            } else {
                None
            }
        };

        let crypto_alloc_pct =
            env_decimal_opt("PLOY_RISK__CRYPTO_ALLOCATION_PCT").and_then(normalize_pct);
        let sports_alloc_pct =
            env_decimal_opt("PLOY_RISK__SPORTS_ALLOCATION_PCT").and_then(normalize_pct);
        let politics_alloc_pct =
            env_decimal_opt("PLOY_RISK__POLITICS_ALLOCATION_PCT").and_then(normalize_pct);
        let economics_alloc_pct =
            env_decimal_opt("PLOY_RISK__ECONOMICS_ALLOCATION_PCT").and_then(normalize_pct);

        let account_reserve_pct = env_decimal_opt("PLOY_RISK__ACCOUNT_RESERVE_PCT")
            .and_then(normalize_pct)
            .unwrap_or(rust_decimal::Decimal::ZERO);
        let account_deployable_pct = env_decimal_opt("PLOY_RISK__ACCOUNT_DEPLOYABLE_PCT")
            .and_then(normalize_pct)
            .unwrap_or_else(|| rust_decimal::Decimal::ONE - account_reserve_pct);
        let alloc_base = (cfg.coordinator.risk.max_platform_exposure * account_deployable_pct)
            .max(rust_decimal::Decimal::ZERO);

        cfg.coordinator.risk.crypto_max_exposure =
            env_decimal_opt("PLOY_RISK__CRYPTO_MAX_EXPOSURE_USD")
                .or_else(|| crypto_alloc_pct.map(|p| alloc_base * p));
        cfg.coordinator.risk.sports_max_exposure =
            env_decimal_opt("PLOY_RISK__SPORTS_MAX_EXPOSURE_USD")
                .or_else(|| sports_alloc_pct.map(|p| alloc_base * p));
        cfg.coordinator.risk.politics_max_exposure =
            env_decimal_opt("PLOY_RISK__POLITICS_MAX_EXPOSURE_USD")
                .or_else(|| politics_alloc_pct.map(|p| alloc_base * p));
        cfg.coordinator.risk.economics_max_exposure =
            env_decimal_opt("PLOY_RISK__ECONOMICS_MAX_EXPOSURE_USD")
                .or_else(|| economics_alloc_pct.map(|p| alloc_base * p));

        cfg.coordinator.risk.crypto_daily_loss_limit =
            env_decimal_opt("PLOY_RISK__CRYPTO_DAILY_LOSS_LIMIT_USD");
        cfg.coordinator.risk.sports_daily_loss_limit =
            env_decimal_opt("PLOY_RISK__SPORTS_DAILY_LOSS_LIMIT_USD");
        cfg.coordinator.risk.politics_daily_loss_limit =
            env_decimal_opt("PLOY_RISK__POLITICS_DAILY_LOSS_LIMIT_USD");
        cfg.coordinator.risk.economics_daily_loss_limit =
            env_decimal_opt("PLOY_RISK__ECONOMICS_DAILY_LOSS_LIMIT_USD");

        cfg.coordinator.duplicate_guard_enabled = env_bool(
            "PLOY_COORDINATOR__DUPLICATE_GUARD_ENABLED",
            cfg.coordinator.duplicate_guard_enabled,
        );
        cfg.coordinator.duplicate_guard_window_ms = env_u64(
            "PLOY_COORDINATOR__DUPLICATE_GUARD_WINDOW_MS",
            cfg.coordinator.duplicate_guard_window_ms,
        )
        .max(100);
        if let Ok(raw) = std::env::var("PLOY_COORDINATOR__DUPLICATE_GUARD_SCOPE") {
            let v = raw.trim().to_ascii_lowercase();
            cfg.coordinator.duplicate_guard_scope = match v.as_str() {
                "deployment" | "dep" => DuplicateGuardScope::Deployment,
                "market" | "global" => DuplicateGuardScope::Market,
                _ => cfg.coordinator.duplicate_guard_scope,
            };
        }
        cfg.coordinator.heartbeat_stale_warn_cooldown_secs = env_u64(
            "PLOY_COORDINATOR__HEARTBEAT_STALE_WARN_COOLDOWN_SECS",
            cfg.coordinator.heartbeat_stale_warn_cooldown_secs,
        )
        .max(10);

        cfg.coordinator.crypto_allocator_enabled = env_bool(
            "PLOY_COORDINATOR__CRYPTO_ALLOCATOR_ENABLED",
            cfg.coordinator.crypto_allocator_enabled,
        );
        cfg.coordinator.crypto_allocator_total_cap_usd =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_ALLOCATOR_TOTAL_CAP_USD")
                .or(cfg.coordinator.crypto_allocator_total_cap_usd);

        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_COIN_CAP_BTC_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_coin_cap_btc_pct = v;
        }
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_COIN_CAP_ETH_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_coin_cap_eth_pct = v;
        }
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_COIN_CAP_SOL_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_coin_cap_sol_pct = v;
        }
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_COIN_CAP_XRP_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_coin_cap_xrp_pct = v;
        }
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_COIN_CAP_OTHER_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_coin_cap_other_pct = v;
        }

        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_HORIZON_CAP_5M_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_horizon_cap_5m_pct = v;
        }
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_HORIZON_CAP_15M_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_horizon_cap_15m_pct = v;
        }
        if let Some(v) = env_decimal_opt("PLOY_COORDINATOR__CRYPTO_HORIZON_CAP_OTHER_PCT")
            .and_then(normalize_pct)
        {
            cfg.coordinator.crypto_horizon_cap_other_pct = v;
        }

        cfg.coordinator.sports_allocator_enabled = env_bool(
            "PLOY_COORDINATOR__SPORTS_ALLOCATOR_ENABLED",
            cfg.coordinator.sports_allocator_enabled,
        );
        cfg.coordinator.sports_allocator_total_cap_usd =
            env_decimal_opt("PLOY_COORDINATOR__SPORTS_ALLOCATOR_TOTAL_CAP_USD")
                .or(cfg.coordinator.sports_allocator_total_cap_usd);
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__SPORTS_MARKET_CAP_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.sports_market_cap_pct = v;
        }
        cfg.coordinator.sports_auto_split_by_active_markets = env_bool(
            "PLOY_COORDINATOR__SPORTS_AUTO_SPLIT_BY_ACTIVE_MARKETS",
            cfg.coordinator.sports_auto_split_by_active_markets,
        );

        cfg.coordinator.politics_allocator_enabled = env_bool(
            "PLOY_COORDINATOR__POLITICS_ALLOCATOR_ENABLED",
            cfg.coordinator.politics_allocator_enabled,
        );
        cfg.coordinator.politics_allocator_total_cap_usd =
            env_decimal_opt("PLOY_COORDINATOR__POLITICS_ALLOCATOR_TOTAL_CAP_USD")
                .or(cfg.coordinator.politics_allocator_total_cap_usd);
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__POLITICS_MARKET_CAP_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.politics_market_cap_pct = v;
        }
        cfg.coordinator.politics_auto_split_by_active_markets = env_bool(
            "PLOY_COORDINATOR__POLITICS_AUTO_SPLIT_BY_ACTIVE_MARKETS",
            cfg.coordinator.politics_auto_split_by_active_markets,
        );

        cfg.coordinator.economics_allocator_enabled = env_bool(
            "PLOY_COORDINATOR__ECONOMICS_ALLOCATOR_ENABLED",
            cfg.coordinator.economics_allocator_enabled,
        );
        cfg.coordinator.economics_allocator_total_cap_usd =
            env_decimal_opt("PLOY_COORDINATOR__ECONOMICS_ALLOCATOR_TOTAL_CAP_USD")
                .or(cfg.coordinator.economics_allocator_total_cap_usd);
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__ECONOMICS_MARKET_CAP_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.economics_market_cap_pct = v;
        }
        cfg.coordinator.economics_auto_split_by_active_markets = env_bool(
            "PLOY_COORDINATOR__ECONOMICS_AUTO_SPLIT_BY_ACTIVE_MARKETS",
            cfg.coordinator.economics_auto_split_by_active_markets,
        );

        cfg.coordinator.governance_block_new_intents =
            std::env::var("PLOY_COORDINATOR__GOVERNANCE_BLOCK_NEW_INTENTS")
                .or_else(|_| std::env::var("PLOY_GOVERNANCE__BLOCK_NEW_INTENTS"))
                .ok()
                .map(|raw| match raw.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => true,
                    "0" | "false" | "no" | "off" => false,
                    _ => cfg.coordinator.governance_block_new_intents,
                })
                .unwrap_or(cfg.coordinator.governance_block_new_intents);
        cfg.coordinator.governance_max_intent_notional_usd =
            env_decimal_opt("PLOY_COORDINATOR__GOVERNANCE_MAX_INTENT_NOTIONAL_USD")
                .or_else(|| env_decimal_opt("PLOY_GOVERNANCE__MAX_INTENT_NOTIONAL_USD"))
                .or(cfg.coordinator.governance_max_intent_notional_usd);
        cfg.coordinator.governance_max_total_notional_usd =
            env_decimal_opt("PLOY_COORDINATOR__GOVERNANCE_MAX_TOTAL_NOTIONAL_USD")
                .or_else(|| env_decimal_opt("PLOY_GOVERNANCE__MAX_TOTAL_NOTIONAL_USD"))
                .or(cfg.coordinator.governance_max_total_notional_usd);

        if let Ok(raw) = std::env::var("PLOY_COORDINATOR__GOVERNANCE_BLOCKED_DOMAINS")
            .or_else(|_| std::env::var("PLOY_GOVERNANCE__BLOCKED_DOMAINS"))
        {
            let domains = raw
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.to_ascii_lowercase())
                .collect::<Vec<_>>();
            if !domains.is_empty() {
                cfg.coordinator.governance_blocked_domains = domains;
            }
        }
        // Coordinator-level Kelly sizing (optional; applied when intents carry `signal_fair_value`).
        cfg.coordinator.kelly_sizing_enabled = env_bool(
            "PLOY_COORDINATOR__KELLY_SIZING_ENABLED",
            cfg.coordinator.kelly_sizing_enabled,
        );
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__KELLY_FRACTION_MULTIPLIER").and_then(normalize_pct)
        {
            cfg.coordinator.kelly_fraction_multiplier = v;
        }
        if let Some(v) = env_decimal_opt("PLOY_COORDINATOR__KELLY_MIN_EDGE") {
            cfg.coordinator.kelly_min_edge = v
                .max(rust_decimal::Decimal::ZERO)
                .min(rust_decimal::Decimal::ONE);
        }
        cfg.coordinator.kelly_min_shares = env_u64(
            "PLOY_COORDINATOR__KELLY_MIN_SHARES",
            cfg.coordinator.kelly_min_shares,
        );

        // Execution venue minimums (used to prevent deterministic 400s that would otherwise
        // trip the circuit breaker and make the system look like it "stops after one loop").
        cfg.coordinator.min_order_shares = env_u64(
            "PLOY_COORDINATOR__MIN_ORDER_SHARES",
            cfg.coordinator.min_order_shares,
        );
        if let Some(v) = env_decimal_opt("PLOY_COORDINATOR__MIN_ORDER_NOTIONAL_USD") {
            cfg.coordinator.min_order_notional_usd = v.max(rust_decimal::Decimal::ZERO);
        }
        // Map legacy [strategy]/[risk] values into crypto-agent defaults so
        // platform mode follows deployed config instead of hardcoded defaults.
        cfg.crypto.default_shares = app.strategy.shares.max(1);
        let effective_threshold = app.strategy.effective_sum_target();
        if effective_threshold > rust_decimal::Decimal::ZERO {
            cfg.crypto.sum_threshold = effective_threshold;
        } else if app.strategy.sum_target > rust_decimal::Decimal::ZERO {
            cfg.crypto.sum_threshold = app.strategy.sum_target;
        }
        cfg.crypto.exit_edge_floor = app.strategy.profit_buffer.max(rust_decimal::Decimal::ZERO);
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
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__ENABLED") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.enable_crypto_momentum = true,
                "0" | "false" | "no" | "off" => cfg.enable_crypto_momentum = false,
                _ => {}
            }
        }
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
        cfg.crypto.min_window_move_pct = env_decimal(
            "PLOY_CRYPTO_AGENT__MIN_WINDOW_MOVE_PCT",
            cfg.crypto.min_window_move_pct,
        );
        cfg.crypto.min_edge = env_decimal("PLOY_CRYPTO_AGENT__MIN_EDGE", cfg.crypto.min_edge);
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
        cfg.crypto.entry_cooldown_secs = env_u64(
            "PLOY_CRYPTO_AGENT__ENTRY_COOLDOWN_SECS",
            cfg.crypto.entry_cooldown_secs,
        );
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__REQUIRE_MTF_AGREEMENT") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto.require_mtf_agreement = true,
                "0" | "false" | "no" | "off" => cfg.crypto.require_mtf_agreement = false,
                _ => {}
            }
        }
        cfg.crypto.exit_edge_floor = env_decimal(
            "PLOY_CRYPTO_AGENT__EXIT_EDGE_FLOOR",
            cfg.crypto.exit_edge_floor,
        );
        cfg.crypto.exit_price_band = env_decimal(
            "PLOY_CRYPTO_AGENT__EXIT_PRICE_BAND",
            cfg.crypto.exit_price_band,
        );
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__ENABLE_PRICE_EXITS") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto.enable_price_exits = true,
                "0" | "false" | "no" | "off" => cfg.crypto.enable_price_exits = false,
                _ => {}
            }
        }
        cfg.crypto.min_hold_secs =
            env_u64("PLOY_CRYPTO_AGENT__MIN_HOLD_SECS", cfg.crypto.min_hold_secs);
        // Entry mode: arb_only | directional | vol_straddle
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__ENTRY_MODE") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "arb_only" | "arb" => {
                    cfg.crypto.entry_mode = crate::agents::crypto::CryptoEntryMode::ArbOnly
                }
                "directional" | "dir" => {
                    cfg.crypto.entry_mode = crate::agents::crypto::CryptoEntryMode::Directional
                }
                "vol_straddle" | "straddle" => {
                    cfg.crypto.entry_mode = crate::agents::crypto::CryptoEntryMode::VolStraddle
                }
                _ => {}
            }
        }
        cfg.crypto.oracle_lag_buffer_secs = env_u64(
            "PLOY_CRYPTO_AGENT__ORACLE_LAG_BUFFER_SECS",
            cfg.crypto.oracle_lag_buffer_secs,
        );
        cfg.crypto.max_spread_pct = env_decimal(
            "PLOY_CRYPTO_AGENT__MAX_SPREAD_PCT",
            cfg.crypto.max_spread_pct,
        );
        cfg.crypto.straddle_threshold = env_decimal(
            "PLOY_CRYPTO_AGENT__STRADDLE_THRESHOLD",
            cfg.crypto.straddle_threshold,
        );
        cfg.crypto.straddle_min_vol = env_decimal(
            "PLOY_CRYPTO_AGENT__STRADDLE_MIN_VOL",
            cfg.crypto.straddle_min_vol,
        );
        cfg.crypto.min_signal_score = env_decimal(
            "PLOY_CRYPTO_AGENT__MIN_SIGNAL_SCORE",
            cfg.crypto.min_signal_score,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::ONE);
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
        cfg.crypto_lob_ml.exit_edge_floor = cfg.crypto.exit_edge_floor;
        cfg.crypto_lob_ml.exit_price_band = cfg.crypto.exit_price_band;
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

        cfg.crypto_lob_ml.default_shares = env_u64(
            "PLOY_CRYPTO_LOB_ML__DEFAULT_SHARES",
            cfg.crypto_lob_ml.default_shares,
        )
        .max(1);
        cfg.crypto_lob_ml.exit_edge_floor = env_decimal(
            "PLOY_CRYPTO_LOB_ML__EXIT_EDGE_FLOOR",
            cfg.crypto_lob_ml.exit_edge_floor,
        );
        cfg.crypto_lob_ml.exit_price_band = env_decimal(
            "PLOY_CRYPTO_LOB_ML__EXIT_PRICE_BAND",
            cfg.crypto_lob_ml.exit_price_band,
        );
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__EXIT_MODE") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "settle_only" | "settle" => {
                    cfg.crypto_lob_ml.exit_mode = CryptoLobMlExitMode::SettleOnly
                }
                "ev_exit" | "ev" | "model_ev" => {
                    cfg.crypto_lob_ml.exit_mode = CryptoLobMlExitMode::EvExit
                }
                "signal_flip" | "flip" => {
                    cfg.crypto_lob_ml.exit_mode = CryptoLobMlExitMode::SignalFlip
                }
                "trailing_exit" | "trailing" | "price_exit" | "price" | "mtm" => {
                    cfg.crypto_lob_ml.exit_mode = CryptoLobMlExitMode::TrailingExit
                }
                _ => {
                    warn!(
                        value = %raw,
                        "invalid PLOY_CRYPTO_LOB_ML__EXIT_MODE; keeping configured/default value"
                    );
                }
            }
        }
        if std::env::var_os("PLOY_CRYPTO_LOB_ML__ENABLE_PRICE_EXITS").is_some() {
            warn!(
                "PLOY_CRYPTO_LOB_ML__ENABLE_PRICE_EXITS is deprecated and ignored; use PLOY_CRYPTO_LOB_ML__EXIT_MODE"
            );
        }
        cfg.crypto_lob_ml.min_hold_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__MIN_HOLD_SECS",
            cfg.crypto_lob_ml.min_hold_secs,
        );
        cfg.crypto_lob_ml.min_edge =
            env_decimal("PLOY_CRYPTO_LOB_ML__MIN_EDGE", cfg.crypto_lob_ml.min_edge);
        cfg.crypto_lob_ml.max_entry_price = env_decimal(
            "PLOY_CRYPTO_LOB_ML__MAX_ENTRY_PRICE",
            cfg.crypto_lob_ml.max_entry_price,
        );
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__ENTRY_SIDE_POLICY") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "best_ev" | "best" => {
                    cfg.crypto_lob_ml.entry_side_policy = CryptoLobMlEntrySidePolicy::BestEv
                }
                "lagging_only" | "lagging" => {
                    cfg.crypto_lob_ml.entry_side_policy = CryptoLobMlEntrySidePolicy::LaggingOnly
                }
                _ => {}
            }
        }
        cfg.crypto_lob_ml.entry_late_window_secs_5m = env_u64(
            "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M",
            cfg.crypto_lob_ml.entry_late_window_secs_5m,
        )
        .min(300);
        if std::env::var_os("PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M").is_none()
            && std::env::var_os("PLOY_CRYPTO_LOB_ML__ENTRY_EARLY_WINDOW_SECS_5M").is_some()
        {
            warn!(
                "PLOY_CRYPTO_LOB_ML__ENTRY_EARLY_WINDOW_SECS_5M is deprecated; use PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M"
            );
            cfg.crypto_lob_ml.entry_late_window_secs_5m = env_u64(
                "PLOY_CRYPTO_LOB_ML__ENTRY_EARLY_WINDOW_SECS_5M",
                cfg.crypto_lob_ml.entry_late_window_secs_5m,
            )
            .min(300);
        }
        cfg.crypto_lob_ml.entry_late_window_secs_15m = env_u64(
            "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_15M",
            cfg.crypto_lob_ml.entry_late_window_secs_15m,
        )
        .min(900);
        cfg.crypto_lob_ml.taker_fee_rate = env_decimal(
            "PLOY_CRYPTO_LOB_ML__TAKER_FEE_RATE",
            cfg.crypto_lob_ml.taker_fee_rate,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::new(25, 2));
        cfg.crypto_lob_ml.entry_slippage_bps = env_decimal(
            "PLOY_CRYPTO_LOB_ML__ENTRY_SLIPPAGE_BPS",
            cfg.crypto_lob_ml.entry_slippage_bps,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::new(2500, 0));
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__USE_PRICE_TO_BEAT") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto_lob_ml.use_price_to_beat = true,
                "0" | "false" | "no" | "off" => cfg.crypto_lob_ml.use_price_to_beat = false,
                _ => {}
            }
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__REQUIRE_PRICE_TO_BEAT") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto_lob_ml.require_price_to_beat = true,
                "0" | "false" | "no" | "off" => cfg.crypto_lob_ml.require_price_to_beat = false,
                _ => {}
            }
        }
        cfg.crypto_lob_ml.model_blend_weight = env_decimal(
            "PLOY_CRYPTO_LOB_ML__MODEL_BLEND_WEIGHT",
            cfg.crypto_lob_ml.model_blend_weight,
        )
        .max(rust_decimal::Decimal::new(1, 2))
        .min(rust_decimal::Decimal::new(99, 2));
        cfg.crypto_lob_ml.min_direction_strength = env_decimal(
            "PLOY_CRYPTO_LOB_ML__MIN_DIRECTION_STRENGTH",
            cfg.crypto_lob_ml.min_direction_strength,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::new(49, 2));
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
        cfg.crypto_lob_ml.max_time_remaining_secs_5m = env_u64(
            "PLOY_CRYPTO_LOB_ML__MAX_TIME_REMAINING_SECS_5M",
            cfg.crypto_lob_ml.max_time_remaining_secs_5m,
        )
        .max(1);
        cfg.crypto_lob_ml.max_time_remaining_secs_15m = env_u64(
            "PLOY_CRYPTO_LOB_ML__MAX_TIME_REMAINING_SECS_15M",
            cfg.crypto_lob_ml.max_time_remaining_secs_15m,
        )
        .max(1);
        if cfg.crypto_lob_ml.max_time_remaining_secs < cfg.crypto_lob_ml.min_time_remaining_secs {
            cfg.crypto_lob_ml.max_time_remaining_secs = cfg.crypto_lob_ml.min_time_remaining_secs;
        }
        if cfg.crypto_lob_ml.max_time_remaining_secs_5m < cfg.crypto_lob_ml.min_time_remaining_secs
        {
            cfg.crypto_lob_ml.max_time_remaining_secs_5m =
                cfg.crypto_lob_ml.min_time_remaining_secs;
        }
        if cfg.crypto_lob_ml.max_time_remaining_secs_15m < cfg.crypto_lob_ml.min_time_remaining_secs
        {
            cfg.crypto_lob_ml.max_time_remaining_secs_15m =
                cfg.crypto_lob_ml.min_time_remaining_secs;
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
        cfg.crypto_lob_ml.heartbeat_interval_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__HEARTBEAT_INTERVAL_SECS",
            cfg.crypto_lob_ml.heartbeat_interval_secs,
        )
        .max(1);
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_TYPE") {
            let v = raw.trim().to_ascii_lowercase();
            if !v.is_empty() {
                cfg.crypto_lob_ml.model_type = v;
            }
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_PATH") {
            let v = raw.trim();
            cfg.crypto_lob_ml.model_path = if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            };
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_VERSION") {
            let v = raw.trim();
            cfg.crypto_lob_ml.model_version = if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            };
        }
        // window_fallback_weight env var kept for backward compat but is unused
        // in the 2-layer blend model. Ignore silently.
        cfg.crypto_lob_ml.ev_exit_buffer = env_decimal(
            "PLOY_CRYPTO_LOB_ML__EV_EXIT_BUFFER",
            cfg.crypto_lob_ml.ev_exit_buffer,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::new(50, 2));
        cfg.crypto_lob_ml.ev_exit_vol_scale = env_decimal(
            "PLOY_CRYPTO_LOB_ML__EV_EXIT_VOL_SCALE",
            cfg.crypto_lob_ml.ev_exit_vol_scale,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::new(50, 2));
        cfg.crypto_lob_ml.oracle_lag_buffer_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__ORACLE_LAG_BUFFER_SECS",
            cfg.crypto_lob_ml.oracle_lag_buffer_secs,
        );
        cfg.crypto_lob_ml.max_spread_pct = env_decimal(
            "PLOY_CRYPTO_LOB_ML__MAX_SPREAD_PCT",
            cfg.crypto_lob_ml.max_spread_pct,
        );
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__FORCE_SETTLE_ONLY_5M") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto_lob_ml.force_settle_only_5m = true,
                "0" | "false" | "no" | "off" => cfg.crypto_lob_ml.force_settle_only_5m = false,
                _ => {}
            }
        }

        #[cfg(feature = "rl")]
        {
            // Optional RL policy crypto agent (disabled by default).
            // Default to the same risk envelope as the momentum agent unless overridden.
            cfg.crypto_rl_policy.default_shares = cfg.crypto.default_shares;
            cfg.crypto_rl_policy.risk_params = cfg.crypto.risk_params.clone();
            cfg.crypto_rl_policy.heartbeat_interval_secs = cfg.crypto.heartbeat_interval_secs;

            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__ENABLED") {
                match raw.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => cfg.enable_crypto_rl_policy = true,
                    "0" | "false" | "no" | "off" => cfg.enable_crypto_rl_policy = false,
                    _ => {}
                }
            }

            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__COINS") {
                let coins: Vec<String> = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_ascii_uppercase())
                    .collect();
                if !coins.is_empty() {
                    cfg.crypto_rl_policy.coins = coins;
                }
            }

            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__MODEL_PATH") {
                let v = raw.trim();
                if !v.is_empty() {
                    cfg.crypto_rl_policy.policy_model_path = Some(v.to_string());
                }
            }
            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__POLICY_OUTPUT") {
                let v = raw.trim().to_ascii_lowercase();
                if !v.is_empty() {
                    cfg.crypto_rl_policy.policy_output = v;
                }
            }
            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__MODEL_VERSION") {
                let v = raw.trim();
                if !v.is_empty() {
                    cfg.crypto_rl_policy.policy_model_version = Some(v.to_string());
                }
            }

            cfg.crypto_rl_policy.default_shares = env_u64(
                "PLOY_CRYPTO_RL_POLICY__DEFAULT_SHARES",
                cfg.crypto_rl_policy.default_shares,
            )
            .max(1);
            cfg.crypto_rl_policy.max_entry_price = env_decimal(
                "PLOY_CRYPTO_RL_POLICY__MAX_ENTRY_PRICE",
                cfg.crypto_rl_policy.max_entry_price,
            );
            cfg.crypto_rl_policy.cooldown_secs = env_u64(
                "PLOY_CRYPTO_RL_POLICY__COOLDOWN_SECS",
                cfg.crypto_rl_policy.cooldown_secs,
            );
            cfg.crypto_rl_policy.max_lob_snapshot_age_secs = env_u64(
                "PLOY_CRYPTO_RL_POLICY__MAX_LOB_SNAPSHOT_AGE_SECS",
                cfg.crypto_rl_policy.max_lob_snapshot_age_secs,
            )
            .max(1);
            cfg.crypto_rl_policy.decision_interval_ms = env_u64(
                "PLOY_CRYPTO_RL_POLICY__DECISION_INTERVAL_MS",
                cfg.crypto_rl_policy.decision_interval_ms,
            )
            .max(50);
            cfg.crypto_rl_policy.observation_version = env_u64(
                "PLOY_CRYPTO_RL_POLICY__OBS_VERSION",
                cfg.crypto_rl_policy.observation_version as u64,
            ) as u32;
            cfg.crypto_rl_policy.event_refresh_secs = env_u64(
                "PLOY_CRYPTO_RL_POLICY__EVENT_REFRESH_SECS",
                cfg.crypto_rl_policy.event_refresh_secs,
            )
            .max(1);
            cfg.crypto_rl_policy.min_time_remaining_secs = env_u64(
                "PLOY_CRYPTO_RL_POLICY__MIN_TIME_REMAINING_SECS",
                cfg.crypto_rl_policy.min_time_remaining_secs,
            );
            cfg.crypto_rl_policy.max_time_remaining_secs = env_u64(
                "PLOY_CRYPTO_RL_POLICY__MAX_TIME_REMAINING_SECS",
                cfg.crypto_rl_policy.max_time_remaining_secs,
            );
            if cfg.crypto_rl_policy.max_time_remaining_secs
                < cfg.crypto_rl_policy.min_time_remaining_secs
            {
                cfg.crypto_rl_policy.max_time_remaining_secs =
                    cfg.crypto_rl_policy.min_time_remaining_secs;
            }
            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__PREFER_CLOSE_TO_END") {
                match raw.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => cfg.crypto_rl_policy.prefer_close_to_end = true,
                    "0" | "false" | "no" | "off" => {
                        cfg.crypto_rl_policy.prefer_close_to_end = false
                    }
                    _ => {}
                }
            }
            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__EXPLORATION_RATE") {
                if let Ok(v) = raw.trim().parse::<f32>() {
                    if v.is_finite() {
                        cfg.crypto_rl_policy.exploration_rate = v.clamp(0.0, 1.0);
                    }
                }
            }
            cfg.crypto_rl_policy.heartbeat_interval_secs = env_u64(
                "PLOY_CRYPTO_RL_POLICY__HEARTBEAT_INTERVAL_SECS",
                cfg.crypto_rl_policy.heartbeat_interval_secs,
            )
            .max(1);
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

        cfg.reapply_strategy_deployments_for_runtime(app);

        // OpenClaw-first runtime lockdown:
        // keep coordinator available, but disable built-in agent loops.
        if app.openclaw_runtime_lockdown() {
            cfg.enable_crypto = false;
            cfg.enable_crypto_momentum = false;
            cfg.enable_crypto_pattern_memory = false;
            cfg.enable_crypto_split_arb = false;
            cfg.enable_crypto_lob_ml = false;
            #[cfg(feature = "rl")]
            {
                cfg.enable_crypto_rl_policy = false;
            }
            cfg.enable_sports = false;
            cfg.enable_politics = false;
            cfg.enable_economics = false;
            info!("agent framework lockdown active (mode=openclaw): built-in agents are disabled");
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

pub(crate) fn split_arb_status_key(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::Pending => "pending",
        OrderStatus::Submitted => "submitted",
        OrderStatus::PartiallyFilled => "partially_filled",
        OrderStatus::Filled => "filled",
        OrderStatus::Cancelled => "cancelled",
        OrderStatus::Rejected => "rejected",
        OrderStatus::Expired => "expired",
        OrderStatus::Failed => "failed",
    }
}

pub(crate) fn split_arb_leg_and_mode(client_order_id: &str) -> (&'static str, &'static str) {
    if client_order_id.starts_with("stag_leg1_") {
        ("leg1", "entry")
    } else if client_order_id.starts_with("stag_leg2_merge_") {
        ("leg2", "merge")
    } else if client_order_id.starts_with("stag_leg2_forced_") {
        ("leg2", "forced")
    } else if client_order_id.starts_with("stag_leg2_") {
        ("leg2", "unknown")
    } else {
        ("unknown", "unknown")
    }
}

pub(crate) fn split_arb_event_signal_type(event: &crate::strategy::StrategyEvent) -> String {
    match &event.event_type {
        crate::strategy::StrategyEventType::SignalDetected => {
            "split_arb_signal_detected".to_string()
        }
        crate::strategy::StrategyEventType::EntryTriggered => {
            "split_arb_entry_triggered".to_string()
        }
        crate::strategy::StrategyEventType::ExitTriggered => "split_arb_exit_triggered".to_string(),
        crate::strategy::StrategyEventType::OrderFilled => "split_arb_order_filled".to_string(),
        crate::strategy::StrategyEventType::CycleCompleted => {
            "split_arb_cycle_completed".to_string()
        }
        crate::strategy::StrategyEventType::RiskTriggered => "split_arb_risk_triggered".to_string(),
        crate::strategy::StrategyEventType::StateChanged => "split_arb_state_changed".to_string(),
        crate::strategy::StrategyEventType::Error => "split_arb_error".to_string(),
        crate::strategy::StrategyEventType::Custom(name) => {
            let sanitized: String = name
                .trim()
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("split_arb_custom_{}", sanitized)
        }
    }
}

pub(crate) async fn persist_split_arb_signal_history(
    pool: &PgPool,
    account_id: &str,
    strategy_id: &str,
    signal_type: &str,
    token_id: Option<&str>,
    side: Option<&str>,
    fair_value: Option<Decimal>,
    market_price: Option<Decimal>,
    edge: Option<Decimal>,
    context: serde_json::Value,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO signal_history (
            account_id, intent_id, agent_id, strategy_id, domain, signal_type,
            market_slug, token_id, symbol, side, confidence, fair_value, market_price, edge, config_hash, context
        )
        VALUES (
            $1, NULL, 'split_arb', $2, 'crypto', $3,
            NULL, $4, NULL, $5, NULL, $6, $7, $8, NULL, $9
        )
        "#,
    )
    .bind(account_id)
    .bind(strategy_id)
    .bind(signal_type)
    .bind(token_id)
    .bind(side)
    .bind(fair_value)
    .bind(market_price)
    .bind(edge)
    .bind(sqlx::types::Json(context))
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(
            strategy = "split_arb",
            strategy_id = strategy_id,
            signal_type = signal_type,
            error = %e,
            "failed to persist managed split_arb signal_history observation"
        );
    }
}

pub(crate) async fn persist_live_order_signal_history(
    pool: &PgPool,
    account_id: &str,
    strategy_label: &str,
    strategy_id: &str,
    signal_type: &str,
    token_id: Option<&str>,
    side: Option<&str>,
    order_price: Option<Decimal>,
    fill_price: Option<Decimal>,
    context: serde_json::Value,
) {
    let agent_id = format!("{}_runtime", strategy_label);
    let result = sqlx::query(
        r#"
        INSERT INTO signal_history (
            account_id, intent_id, agent_id, strategy_id, domain, signal_type,
            market_slug, token_id, symbol, side, confidence, fair_value, market_price, edge, config_hash, context
        )
        VALUES (
            $1, NULL, $2, $3, 'strategy_runtime', $4,
            NULL, $5, NULL, $6, NULL, $7, $8, NULL, NULL, $9
        )
        "#,
    )
    .bind(account_id)
    .bind(agent_id)
    .bind(strategy_id)
    .bind(signal_type)
    .bind(token_id)
    .bind(side)
    .bind(order_price)
    .bind(fill_price)
    .bind(sqlx::types::Json(context))
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(
            strategy = strategy_label,
            strategy_id = strategy_id,
            signal_type = signal_type,
            error = %e,
            "failed to persist live order signal_history observation"
        );
    }
}

#[allow(dead_code)]
async fn handle_strategy_actions_runtime(
    strategy_label: &str,
    manager: Arc<StrategyManager>,
    mut rx: mpsc::Receiver<(String, StrategyAction)>,
    executor: Arc<OrderExecutor>,
    paused: Arc<AtomicBool>,
    orders_submitted: Arc<AtomicU64>,
    orders_filled: Arc<AtomicU64>,
    observability_pool: Option<PgPool>,
    observability_account_id: String,
) {
    while let Some((strategy_id, action)) = rx.recv().await {
        let split_arb_managed = strategy_label == "split_arb";
        match action {
            StrategyAction::SubmitOrder {
                client_order_id,
                order,
                priority: _,
            } => {
                if paused.load(Ordering::Relaxed) {
                    warn!(
                        strategy = strategy_label,
                        strategy_id = %strategy_id,
                        "strategy submit-order rejected while paused"
                    );
                    let update = crate::strategy::OrderUpdate {
                        order_id: client_order_id.clone(),
                        client_order_id: Some(client_order_id.clone()),
                        status: OrderStatus::Rejected,
                        filled_qty: 0,
                        avg_fill_price: None,
                        timestamp: Utc::now(),
                        error: Some("strategy paused by coordinator".to_string()),
                    };
                    manager.send_order_update(update.clone());
                    if let Some(pool) = observability_pool.as_ref() {
                        let context = json!({
                            "source": "managed_runtime",
                            "phase": "submit_paused",
                            "order_id": update.order_id,
                            "client_order_id": client_order_id.clone(),
                            "status": format!("{:?}", update.status),
                            "filled_qty": update.filled_qty,
                            "avg_fill_price": update.avg_fill_price.map(|p| p.to_string()),
                            "error": update.error,
                        });
                        persist_live_order_signal_history(
                            pool,
                            &observability_account_id,
                            strategy_label,
                            &strategy_id,
                            "live_order_rejected",
                            Some(order.token_id.as_str()),
                            Some(order.market_side.as_str()),
                            Some(order.limit_price),
                            update.avg_fill_price,
                            context,
                        )
                        .await;
                    }
                    if split_arb_managed {
                        if let Some(pool) = observability_pool.as_ref() {
                            let (leg, mode) = split_arb_leg_and_mode(&client_order_id);
                            let signal_type = format!("split_arb_{}_{}_rejected", leg, mode);
                            let context = json!({
                                "source": "managed_runtime",
                                "phase": "submit_paused",
                                "order_id": update.order_id,
                                "client_order_id": client_order_id,
                                "status": format!("{:?}", update.status),
                                "filled_qty": update.filled_qty,
                                "error": update.error,
                                "leg": leg,
                                "mode": mode,
                            });
                            persist_split_arb_signal_history(
                                pool,
                                &observability_account_id,
                                &strategy_id,
                                &signal_type,
                                Some(order.token_id.as_str()),
                                Some(order.market_side.as_str()),
                                Some(order.limit_price),
                                update.avg_fill_price,
                                None,
                                context,
                            )
                            .await;
                        }
                    }
                    continue;
                }

                orders_submitted.fetch_add(1, Ordering::Relaxed);
                match executor.execute(&order).await {
                    Ok(result) => {
                        if matches!(result.status, OrderStatus::Filled) {
                            orders_filled.fetch_add(1, Ordering::Relaxed);
                        }
                        let update = crate::strategy::OrderUpdate {
                            order_id: result.order_id,
                            client_order_id: Some(client_order_id.clone()),
                            status: result.status,
                            filled_qty: result.filled_shares,
                            avg_fill_price: result.avg_fill_price,
                            timestamp: Utc::now(),
                            error: None,
                        };
                        manager.send_order_update(update.clone());
                        if let Some(pool) = observability_pool.as_ref() {
                            let context = json!({
                                "source": "managed_runtime",
                                "phase": "submit_result",
                                "order_id": update.order_id,
                                "client_order_id": client_order_id.clone(),
                                "status": format!("{:?}", update.status),
                                "filled_qty": update.filled_qty,
                                "avg_fill_price": update.avg_fill_price.map(|p| p.to_string()),
                            });
                            persist_live_order_signal_history(
                                pool,
                                &observability_account_id,
                                strategy_label,
                                &strategy_id,
                                "live_order_submit_result",
                                Some(order.token_id.as_str()),
                                Some(order.market_side.as_str()),
                                Some(order.limit_price),
                                update.avg_fill_price,
                                context,
                            )
                            .await;
                        }

                        if split_arb_managed {
                            if let Some(pool) = observability_pool.as_ref() {
                                let (leg, mode) = split_arb_leg_and_mode(&client_order_id);
                                let status_key = split_arb_status_key(update.status);
                                let signal_type =
                                    format!("split_arb_{}_{}_{}", leg, mode, status_key);
                                let context = json!({
                                    "source": "managed_runtime",
                                    "phase": "submit_result",
                                    "order_id": update.order_id,
                                    "client_order_id": client_order_id.clone(),
                                    "status": format!("{:?}", update.status),
                                    "filled_qty": update.filled_qty,
                                    "avg_fill_price": update.avg_fill_price.map(|p| p.to_string()),
                                    "leg": leg,
                                    "mode": mode,
                                });
                                persist_split_arb_signal_history(
                                    pool,
                                    &observability_account_id,
                                    &strategy_id,
                                    &signal_type,
                                    Some(order.token_id.as_str()),
                                    Some(order.market_side.as_str()),
                                    Some(order.limit_price),
                                    update.avg_fill_price,
                                    None,
                                    context,
                                )
                                .await;
                            }
                        }

                        if split_arb_managed
                            && matches!(
                                update.status,
                                OrderStatus::Pending
                                    | OrderStatus::Submitted
                                    | OrderStatus::PartiallyFilled
                            )
                        {
                            let manager_for_poll = manager.clone();
                            let executor_for_poll = executor.clone();
                            let orders_filled_for_poll = orders_filled.clone();
                            let observability_pool_for_poll = observability_pool.clone();
                            let observability_account_for_poll = observability_account_id.clone();
                            let strategy_id_for_poll = strategy_id.clone();
                            let client_order_id_for_poll = client_order_id.clone();
                            let exchange_order_id_for_poll = update.order_id.clone();
                            let order_for_poll = order.clone();
                            let mut last_status = update.status;
                            let mut last_filled_qty = update.filled_qty;
                            let mut last_fill_price = update.avg_fill_price;
                            let poll_interval_ms =
                                env_u64("PLOY_MANAGED_STRATEGY_ORDER_POLL_MS", 1500)
                                    .clamp(200, 10_000);
                            let poll_max_ms =
                                env_u64("PLOY_MANAGED_STRATEGY_ORDER_POLL_MAX_MS", 600_000)
                                    .max(poll_interval_ms);
                            let strategy_label_owned = strategy_label.to_string();

                            tokio::spawn(async move {
                                let started_at = std::time::Instant::now();
                                while started_at.elapsed().as_millis() < poll_max_ms as u128 {
                                    tokio::time::sleep(Duration::from_millis(poll_interval_ms))
                                        .await;

                                    let polled = match executor_for_poll
                                        .query_order_status(&exchange_order_id_for_poll)
                                        .await
                                    {
                                        Ok(r) => r,
                                        Err(e) => {
                                            debug!(
                                                strategy = strategy_label_owned.as_str(),
                                                strategy_id = %strategy_id_for_poll,
                                                client_order_id = %client_order_id_for_poll,
                                                exchange_order_id = %exchange_order_id_for_poll,
                                                error = %e,
                                                "managed strategy poll status failed (will retry)"
                                            );
                                            continue;
                                        }
                                    };

                                    let changed = polled.status != last_status
                                        || polled.filled_shares != last_filled_qty
                                        || polled.avg_fill_price != last_fill_price;
                                    if !changed {
                                        if polled.status.is_terminal() {
                                            break;
                                        }
                                        continue;
                                    }

                                    if polled.status == OrderStatus::Filled
                                        && last_status != OrderStatus::Filled
                                    {
                                        orders_filled_for_poll.fetch_add(1, Ordering::Relaxed);
                                    }

                                    let update = crate::strategy::OrderUpdate {
                                        order_id: polled.order_id,
                                        client_order_id: Some(client_order_id_for_poll.clone()),
                                        status: polled.status,
                                        filled_qty: polled.filled_shares,
                                        avg_fill_price: polled.avg_fill_price,
                                        timestamp: Utc::now(),
                                        error: None,
                                    };
                                    manager_for_poll.send_order_update(update.clone());

                                    if let Some(pool) = observability_pool_for_poll.as_ref() {
                                        let context = json!({
                                            "source": "managed_runtime",
                                            "phase": "poll",
                                            "order_id": update.order_id,
                                            "client_order_id": client_order_id_for_poll.clone(),
                                            "status": format!("{:?}", update.status),
                                            "filled_qty": update.filled_qty,
                                            "avg_fill_price": update.avg_fill_price.map(|p| p.to_string()),
                                        });
                                        persist_live_order_signal_history(
                                            pool,
                                            &observability_account_for_poll,
                                            strategy_label_owned.as_str(),
                                            &strategy_id_for_poll,
                                            "live_order_poll_update",
                                            Some(order_for_poll.token_id.as_str()),
                                            Some(order_for_poll.market_side.as_str()),
                                            Some(order_for_poll.limit_price),
                                            update.avg_fill_price,
                                            context,
                                        )
                                        .await;
                                    }

                                    if let Some(pool) = observability_pool_for_poll.as_ref() {
                                        let (leg, mode) = split_arb_leg_and_mode(
                                            client_order_id_for_poll.as_str(),
                                        );
                                        let status_key = split_arb_status_key(update.status);
                                        let signal_type =
                                            format!("split_arb_{}_{}_{}", leg, mode, status_key);
                                        let context = json!({
                                            "source": "managed_runtime",
                                            "phase": "poll",
                                            "order_id": update.order_id,
                                            "client_order_id": client_order_id_for_poll.clone(),
                                            "status": format!("{:?}", update.status),
                                            "filled_qty": update.filled_qty,
                                            "avg_fill_price": update.avg_fill_price.map(|p| p.to_string()),
                                            "leg": leg,
                                            "mode": mode,
                                        });
                                        persist_split_arb_signal_history(
                                            pool,
                                            &observability_account_for_poll,
                                            &strategy_id_for_poll,
                                            &signal_type,
                                            Some(order_for_poll.token_id.as_str()),
                                            Some(order_for_poll.market_side.as_str()),
                                            Some(order_for_poll.limit_price),
                                            update.avg_fill_price,
                                            None,
                                            context,
                                        )
                                        .await;
                                    }

                                    last_status = update.status;
                                    last_filled_qty = update.filled_qty;
                                    last_fill_price = update.avg_fill_price;

                                    if update.status.is_terminal() {
                                        break;
                                    }
                                }
                            });
                        }
                    }
                    Err(e) => {
                        warn!(
                            strategy = strategy_label,
                            strategy_id = %strategy_id,
                            error = %e,
                            "strategy action order execution failed"
                        );
                        let update = crate::strategy::OrderUpdate {
                            order_id: client_order_id.clone(),
                            client_order_id: Some(client_order_id.clone()),
                            status: OrderStatus::Failed,
                            filled_qty: 0,
                            avg_fill_price: None,
                            timestamp: Utc::now(),
                            error: Some(e.to_string()),
                        };
                        manager.send_order_update(update.clone());
                        if let Some(pool) = observability_pool.as_ref() {
                            let context = json!({
                                "source": "managed_runtime",
                                "phase": "submit_error",
                                "order_id": update.order_id,
                                "client_order_id": client_order_id.clone(),
                                "status": format!("{:?}", update.status),
                                "filled_qty": update.filled_qty,
                                "avg_fill_price": update.avg_fill_price.map(|p| p.to_string()),
                                "error": update.error,
                            });
                            persist_live_order_signal_history(
                                pool,
                                &observability_account_id,
                                strategy_label,
                                &strategy_id,
                                "live_order_submit_error",
                                Some(order.token_id.as_str()),
                                Some(order.market_side.as_str()),
                                Some(order.limit_price),
                                update.avg_fill_price,
                                context,
                            )
                            .await;
                        }
                        if split_arb_managed {
                            if let Some(pool) = observability_pool.as_ref() {
                                let (leg, mode) = split_arb_leg_and_mode(&client_order_id);
                                let signal_type = format!("split_arb_{}_{}_failed", leg, mode);
                                let context = json!({
                                    "source": "managed_runtime",
                                    "phase": "submit_error",
                                    "order_id": update.order_id,
                                    "client_order_id": client_order_id,
                                    "status": format!("{:?}", update.status),
                                    "filled_qty": update.filled_qty,
                                    "error": update.error,
                                    "leg": leg,
                                    "mode": mode,
                                });
                                persist_split_arb_signal_history(
                                    pool,
                                    &observability_account_id,
                                    &strategy_id,
                                    &signal_type,
                                    Some(order.token_id.as_str()),
                                    Some(order.market_side.as_str()),
                                    Some(order.limit_price),
                                    None,
                                    None,
                                    context,
                                )
                                .await;
                            }
                        }
                    }
                };
            }
            StrategyAction::CancelOrder { order_id } => match executor.cancel(&order_id).await {
                Ok(cancelled) => {
                    let (status, filled_qty, avg_fill_price, error) = if cancelled {
                        match executor.query_order_status(&order_id).await {
                            Ok(polled) => {
                                let status = if polled.status.is_terminal() {
                                    polled.status
                                } else {
                                    OrderStatus::Cancelled
                                };
                                (status, polled.filled_shares, polled.avg_fill_price, None)
                            }
                            Err(e) => {
                                debug!(
                                    strategy = strategy_label,
                                    strategy_id = %strategy_id,
                                    order_id = %order_id,
                                    error = %e,
                                    "cancel succeeded but post-cancel status query failed"
                                );
                                (OrderStatus::Cancelled, 0, None, None)
                            }
                        }
                    } else {
                        (
                            OrderStatus::Rejected,
                            0,
                            None,
                            Some("order not found or already closed".to_string()),
                        )
                    };
                    manager.send_order_update(crate::strategy::OrderUpdate {
                        order_id: order_id.clone(),
                        client_order_id: None,
                        status,
                        filled_qty,
                        avg_fill_price,
                        timestamp: Utc::now(),
                        error,
                    });
                }
                Err(e) => {
                    warn!(
                        strategy = strategy_label,
                        strategy_id = %strategy_id,
                        order_id = %order_id,
                        error = %e,
                        "strategy cancel failed"
                    );
                    manager.send_order_update(crate::strategy::OrderUpdate {
                        order_id,
                        client_order_id: None,
                        status: OrderStatus::Failed,
                        filled_qty: 0,
                        avg_fill_price: None,
                        timestamp: Utc::now(),
                        error: Some(e.to_string()),
                    });
                }
            },
            StrategyAction::ModifyOrder {
                order_id,
                new_price,
                new_size,
            } => {
                warn!(
                    strategy = strategy_label,
                    strategy_id = %strategy_id,
                    order_id = %order_id,
                    new_price = ?new_price,
                    new_size = ?new_size,
                    "strategy modify-order action is not implemented"
                );
            }
            StrategyAction::Alert { level, message } => {
                info!(
                    strategy = strategy_label,
                    strategy_id = %strategy_id,
                    alert_level = ?level,
                    message = message,
                    "strategy alert"
                );
            }
            StrategyAction::LogEvent { event } => {
                debug!(
                    strategy = strategy_label,
                    strategy_id = %strategy_id,
                    event_type = ?event.event_type,
                    message = event.message,
                    "strategy event"
                );
                if split_arb_managed {
                    if let Some(pool) = observability_pool.as_ref() {
                        let signal_type = split_arb_event_signal_type(&event);
                        let context = json!({
                            "source": "managed_runtime",
                            "phase": "strategy_event",
                            "event_type": format!("{:?}", event.event_type),
                            "message": event.message,
                            "data": event.data,
                            "timestamp": event.timestamp,
                        });
                        persist_split_arb_signal_history(
                            pool,
                            &observability_account_id,
                            &strategy_id,
                            &signal_type,
                            None,
                            None,
                            None,
                            None,
                            None,
                            context,
                        )
                        .await;
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_managed_strategy_runtime_task(
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    coordinator: &mut Coordinator,
    shutdown_tx: &broadcast::Sender<()>,
    strategy_label: &'static str,
    agent_id: String,
    domain: Domain,
    risk_params: AgentRiskParams,
    strategy_config_toml: String,
    dry_run: bool,
    pm_client: PolymarketClient,
    pm_ws_url: String,
    data_plane: Option<Arc<PlatformDataPlane>>,
    observability_pool: Option<PgPool>,
    observability_account_id: String,
) {
    let strategy_cmd_rx = coordinator.register_agent(agent_id.clone(), domain.clone(), risk_params);
    let strategy_shutdown_rx = shutdown_tx.subscribe();
    let strategy_agent_id_for_runtime = agent_id.clone();

    let jh = tokio::spawn(async move {
        if let Err(e) = run_managed_strategy_runtime_module(ManagedStrategyRuntimeConfig {
            strategy_label: strategy_label.to_string(),
            agent_id: strategy_agent_id_for_runtime,
            domain,
            strategy_config_toml,
            dry_run,
            pm_client,
            pm_ws_url,
            data_plane,
            observability_pool,
            observability_account_id,
            cmd_rx: strategy_cmd_rx,
            shutdown_rx: strategy_shutdown_rx,
        })
        .await
        {
            error!(
                agent = strategy_label,
                error = %e,
                "managed strategy runtime exited with error"
            );
        }
    });
    agent_handles.push(jh);
}

#[allow(clippy::too_many_arguments)]
fn spawn_managed_strategy_runtime_spec(
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    coordinator: &mut Coordinator,
    shutdown_tx: &broadcast::Sender<()>,
    runtime_spec: ManagedStrategyBootstrapSpec,
    risk_params: AgentRiskParams,
    dry_run: bool,
    pm_client: Option<PolymarketClient>,
    pm_ws_url: &str,
    data_plane: Option<Arc<PlatformDataPlane>>,
    observability_pool: Option<PgPool>,
    observability_account_id: &str,
) -> Result<()> {
    let pm_client = pm_client.ok_or_else(|| {
        crate::error::PloyError::Validation(format!(
            "managed strategy runtime '{}' requires a Polymarket client, but none was initialized",
            runtime_spec.strategy_label
        ))
    })?;

    spawn_managed_strategy_runtime_task(
        agent_handles,
        coordinator,
        shutdown_tx,
        runtime_spec.strategy_label,
        runtime_spec.agent_id,
        runtime_spec.domain,
        risk_params,
        runtime_spec.strategy_config_toml,
        dry_run,
        pm_client,
        pm_ws_url.to_string(),
        data_plane,
        observability_pool,
        observability_account_id.to_string(),
    );

    Ok(())
}

fn spawn_trading_agent_task<A: TradingAgent>(
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    coordinator: &mut Coordinator,
    handle: &CoordinatorHandle,
    agent: A,
    risk_params: AgentRiskParams,
    error_label: &'static str,
) {
    let agent_id = agent.id().to_string();
    let domain = agent.domain();
    let cmd_rx = coordinator.register_agent(agent_id.clone(), domain.clone(), risk_params);
    let ctx = AgentContext::new(agent_id, domain, handle.clone(), cmd_rx);

    let jh = tokio::spawn(async move {
        if let Err(e) = agent.run(ctx).await {
            error!(agent = error_label, error = %e, "agent exited with error");
        }
    });
    agent_handles.push(jh);
}

async fn load_sports_collector_targets(pool: &PgPool) -> HashMap<String, Side> {
    let mut desired: HashMap<String, Side> = HashMap::new();
    if let Ok(rows) = sqlx::query_as::<_, (String, Option<String>)>(
        r#"
        SELECT token_id, metadata->>'side'
        FROM collector_token_targets
        WHERE domain = 'SPORTS_NBA'
          AND target_date BETWEEN (CURRENT_DATE - 1) AND (CURRENT_DATE + 1)
          AND (expires_at IS NULL OR expires_at > NOW())
        "#,
    )
    .fetch_all(pool)
    .await
    {
        for (token_id, side_str) in rows {
            let side = match side_str.as_deref() {
                Some("DOWN") | Some("NO") => Side::Down,
                _ => Side::Up,
            };
            desired.insert(token_id, side);
        }
    }

    desired
}

async fn start_sports_market_data_support(
    shared_pool: Option<PgPool>,
    app_config: &AppConfig,
    freshness: Arc<crate::platform::DataPlaneFreshness>,
    sports_cfg: &SportsTradingConfig,
) -> Result<PgPool> {
    let pool = match shared_pool {
        Some(pool) => pool,
        None => {
            PgPoolOptions::new()
                .max_connections(app_config.database.max_connections)
                .connect(&app_config.database.url)
                .await?
        }
    };
    spawn_polymarket_trade_persistence_from_collector_targets(
        pool.clone(),
        sports_cfg.agent_id.clone(),
        Domain::Sports,
    );

    let sports_data_plane_config = DataPlaneConfig {
        polymarket_ws_url: app_config.market.ws_url.clone(),
        ..DataPlaneConfig::default()
    };
    let sports_data_plane = Arc::new(PlatformDataPlane::new(
        sports_data_plane_config,
        Arc::clone(&freshness),
    ));
    sports_data_plane.start(Vec::new()).await?;
    let sports_pm_ws = sports_data_plane.polymarket_ws().ok_or_else(|| {
        crate::error::PloyError::Validation(
            "sports data plane misconfigured: missing Polymarket WS adapter".to_string(),
        )
    })?;

    let sports_desired = load_sports_collector_targets(&pool).await;
    let initial_count = sports_desired.len();
    if initial_count > 0 {
        sports_pm_ws.reconcile_token_sides(&sports_desired).await;
        info!(
            agent = sports_cfg.agent_id,
            token_count = initial_count,
            "seeded sports PM WS tokens for L2 data collection"
        );
    }

    let refresh_ws = sports_pm_ws.clone();
    let refresh_pool = pool.clone();
    let refresh_agent = sports_cfg.agent_id.clone();
    tokio::spawn(async move {
        let secs = env_u64("PM_SPORTS_COLLECTOR_REFRESH_SECS", 300).max(30);
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let desired = load_sports_collector_targets(&refresh_pool).await;
            let (_a, _r, _u, total) = refresh_ws.reconcile_token_sides(&desired).await;
            trace!(
                agent = refresh_agent,
                total,
                "refreshed sports PM WS token subscriptions"
            );
        }
    });

    let sports_quote_table_ready = match ensure_clob_quote_ticks_table(&pool).await {
        Ok(()) => true,
        Err(e) => {
            warn!(
                agent = sports_cfg.agent_id,
                error = %e,
                "failed to ensure clob_quote_ticks table; sports quote persistence bridge disabled"
            );
            false
        }
    };
    let sports_orderbook_table_ready = match ensure_clob_orderbook_snapshots_table(&pool).await {
        Ok(()) => true,
        Err(e) => {
            warn!(
                agent = sports_cfg.agent_id,
                error = %e,
                "failed to ensure clob_orderbook_snapshots table; sports orderbook persistence bridge disabled"
            );
            false
        }
    };
    if sports_quote_table_ready || sports_orderbook_table_ready {
        let sports_orderbook_levels = env_usize("PM_ORDERBOOK_LEVELS", 20).clamp(1, 200);
        let sports_orderbook_snapshot_ms = match std::env::var("PM_ORDERBOOK_SNAPSHOT_MS") {
            Ok(raw) => raw.parse::<u64>().unwrap_or(0),
            Err(_) => {
                (env_i64("PM_ORDERBOOK_SNAPSHOT_SECS", 60).max(0) as u64).saturating_mul(1000)
            }
        };
        let sports_orderbook_require_hash_change =
            env_bool("PM_ORDERBOOK_REQUIRE_HASH_CHANGE", true);
        let sports_pipeline_config = crate::platform::PersistenceConfig {
            clob_quote_min_interval_secs: CLOB_PERSIST_MIN_INTERVAL_SECS,
            clob_orderbook_snapshot_interval_ms: sports_orderbook_snapshot_ms as i64,
            clob_orderbook_max_levels: sports_orderbook_levels,
            clob_orderbook_require_hash_change: sports_orderbook_require_hash_change,
            ..Default::default()
        };
        let sports_pipeline = crate::platform::PersistencePipeline::spawn_with_freshness(
            pool.clone(),
            sports_pipeline_config,
            Some(Arc::clone(&freshness)),
        );

        if sports_quote_table_ready {
            if let Some(quote_rx) = sports_data_plane.subscribe_quotes() {
                sports_pipeline.spawn_bridge(
                    quote_rx,
                    format!("{}.sports_quote", sports_cfg.agent_id),
                    |update| {
                        Some(crate::platform::PersistenceEvent::ClobQuote(
                            crate::platform::ClobQuoteTick {
                                token_id: update.token_id.clone(),
                                side: update.side.as_str().to_string(),
                                best_bid: update.quote.best_bid,
                                best_ask: update.quote.best_ask,
                                bid_size: update.quote.bid_size,
                                ask_size: update.quote.ask_size,
                                domain: Domain::Sports,
                                received_at: Utc::now(),
                            },
                        ))
                    },
                );
            } else {
                warn!("sports quote bridge unavailable: no quote receiver");
            }
        }

        if sports_orderbook_table_ready {
            if let Some(book_rx) = sports_data_plane.subscribe_books() {
                sports_pipeline.spawn_bridge(
                    book_rx,
                    format!("{}.sports_orderbook", sports_cfg.agent_id),
                    |book_msg| {
                        use sha2::{Digest, Sha256};
                        let bids_json = serde_json::to_value(&book_msg.bids).unwrap_or_default();
                        let asks_json = serde_json::to_value(&book_msg.asks).unwrap_or_default();
                        let mut hasher = Sha256::new();
                        hasher.update(bids_json.to_string().as_bytes());
                        hasher.update(asks_json.to_string().as_bytes());
                        let hash = format!("{:x}", hasher.finalize());
                        Some(crate::platform::PersistenceEvent::ClobOrderbook(
                            crate::platform::ClobOrderbookSnapshot {
                                domain: Domain::Sports,
                                token_id: book_msg.asset_id.clone(),
                                market: Some(book_msg.market.clone()),
                                bids: bids_json,
                                asks: asks_json,
                                book_timestamp: book_msg
                                    .timestamp
                                    .as_deref()
                                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                                    .map(|dt| dt.with_timezone(&Utc)),
                                hash,
                                source: "polymarket_ws".into(),
                                context: None,
                            },
                        ))
                    },
                );
            } else {
                warn!("sports orderbook bridge unavailable: no book receiver");
            }
        }
    } else {
        warn!(
            agent = sports_cfg.agent_id,
            "sports persistence tables unavailable; WS persistence bridges disabled"
        );
    }

    info!(
        agent = sports_cfg.agent_id,
        "sports PM WS L2 data collection started"
    );

    Ok(pool)
}

fn spawn_legacy_nba_comeback_agent(
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    coordinator: &mut Coordinator,
    handle: &CoordinatorHandle,
    sports_cfg: SportsTradingConfig,
    nba_cfg: crate::config::NbaComebackConfig,
    pool: PgPool,
) {
    warn!(
        agent = %sports_cfg.agent_id,
        "grok-enabled nba_comeback deployment remains on the legacy sports agent path until canonical strategy runtime absorbs Grok behavior"
    );

    let espn = crate::strategy::nba_comeback::espn::EspnClient::new();
    let stats =
        crate::strategy::nba_comeback::ComebackStatsProvider::new(pool.clone(), nba_cfg.season.clone());
    let core = crate::strategy::nba_comeback::NbaComebackCore::new(espn, stats, nba_cfg.clone());
    let risk_params = sports_cfg.risk_params.clone();
    let mut agent = SportsTradingAgent::new(sports_cfg.clone(), core).with_observation_pool(pool);

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
        match crate::ai_clients::grok::GrokClient::from_env() {
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

    spawn_trading_agent_task(
        agent_handles,
        coordinator,
        handle,
        agent,
        risk_params,
        "sports",
    );
    info!("sports agent spawned");
}

/// Start the multi-agent platform
///
/// Creates shared infrastructure, registers configured agents,
/// and runs the coordinator loop until shutdown.
pub async fn start_platform(
    config: PlatformBootstrapConfig,
    app_config: &AppConfig,
    control: PlatformStartControl,
) -> Result<()> {
    let exchange_kind = parse_exchange_kind(&app_config.execution.exchange)?;
    let exchange_client = build_exchange_client(app_config, config.dry_run).await?;
    let non_pm_builtin_agents_enabled = exchange_kind != ExchangeKind::Polymarket
        && (config.enable_crypto || config.enable_sports || config.enable_politics);
    if non_pm_builtin_agents_enabled {
        return Err(crate::error::PloyError::Validation(format!(
            "execution.exchange={} is not yet supported with built-in agents (crypto/sports/politics). Disable built-in agents or set execution.exchange=polymarket",
            exchange_kind
        )));
    }

    // Polymarket client is required for:
    // - crypto event discovery (Gamma)
    // - settlement persistence (Gamma)
    // - politics agent
    // - sports settlement labeling (Gamma)
    let needs_polymarket_client =
        config.enable_crypto || config.enable_sports || config.enable_politics;
    let pm_client = if needs_polymarket_client {
        let rest_url = app_config
            .market
            .exchange_rest_url
            .as_deref()
            .unwrap_or(&app_config.market.rest_url);

        if config.dry_run {
            Some(PolymarketClient::new(rest_url, true)?)
        } else {
            let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
            let funder = std::env::var("POLYMARKET_FUNDER").ok();
            if let Some(funder_addr) = funder {
                Some(
                    PolymarketClient::new_authenticated_proxy(rest_url, wallet, &funder_addr, true)
                        .await?,
                )
            } else {
                Some(PolymarketClient::new_authenticated(rest_url, wallet, true).await?)
            }
        }
    } else {
        None
    };

    let account_id = if app_config.account.id.trim().is_empty() {
        "default".to_string()
    } else {
        app_config.account.id.clone()
    };
    let runtime_crypto_targets =
        collect_runtime_crypto_strategy_targets(&account_id, config.dry_run);
    #[cfg(feature = "rl")]
    let crypto_rl_policy_enabled = config.enable_crypto_rl_policy;
    #[cfg(not(feature = "rl"))]
    let crypto_rl_policy_enabled = false;

    info!(
        account_id = %account_id,
        crypto = config.enable_crypto,
        crypto_momentum = config.enable_crypto_momentum,
        crypto_pattern_memory = config.enable_crypto_pattern_memory,
        crypto_split_arb = config.enable_crypto_split_arb,
        crypto_lob_ml = config.enable_crypto_lob_ml,
        crypto_rl_policy = crypto_rl_policy_enabled,
        sports = config.enable_sports,
        politics = config.enable_politics,
        economics = config.enable_economics,
        openclaw = config.enable_openclaw || config.openclaw.enabled,
        exchange = %exchange_kind,
        dry_run = config.dry_run,
        "starting multi-agent platform"
    );
    if config.enable_economics {
        warn!(
            "economics domain enabled, but no built-in economics agent is registered; coordinator-level risk and allocator gates remain active"
        );
    }

    let mut allowed_domains: HashSet<Domain> = HashSet::new();
    if config.enable_crypto {
        allowed_domains.insert(Domain::Crypto);
    }
    if config.enable_sports {
        allowed_domains.insert(Domain::Sports);
    }
    if config.enable_politics {
        allowed_domains.insert(Domain::Politics);
    }

    let db_required = env_bool(
        "PLOY_DB_REQUIRED",
        env_bool("PLOY_REQUIRE_DB", !app_config.dry_run.enabled),
    );

    // Optional shared DB pool used for (a) coordinator execution logs and (b) market data persistence.
    // Crypto agents can run without DB; sports agent requires DB for calendar/stats.
    let shared_pool = match PgPoolOptions::new()
        .max_connections(app_config.database.max_connections)
        .connect(&app_config.database.url)
        .await
    {
        Ok(pool) => Some(pool),
        Err(e) => {
            if db_required {
                return Err(crate::error::PloyError::Internal(format!(
                    "database connection is required but failed at startup: {}",
                    e
                )));
            }
            warn!(
                error = %e,
                "failed to connect DB at startup; continuing without shared pool"
            );
            None
        }
    };

    // 1. Create shared executor (+ DB-backed idempotency when DB is available)
    let exec_config = app_config.execution.clone();
    let mut executor_builder =
        OrderExecutor::new_with_exchange(exchange_client.clone(), exec_config);
    if let Some(pool) = shared_pool.as_ref() {
        let idem_store = PostgresStore::from_pool(pool.clone());
        let idem_mgr = Arc::new(IdempotencyManager::new_with_account(
            idem_store,
            account_id.clone(),
        ));
        executor_builder = executor_builder.with_idempotency(idem_mgr.clone());
        info!("order executor idempotency enabled");

        // Spawn hourly idempotency key cleanup task
        let cleanup_mgr = idem_mgr.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                match cleanup_mgr.cleanup_expired().await {
                    Ok(n) if n > 0 => info!("idempotency cleanup: removed {} expired keys", n),
                    Err(e) => warn!("idempotency cleanup error: {}", e),
                    _ => {}
                }
            }
        });
    } else {
        warn!("order executor idempotency disabled (no database connection)");
    }
    let executor = Arc::new(executor_builder);

    // 2. Create coordinator
    let mut coordinator = Coordinator::new(
        config.coordinator.clone(),
        executor,
        account_id.clone(),
        allowed_domains.clone(),
    );
    if let Some(pool) = shared_pool.as_ref() {
        // Run migrations by default whenever a DB connection is available, even in dry-run.
        // This prevents long-lived services from starting on a stale schema.
        let mut run_sqlx_migrations = env_bool("PLOY_RUN_SQLX_MIGRATIONS", true);
        let require_sqlx_migrations = env_bool("PLOY_REQUIRE_SQLX_MIGRATIONS", true);
        if require_sqlx_migrations && !run_sqlx_migrations {
            warn!(
                "PLOY_RUN_SQLX_MIGRATIONS=false but PLOY_REQUIRE_SQLX_MIGRATIONS=true; forcing migrations"
            );
            run_sqlx_migrations = true;
        }
        let require_startup_schema =
            env_bool("PLOY_REQUIRE_STARTUP_SCHEMA", !app_config.dry_run.enabled);
        let require_runtime_restore = env_bool(
            "PLOY_REQUIRE_RUNTIME_STATE_RESTORE",
            !app_config.dry_run.enabled,
        );
        let migration_store = PostgresStore::from_pool(pool.clone());
        if run_sqlx_migrations {
            if let Err(e) = migration_store.migrate().await {
                if require_sqlx_migrations {
                    return Err(e);
                }
                warn!(
                    error = %e,
                    "sqlx migration runner failed at startup; continuing due to PLOY_REQUIRE_SQLX_MIGRATIONS=false"
                );
            }
        } else {
            info!("sqlx migration runner skipped at startup (PLOY_RUN_SQLX_MIGRATIONS=false)");
        }
        ensure_schema_repairs(pool).await?;
        if let Err(e) = ensure_accounts_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure accounts table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure accounts table");
        } else if let Err(e) =
            upsert_account_from_config(pool, &account_id, &app_config.account).await
        {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to upsert account metadata: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to upsert account metadata");
        }
        if let Err(e) = ensure_coordinator_governance_policies_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure coordinator_governance_policies table: {}",
                    e
                )));
            }
            warn!(
                error = %e,
                "failed to ensure coordinator_governance_policies table; governance persistence disabled"
            );
        } else if let Err(e) = ensure_coordinator_governance_policy_history_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure coordinator_governance_policy_history table: {}",
                    e
                )));
            }
            warn!(
                error = %e,
                "failed to ensure coordinator_governance_policy_history table; governance history persistence disabled"
            );
        } else {
            coordinator.set_governance_store_pool(pool.clone());
            if let Err(e) = coordinator.load_persisted_governance_policy().await {
                if require_startup_schema {
                    return Err(crate::error::PloyError::Internal(format!(
                        "failed to restore coordinator governance policy: {}",
                        e
                    )));
                }
                warn!(
                    error = %e,
                    "failed to restore coordinator governance policy from DB"
                );
            }
        }
        if let Err(e) = ensure_agent_order_executions_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure agent_order_executions table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure agent_order_executions table; execution logging disabled");
        } else {
            coordinator.set_execution_log_pool(pool.clone());
            if let Err(e) = coordinator.restore_runtime_state_from_execution_log().await {
                if require_runtime_restore {
                    return Err(crate::error::PloyError::Internal(format!(
                        "failed to restore coordinator runtime state from execution log: {}",
                        e
                    )));
                }
                warn!(
                    error = %e,
                    "failed to restore coordinator runtime state from execution log"
                );
            }
        }
        if let Err(e) = ensure_strategy_observability_tables(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure strategy observability tables: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure strategy observability tables");
        }
        if let Err(e) = ensure_pm_market_metadata_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure pm_market_metadata table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure pm_market_metadata table");
        }
        if let Err(e) = ensure_pm_token_settlements_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure pm_token_settlements table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure pm_token_settlements table");
        }
        if let Err(e) = ensure_risk_runtime_state_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure risk_runtime_state table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure risk_runtime_state table");
        } else if let Err(e) = coordinator.restore_risk_runtime_state().await {
            warn!(error = %e, "failed to restore risk runtime state");
        }
        if config.enable_crypto {
            if let Err(e) = ensure_clob_trade_alerts_table(pool).await {
                if require_startup_schema {
                    return Err(crate::error::PloyError::Internal(format!(
                        "failed to ensure clob_trade_alerts table: {}",
                        e
                    )));
                }
                warn!(
                    error = %e,
                    "failed to ensure clob_trade_alerts table at startup"
                );
            }
        }
    }

    let ingress_agents = std::env::var("PLOY_EXTERNAL_INGRESS_AGENT_IDS")
        .unwrap_or_else(|_| "openclaw_rpc,sidecar".to_string());
    for agent_id in ingress_agents
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        coordinator
            .authorize_external_agent(agent_id, AgentRiskParams::conservative())
            .await;
    }
    let handle = coordinator.handle();
    let _global_state = coordinator.global_state();

    // 2a. Start API server with platform services (if api feature enabled)
    #[cfg(feature = "api")]
    let _api_handle = {
        use crate::adapters::{start_api_server_platform_background, PostgresStore};
        use crate::ai_clients::grok::GrokClient;
        use crate::api::state::StrategyConfigState;

        let api_port = std::env::var("API_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(8081);

        // Initialize Grok client if GROK_API_KEY is set
        let grok_client = std::env::var("GROK_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .and_then(|_| match GrokClient::from_env() {
                Ok(client) => {
                    info!("Grok client initialized for sidecar endpoints");
                    Some(Arc::new(client))
                }
                Err(e) => {
                    warn!(error = %e, "failed to initialize Grok client");
                    None
                }
            });

        if let Some(ref pool) = shared_pool {
            let store = Arc::new(PostgresStore::from_pool(pool.clone()));
            let api_config = StrategyConfigState {
                symbols: vec![],
                min_move: 0.0,
                max_entry: 1.0,
                shares: 0,
                predictive: false,
                exit_edge_floor: None,
                exit_price_band: None,
                time_decay_exit_secs: None,
                liquidity_exit_spread_bps: None,
            };

            match start_api_server_platform_background(
                store,
                api_port,
                api_config,
                Some(handle.clone()),
                grok_client,
                account_id.clone(),
                config.dry_run,
            )
            .await
            {
                Ok(handle) => {
                    info!(
                        port = api_port,
                        "API server started in platform mode with sidecar endpoints"
                    );
                    Some(handle)
                }
                Err(e) => {
                    warn!(error = %e, "API server failed to start");
                    None
                }
            }
        } else {
            warn!("API server not started: no database connection");
            None
        }
    };
    #[cfg(not(feature = "api"))]
    let _api_handle: Option<tokio::task::JoinHandle<crate::error::Result<()>>> = None;

    // 3. Shutdown broadcast channel
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // 3b. Optional Polymarket settlement persistence (Gamma) for training labels.
    // Keep it read-only and enabled even in dry-run (no order placement).
    if let Some(pool) = shared_pool.as_ref() {
        let mut collector_domains: Vec<&'static str> = Vec::new();
        if config.enable_crypto {
            collector_domains.push("CRYPTO");
        }
        if config.enable_sports {
            collector_domains.push("SPORTS_NBA");
        }

        if !collector_domains.is_empty() {
            if let Some(client) = pm_client.clone() {
                spawn_pm_token_settlement_persistence(
                    client,
                    pool.clone(),
                    format!("settlements:{}", account_id),
                    collector_domains,
                );
            } else {
                warn!(
                    account_id = %account_id,
                    "pm client not configured; skipping token settlement persistence task"
                );
            }
        }
    }

    // 4. Spawn agents
    let mut agent_handles = Vec::new();

    // Shared per-symbol freshness tracker — attached to all WS adapters.
    let freshness = Arc::new(crate::platform::DataPlaneFreshness::new());
    let use_data_plane = env_bool("PLOY_DATA_PLANE", false);

    if config.enable_crypto {
        let crypto_cfg = config.crypto.clone();
        let momentum_enabled = config.enable_crypto_momentum;
        let pattern_memory_enabled = config.enable_crypto_pattern_memory;
        let split_arb_enabled = config.enable_crypto_split_arb;
        let lob_cfg = config.crypto_lob_ml.clone();
        let lob_agent_enabled = config.enable_crypto_lob_ml;
        #[cfg(feature = "rl")]
        let rl_cfg = config.crypto_rl_policy.clone();
        #[cfg(feature = "rl")]
        let rl_agent_enabled = config.enable_crypto_rl_policy;
        #[cfg(not(feature = "rl"))]
        let rl_agent_enabled = false;

        // Discover active crypto events and token IDs (Gamma API) via EventMatcher
        let pm_client_ref = pm_client.as_ref().ok_or_else(|| {
            crate::error::PloyError::Validation(
                "crypto domain requires a Polymarket client, but none was initialized".to_string(),
            )
        })?;
        let event_matcher = Arc::new(EventMatcher::new(pm_client_ref.clone()));
        if let Err(e) = event_matcher.refresh().await {
            warn!(error = %e, "crypto event matcher refresh failed (continuing)");
        }

        // Build a unified coin set across all enabled crypto strategies.
        // Also build a SubscriptionPlan for audit/observability (Phase 1 shadow).
        let mut all_coins: Vec<String> = Vec::new();
        let mut planner_requirements: Vec<(crate::platform::ConsumerId, Domain, Vec<DataFeed>)> =
            Vec::new();
        let mut momentum_coins: Vec<String> = if runtime_crypto_targets.momentum_coins.is_empty() {
            crypto_cfg.coins.clone()
        } else {
            runtime_crypto_targets
                .momentum_coins
                .iter()
                .cloned()
                .collect()
        };
        momentum_coins.sort();
        momentum_coins.dedup();
        let momentum_symbols: Vec<String> = momentum_coins
            .iter()
            .filter_map(|coin| coin_symbol_for(coin.trim_end_matches("USDT")))
            .collect();
        if momentum_enabled {
            planner_requirements.push((
                crate::platform::ConsumerId::from(format!("momentum-{}", crypto_cfg.agent_id)),
                Domain::Crypto,
                vec![DataFeed::BinanceSpot {
                    symbols: momentum_symbols.clone(),
                }],
            ));
            for coin in &momentum_coins {
                if !all_coins.contains(coin) {
                    all_coins.push(coin.clone());
                }
            }
        }
        if lob_agent_enabled {
            let symbols: Vec<String> = lob_cfg.coins.iter().map(|c| format!("{}USDT", c)).collect();
            planner_requirements.push((
                crate::platform::ConsumerId::from("lob-ml"),
                Domain::Crypto,
                vec![DataFeed::BinanceSpot { symbols }],
            ));
            for coin in &lob_cfg.coins {
                if !all_coins.contains(coin) {
                    all_coins.push(coin.clone());
                }
            }
        }
        #[cfg(feature = "rl")]
        if rl_agent_enabled {
            let symbols: Vec<String> = rl_cfg.coins.iter().map(|c| format!("{}USDT", c)).collect();
            planner_requirements.push((
                crate::platform::ConsumerId::from("rl-policy"),
                Domain::Crypto,
                vec![DataFeed::BinanceSpot { symbols }],
            ));
            for coin in &rl_cfg.coins {
                if !all_coins.contains(coin) {
                    all_coins.push(coin.clone());
                }
            }
        }
        if use_data_plane && pattern_memory_enabled {
            let mut coins: Vec<String> = if runtime_crypto_targets.pattern_memory_coins.is_empty() {
                crypto_cfg.coins.clone()
            } else {
                runtime_crypto_targets
                    .pattern_memory_coins
                    .iter()
                    .cloned()
                    .collect()
            };
            coins.sort();
            coins.dedup();
            for coin in coins {
                if !all_coins.contains(&coin) {
                    all_coins.push(coin);
                }
            }
        }
        if use_data_plane && split_arb_enabled {
            let mut coins: Vec<String> = if runtime_crypto_targets.split_arb_coins.is_empty() {
                crypto_cfg.coins.clone()
            } else {
                runtime_crypto_targets
                    .split_arb_coins
                    .iter()
                    .cloned()
                    .collect()
            };
            coins.sort();
            coins.dedup();
            for coin in coins {
                if !all_coins.contains(&coin) {
                    all_coins.push(coin);
                }
            }
        }
        if all_coins.is_empty() {
            warn!("crypto domain enabled but no crypto agents are active (coins set is empty)");
        }

        // Build and log the subscription plan (Phase 1: shadow audit).
        let subscription_plan =
            crate::platform::SubscriptionPlanner::build_plan(planner_requirements);
        info!(
            unique_keys = subscription_plan.key_count(),
            total_refs = subscription_plan.ref_count(),
            binance_symbols = subscription_plan.binance_symbols().len(),
            "subscription plan built (shadow audit)"
        );

        // Create WebSocket feeds
        let symbols: Vec<String> = all_coins.iter().map(|c| format!("{}USDT", c)).collect();
        let mut data_plane: Option<Arc<PlatformDataPlane>> = None;
        let (binance_ws, pm_ws) = if use_data_plane {
            let data_plane_config = DataPlaneConfig {
                polymarket_ws_url: app_config.market.ws_url.clone(),
                binance_spot_symbols: symbols.clone(),
                binance_kline_symbols: symbols.clone(),
                binance_kline_intervals: vec!["5m".to_string(), "15m".to_string()],
                binance_kline_closed_only: true,
                chainlink_symbols: vec![],
            };
            let dp = Arc::new(PlatformDataPlane::new(
                data_plane_config,
                Arc::clone(&freshness),
            ));
            dp.start(Vec::new()).await?;
            info!("PlatformDataPlane started");

            let binance_ws = dp.binance_ws().ok_or_else(|| {
                crate::error::PloyError::Validation(
                    "PLOY_DATA_PLANE=1 but PlatformDataPlane has no Binance WS adapter".to_string(),
                )
            })?;
            let pm_ws = dp.polymarket_ws().ok_or_else(|| {
                crate::error::PloyError::Validation(
                    "PLOY_DATA_PLANE=1 but PlatformDataPlane has no Polymarket WS adapter"
                        .to_string(),
                )
            })?;
            data_plane = Some(dp);
            (binance_ws, pm_ws)
        } else {
            let binance_ws = Arc::new(BinanceWebSocket::new(symbols));
            let pm_ws = Arc::new(PolymarketWebSocket::new(&app_config.market.ws_url));

            // Attach per-symbol freshness tracker to WS adapters.
            binance_ws.set_freshness(Arc::clone(&freshness));
            pm_ws.set_freshness(Arc::clone(&freshness));
            info!("data plane freshness tracker attached to WS adapters");
            (binance_ws, pm_ws)
        };
        let crypto_market_data = CryptoDataPlaneHandle::new(binance_ws.clone(), pm_ws.clone());

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
                            "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
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
                            "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                        })),
                );
            }
        }
        if use_data_plane {
            for (token, side) in desired.iter() {
                pm_ws.register_token(token, *side).await;
            }
            pm_ws.request_resubscribe();
            info!(
                agent = %crypto_cfg.agent_id,
                token_count = desired.len(),
                "seeded PM token mappings for crypto data collection"
            );
        } else {
            let (_added, _removed, _updated, total) = pm_ws.reconcile_token_sides(&desired).await;
            info!(
                agent = %crypto_cfg.agent_id,
                token_count = total,
                "seeded PM token mappings for crypto data collection"
            );
        }

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
        let use_data_plane_collector = use_data_plane;
        let initial_last_desired = if use_data_plane_collector {
            desired.clone()
        } else {
            HashMap::new()
        };
        tokio::spawn(async move {
            let refresh_secs =
                env_u64("PM_COLLECTOR_REFRESH_SECS", PM_COLLECTOR_REFRESH_SECS).max(10);
            let mut tick = tokio::time::interval(Duration::from_secs(refresh_secs));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut last_desired = initial_last_desired;

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
                                "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
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
                                "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                            })),
                        );
                    }
                }

                if use_data_plane_collector {
                    if desired != last_desired {
                        let previous_token_count = last_desired.len();
                        for (token, side) in desired.iter() {
                            pm_ws_collector.register_token(token, *side).await;
                        }
                        pm_ws_collector.request_resubscribe();
                        info!(
                            agent = %agent_id_collector,
                            previous_token_count,
                            token_count = desired.len(),
                            "pm token collector refreshed token set on shared data-plane ws; resubscribe requested"
                        );
                        last_desired = desired;
                    }
                } else {
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

        let mut crypto_persistence_pipeline: Option<crate::platform::PersistencePipelineHandle> =
            None;
        // Optional persistence pipeline for WS-driven market data (best-effort).
        // Do not block agent startup if DB is temporarily unavailable.
        if let Some(pool) = shared_pool.as_ref() {
            let (orderbook_levels_default, orderbook_snapshot_secs_default) = (20usize, 60i64);
            let orderbook_levels =
                env_usize("PM_ORDERBOOK_LEVELS", orderbook_levels_default).clamp(1, 200);
            let orderbook_snapshot_ms = match std::env::var("PM_ORDERBOOK_SNAPSHOT_MS") {
                Ok(raw) => raw.parse::<u64>().unwrap_or(0),
                Err(_) => (env_i64(
                    "PM_ORDERBOOK_SNAPSHOT_SECS",
                    orderbook_snapshot_secs_default,
                )
                .max(0) as u64)
                    .saturating_mul(1000),
            };
            let orderbook_require_hash_change = env_bool("PM_ORDERBOOK_REQUIRE_HASH_CHANGE", true);

            let quote_table_ready = match ensure_clob_quote_ticks_table(pool).await {
                Ok(()) => true,
                Err(e) => {
                    warn!(
                        agent = crypto_cfg.agent_id,
                        error = %e,
                        "failed to ensure clob_quote_ticks table; quote persistence bridge disabled"
                    );
                    false
                }
            };
            let price_table_ready = match ensure_binance_price_ticks_table(pool).await {
                Ok(()) => true,
                Err(e) => {
                    warn!(
                        agent = crypto_cfg.agent_id,
                        error = %e,
                        "failed to ensure binance_price_ticks table; Binance price persistence bridge disabled"
                    );
                    false
                }
            };
            let orderbook_table_ready = match ensure_clob_orderbook_snapshots_table(pool).await {
                Ok(()) => true,
                Err(e) => {
                    warn!(
                        agent = crypto_cfg.agent_id,
                        error = %e,
                        "failed to ensure clob_orderbook_snapshots table; orderbook persistence bridge disabled"
                    );
                    false
                }
            };

            if quote_table_ready || price_table_ready || orderbook_table_ready {
                let pipeline_config = crate::platform::PersistenceConfig {
                    clob_quote_min_interval_secs: CLOB_PERSIST_MIN_INTERVAL_SECS,
                    binance_price_min_interval_secs: BINANCE_PERSIST_MIN_INTERVAL_SECS,
                    binance_lob_snapshot_interval_ms: env_u64("BN_LOB_SNAPSHOT_MS", 1000).max(100)
                        as i64,
                    binance_lob_max_levels: env_usize("BN_LOB_LEVELS", 20).clamp(0, 200),
                    clob_orderbook_snapshot_interval_ms: orderbook_snapshot_ms as i64,
                    clob_orderbook_max_levels: orderbook_levels,
                    clob_orderbook_require_hash_change: orderbook_require_hash_change,
                    ..Default::default()
                };
                let pipeline_handle = crate::platform::PersistencePipeline::spawn_with_freshness(
                    pool.clone(),
                    pipeline_config,
                    Some(Arc::clone(&freshness)),
                );
                crypto_persistence_pipeline = Some(pipeline_handle.clone());

                if quote_table_ready {
                    let quote_rx = if use_data_plane {
                        data_plane.as_ref().and_then(|dp| dp.subscribe_quotes())
                    } else {
                        Some(pm_ws.subscribe_updates())
                    };
                    if let Some(quote_rx) = quote_rx {
                        pipeline_handle.spawn_bridge(
                            quote_rx,
                            format!("{}.quote", crypto_cfg.agent_id),
                            |update| {
                                Some(crate::platform::PersistenceEvent::ClobQuote(
                                    crate::platform::ClobQuoteTick {
                                        token_id: update.token_id.clone(),
                                        side: update.side.as_str().to_string(),
                                        best_bid: update.quote.best_bid,
                                        best_ask: update.quote.best_ask,
                                        bid_size: update.quote.bid_size,
                                        ask_size: update.quote.ask_size,
                                        domain: Domain::Crypto,
                                        received_at: Utc::now(),
                                    },
                                ))
                            },
                        );
                    } else {
                        warn!("persistence quote bridge unavailable: no quote receiver");
                    }
                }

                if price_table_ready {
                    let price_rx = if use_data_plane {
                        data_plane.as_ref().and_then(|dp| dp.subscribe_prices())
                    } else {
                        Some(binance_ws.subscribe())
                    };
                    if let Some(price_rx) = price_rx {
                        pipeline_handle.spawn_bridge(
                            price_rx,
                            format!("{}.price", crypto_cfg.agent_id),
                            |update| {
                                Some(crate::platform::PersistenceEvent::BinancePrice(
                                    crate::platform::BinancePriceTick {
                                        symbol: update.symbol.clone(),
                                        price: Some(update.price),
                                        quantity: update.quantity,
                                        trade_time: update.timestamp,
                                    },
                                ))
                            },
                        );
                    } else {
                        warn!("persistence price bridge unavailable: no price receiver");
                    }
                }

                if orderbook_table_ready {
                    let book_rx = if use_data_plane {
                        data_plane.as_ref().and_then(|dp| dp.subscribe_books())
                    } else {
                        Some(pm_ws.subscribe_books())
                    };
                    if let Some(book_rx) = book_rx {
                        pipeline_handle.spawn_bridge(
                            book_rx,
                            format!("{}.orderbook", crypto_cfg.agent_id),
                            |book_msg| {
                                use sha2::{Digest, Sha256};
                                let bids_json =
                                    serde_json::to_value(&book_msg.bids).unwrap_or_default();
                                let asks_json =
                                    serde_json::to_value(&book_msg.asks).unwrap_or_default();
                                let mut hasher = Sha256::new();
                                hasher.update(bids_json.to_string().as_bytes());
                                hasher.update(asks_json.to_string().as_bytes());
                                let hash = format!("{:x}", hasher.finalize());
                                Some(crate::platform::PersistenceEvent::ClobOrderbook(
                                    crate::platform::ClobOrderbookSnapshot {
                                        domain: Domain::Crypto,
                                        token_id: book_msg.asset_id.clone(),
                                        market: Some(book_msg.market.clone()),
                                        bids: bids_json,
                                        asks: asks_json,
                                        book_timestamp: book_msg
                                            .timestamp
                                            .as_deref()
                                            .and_then(|s| {
                                                chrono::DateTime::parse_from_rfc3339(s).ok()
                                            })
                                            .map(|dt| dt.with_timezone(&Utc)),
                                        hash,
                                        source: "polymarket_ws".into(),
                                        context: None,
                                    },
                                ))
                            },
                        );
                    } else {
                        warn!("persistence orderbook bridge unavailable: no book receiver");
                    }
                }

                info!(agent = crypto_cfg.agent_id, "persistence pipeline started");
            } else {
                warn!(
                    agent = crypto_cfg.agent_id,
                    "all market-data persistence tables unavailable; WS persistence bridges disabled"
                );
            }

            // Trade persistence (polling-based) — still separate from WS pipeline.
            spawn_polymarket_trade_persistence(
                event_matcher.clone(),
                pool.clone(),
                crypto_cfg.agent_id.clone(),
                all_coins.clone(),
                Domain::Crypto,
            );
            info!(
                agent = crypto_cfg.agent_id,
                "market data persistence tasks started"
            );
        }

        // Optional Binance LOB depth stream (for ML/RL feature generation).
        let mut enable_binance_lob = lob_agent_enabled || rl_agent_enabled;
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
                match ensure_binance_lob_ticks_table(pool).await {
                    Ok(()) => {
                        if let Some(ph) = crypto_persistence_pipeline.clone() {
                            let rx = depth_stream.subscribe();
                            let agent_id = crypto_cfg.agent_id.clone();
                            let max_levels = env_usize("BN_LOB_LEVELS", 20).clamp(0, 200);
                            ph.spawn_bridge(
                                rx,
                                format!("{}.binance_lob", agent_id),
                                move |update| {
                                    let symbol = update.symbol.clone();
                                    let (bids, asks) = if max_levels == 0 {
                                        (Vec::new(), Vec::new())
                                    } else {
                                        (
                                            lob_levels_json(&update.raw_state, true, max_levels),
                                            lob_levels_json(&update.raw_state, false, max_levels),
                                        )
                                    };
                                    Some(crate::platform::PersistenceEvent::BinanceLob(
                                        crate::platform::BinanceLobTick {
                                            symbol,
                                            update_id: update.snapshot.update_id,
                                            best_bid: Some(update.snapshot.best_bid),
                                            best_ask: Some(update.snapshot.best_ask),
                                            mid_price: Some(update.snapshot.mid_price),
                                            spread_bps: Some(update.snapshot.spread_bps),
                                            obi_5: update.snapshot.obi_5.to_f64(),
                                            obi_10: update.snapshot.obi_10.to_f64(),
                                            bid_volume_5: Some(update.snapshot.bid_volume_5),
                                            ask_volume_5: Some(update.snapshot.ask_volume_5),
                                            bids: serde_json::to_value(&bids).unwrap_or_default(),
                                            asks: serde_json::to_value(&asks).unwrap_or_default(),
                                            event_time: update.snapshot.timestamp,
                                        },
                                    ))
                                },
                            );
                        } else {
                            warn!(
                                agent = crypto_cfg.agent_id,
                                "Binance LOB persistence requested but pipeline handle unavailable"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            agent = crypto_cfg.agent_id,
                            error = %e,
                            "failed to ensure binance_lob_ticks table; Binance LOB persistence bridge disabled"
                        );
                    }
                }
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

        if !use_data_plane {
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
        }

        if momentum_enabled {
            if momentum_symbols.is_empty() {
                warn!(
                    agent = crypto_cfg.agent_id,
                    "crypto momentum enabled but no recognized symbols were resolved"
                );
            } else {
                let strategy_agent_id = crypto_cfg.agent_id.clone();
                let runtime_spec =
                    build_momentum_managed_runtime_spec(&momentum_symbols, &crypto_cfg);
                if let Err(e) = spawn_managed_strategy_runtime_spec(
                    &mut agent_handles,
                    &mut coordinator,
                    &shutdown_tx,
                    runtime_spec,
                    crypto_cfg.risk_params.clone(),
                    config.dry_run,
                    pm_client.clone(),
                    &app_config.market.ws_url,
                    data_plane.clone(),
                    shared_pool.clone(),
                    &account_id,
                ) {
                    warn!(
                        agent = crypto_cfg.agent_id,
                        error = %e,
                        "crypto momentum enabled but canonical runtime could not be spawned; skipping"
                    );
                } else {
                    info!(agent = %strategy_agent_id, "crypto momentum strategy runtime spawned");
                }
            }
        } else {
            info!(
                agent = crypto_cfg.agent_id,
                "crypto momentum strategy runtime disabled"
            );
        }

        if pattern_memory_enabled {
            // Ensure persistence table exists
            if let Some(ref pool) = shared_pool {
                if let Err(e) =
                    crate::strategy::pattern_memory::persistence::ensure_table(pool).await
                {
                    warn!(error = %e, "failed to create pattern_memory_samples table");
                }
            }

            let mut coins: Vec<String> = if runtime_crypto_targets.pattern_memory_coins.is_empty() {
                crypto_cfg.coins.clone()
            } else {
                runtime_crypto_targets
                    .pattern_memory_coins
                    .iter()
                    .cloned()
                    .collect()
            };
            coins.sort();
            coins.dedup();

            match build_pattern_memory_managed_runtime_spec(&coins) {
                Ok(runtime_spec) => {
                    if let Err(e) = spawn_managed_strategy_runtime_spec(
                        &mut agent_handles,
                        &mut coordinator,
                        &shutdown_tx,
                        runtime_spec,
                        crypto_cfg.risk_params.clone(),
                        config.dry_run,
                        pm_client.clone(),
                        &app_config.market.ws_url,
                        data_plane.clone(),
                        shared_pool.clone(),
                        &account_id,
                    ) {
                        warn!(
                            agent = "pattern_memory",
                            error = %e,
                            "pattern_memory enabled but canonical runtime could not be spawned"
                        );
                    } else {
                        info!("pattern_memory strategy runtime spawned");
                    }
                }
                Err(e) => {
                    warn!(
                        agent = "pattern_memory",
                        error = %e,
                        "pattern_memory enabled but no valid runtime spec could be built"
                    );
                }
            }
        }

        if split_arb_enabled {
            let mut coins: Vec<String> = if runtime_crypto_targets.split_arb_coins.is_empty() {
                crypto_cfg.coins.clone()
            } else {
                runtime_crypto_targets
                    .split_arb_coins
                    .iter()
                    .cloned()
                    .collect()
            };
            coins.sort();
            coins.dedup();

            let mut horizons: Vec<String> = if runtime_crypto_targets.split_arb_horizons.is_empty()
            {
                vec!["5m".to_string(), "15m".to_string()]
            } else {
                runtime_crypto_targets
                    .split_arb_horizons
                    .iter()
                    .cloned()
                    .collect()
            };
            horizons.sort();
            horizons.dedup();

            let mut series_set: HashSet<String> = HashSet::new();
            for coin in &coins {
                let normalized = coin.trim_end_matches("USDT");
                for horizon in &horizons {
                    if let Some(series_id) = crypto_series_id_for(normalized, horizon) {
                        series_set.insert(series_id.to_string());
                    }
                }
            }
            let mut series_ids: Vec<String> = series_set.into_iter().collect();
            series_ids.sort();

            let mut symbols: Vec<String> = coins
                .iter()
                .filter_map(|coin| {
                    let normalized = coin.trim_end_matches("USDT");
                    coin_symbol_for(normalized)
                })
                .collect();
            symbols.sort();
            symbols.dedup();
            if symbols.is_empty() {
                symbols = series_ids
                    .iter()
                    .filter_map(|series_id| {
                        symbol_for_crypto_series_id(series_id).map(str::to_string)
                    })
                    .collect();
                symbols.sort();
                symbols.dedup();
            }

            if series_ids.is_empty() {
                warn!(
                    agent = "split_arb",
                    "split_arb enabled but no recognized coin/horizon series ids were resolved"
                );
            } else {
                let runtime_spec = build_split_arb_managed_runtime_spec(&symbols, &series_ids);
                if let Err(e) = spawn_managed_strategy_runtime_spec(
                    &mut agent_handles,
                    &mut coordinator,
                    &shutdown_tx,
                    runtime_spec,
                    crypto_cfg.risk_params.clone(),
                    config.dry_run,
                    pm_client.clone(),
                    &app_config.market.ws_url,
                    data_plane.clone(),
                    shared_pool.clone(),
                    &account_id,
                ) {
                    warn!(
                        agent = "split_arb",
                        error = %e,
                        "split_arb enabled but canonical runtime could not be spawned"
                    );
                } else {
                    info!("split_arb strategy runtime spawned");
                }
            }
        }

        if lob_agent_enabled {
            let model_type = lob_cfg.model_type.trim().to_ascii_lowercase();
            let model_is_tcn = matches!(
                model_type.as_str(),
                "onnx_tcn" | "tcn" | "tcn_onnx" | "tcn-onnx"
            );

            if model_is_tcn && !cfg!(feature = "onnx") {
                warn!(
                    agent = lob_cfg.agent_id,
                    model_type = %model_type,
                    "crypto lob-ml agent model_type=onnx_tcn requires --features onnx; skipping agent spawn"
                );
            } else if model_is_tcn && shared_pool.is_none() {
                warn!(
                    agent = lob_cfg.agent_id,
                    model_type = %model_type,
                    "crypto lob-ml agent model_type=onnx_tcn requires DB for feature parity with training; skipping agent spawn"
                );
            } else if !model_is_tcn && lob_cache_opt.is_none() {
                warn!(
                    agent = lob_cfg.agent_id,
                    model_type = %model_type,
                    "crypto lob-ml agent requires binance depth stream but it is disabled; skipping agent spawn"
                );
            } else {
                if let Some(lob_cache) = lob_cache_opt.clone() {
                    let risk_params = lob_cfg.risk_params.clone();
                    let agent = CryptoLobMlAgent::new(
                        lob_cfg.clone(),
                        crypto_market_data.clone(),
                        event_matcher.clone(),
                        lob_cache,
                    )?;
                    spawn_trading_agent_task(
                        &mut agent_handles,
                        &mut coordinator,
                        &handle,
                        agent,
                        risk_params,
                        "crypto_lob_ml",
                    );
                    info!("crypto lob-ml agent spawned");
                } else {
                    warn!(
                        agent = lob_cfg.agent_id,
                        model_type = %model_type,
                        "crypto lob-ml agent requires binance depth stream but it is disabled; skipping agent spawn"
                    );
                }
            }
        }

        #[cfg(feature = "rl")]
        if rl_agent_enabled {
            if let Some(lob_cache) = lob_cache_opt.clone() {
                let risk_params = rl_cfg.risk_params.clone();
                let agent = CryptoRlPolicyAgent::new(
                    rl_cfg.clone(),
                    crypto_market_data.clone(),
                    event_matcher.clone(),
                    lob_cache,
                );
                spawn_trading_agent_task(
                    &mut agent_handles,
                    &mut coordinator,
                    &handle,
                    agent,
                    risk_params,
                    "crypto_rl_policy",
                );
                info!("crypto RL policy agent spawned");
            } else {
                warn!(
                    agent = rl_cfg.agent_id,
                    "RL policy agent enabled but binance depth stream is disabled; skipping agent spawn"
                );
            }
        }
    }

    if config.enable_sports {
        if let Some(ref nba_cfg) = app_config.nba_comeback {
            let sports_cfg = config.sports.clone();
            let managed_runtime_spec = build_nba_comeback_managed_runtime_spec(
                &app_config.database.url,
                &sports_cfg,
                nba_cfg,
            );

            let pool = start_sports_market_data_support(
                shared_pool.clone(),
                app_config,
                Arc::clone(&freshness),
                &sports_cfg,
            )
            .await?;

            if let Some(runtime_spec) = managed_runtime_spec {
                spawn_managed_strategy_runtime_spec(
                    &mut agent_handles,
                    &mut coordinator,
                    &shutdown_tx,
                    runtime_spec,
                    sports_cfg.risk_params.clone(),
                    config.dry_run,
                    pm_client.clone(),
                    &app_config.market.ws_url,
                    None,
                    Some(pool.clone()),
                    &account_id,
                )?;
                info!(
                    agent = %sports_cfg.agent_id,
                    "sports nba_comeback strategy runtime spawned"
                );
            } else {
                spawn_legacy_nba_comeback_agent(
                    &mut agent_handles,
                    &mut coordinator,
                    &handle,
                    sports_cfg.clone(),
                    nba_cfg.clone(),
                    pool,
                );
            }
        }
    }

    if config.enable_politics {
        if let Some(ref ee_cfg) = app_config.event_edge_agent {
            let politics_cfg = config.politics.clone();
            let strategy_agent_id = politics_cfg.agent_id.clone();
            let runtime_spec =
                build_event_edge_managed_runtime_spec(&app_config.market.rest_url, &politics_cfg, ee_cfg);
            spawn_managed_strategy_runtime_spec(
                &mut agent_handles,
                &mut coordinator,
                &shutdown_tx,
                runtime_spec,
                politics_cfg.risk_params.clone(),
                config.dry_run,
                pm_client.clone(),
                &app_config.market.ws_url,
                None,
                shared_pool.clone(),
                &account_id,
            )?;
            info!(agent = %strategy_agent_id, "politics event_edge strategy runtime spawned");
        }
    }

    // --- OpenClaw meta-agent (Layer 3 orchestrator) ---
    let openclaw_enabled = env_bool(
        "PLOY_OPENCLAW__ENABLED",
        config.enable_openclaw || config.openclaw.enabled,
    );
    if openclaw_enabled {
        // OpenClaw needs a BinanceWebSocket for regime detection.
        // If crypto is enabled, a binance_ws was already created above and lives in a local scope.
        // We create a dedicated one for OpenClaw using the configured BTC symbol.
        let oc_symbols = vec![config.openclaw.btc_symbol.clone()];
        let oc_binance_ws = Arc::new(BinanceWebSocket::new(oc_symbols));
        oc_binance_ws.set_freshness(Arc::clone(&freshness));

        // Spawn Binance WS feed for OpenClaw
        let oc_ws = oc_binance_ws.clone();
        tokio::spawn(async move {
            if let Err(e) = oc_ws.run().await {
                tracing::error!(error = %e, "openclaw binance ws exited");
            }
        });

        let oc_risk_params = AgentRiskParams {
            max_order_value: Decimal::ZERO,
            max_total_exposure: Decimal::ZERO,
            max_unhedged_positions: 0,
            max_daily_loss: Decimal::ZERO,
            allow_overnight: false,
            allowed_markets: vec![],
        };
        let oc_agent_id = config.openclaw.agent_id.clone();
        let oc_market_data = BinanceDataPlaneHandle::new(oc_binance_ws.clone());
        let agent = OpenClawAgent::new(config.openclaw.clone(), oc_market_data);
        spawn_trading_agent_task(
            &mut agent_handles,
            &mut coordinator,
            &handle,
            agent,
            oc_risk_params,
            "openclaw",
        );
        info!(
            agent_id = %oc_agent_id,
            regime_tick = config.openclaw.regime_tick_secs,
            "openclaw meta-agent spawned"
        );
    }

    // ── Auto-claimer background task ──
    // Ensure single account-level daemon (deduped) and avoid spawning a second
    // direct AutoClaimer loop in platform bootstrap.
    #[cfg(feature = "claimer_daemon")]
    if !config.dry_run && pm_client.is_some() {
        if let Err(e) = crate::strategy::ensure_account_claimer_daemon().await {
            warn!(error = %e, "failed to ensure account-level auto-claimer daemon");
        } else {
            info!("auto-claimer background task ensured (account-level)");
        }
    }

    #[cfg(not(feature = "claimer_daemon"))]
    if pm_client.is_some() {
        info!("claimer feature disabled; skipping auto-claimer background task");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{
        DeploymentExecutionMode, StrategyLifecycleStage, StrategyProductType, Timeframe,
    };
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn set_env(key: &str, value: Option<&str>) {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    fn economics_deployment(enabled: bool) -> StrategyDeployment {
        StrategyDeployment {
            id: "deploy.econ.fed.15m".to_string(),
            strategy: "macro_regime".to_string(),
            strategy_version: "v1".to_string(),
            domain: Domain::Economics,
            market_selector: MarketSelector::Static {
                symbol: None,
                series_id: None,
                market_slug: Some("fed-rate-15m".to_string()),
            },
            timeframe: Timeframe::M15,
            enabled,
            allocator_profile: "default".to_string(),
            risk_profile: "default".to_string(),
            priority: 0,
            cooldown_secs: 60,
            account_ids: Vec::new(),
            execution_mode: DeploymentExecutionMode::Any,
            lifecycle_stage: StrategyLifecycleStage::Live,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: None,
            last_evaluation_score: None,
        }
    }

    fn crypto_deployment(strategy: &str, enabled: bool) -> StrategyDeployment {
        StrategyDeployment {
            id: format!("deploy.crypto.{strategy}.5m"),
            strategy: strategy.to_string(),
            strategy_version: "v1".to_string(),
            domain: Domain::Crypto,
            market_selector: MarketSelector::Static {
                symbol: Some("BTCUSDT".to_string()),
                series_id: None,
                market_slug: None,
            },
            timeframe: Timeframe::M5,
            enabled,
            allocator_profile: "default".to_string(),
            risk_profile: "default".to_string(),
            priority: 0,
            cooldown_secs: 60,
            account_ids: Vec::new(),
            execution_mode: DeploymentExecutionMode::Any,
            lifecycle_stage: StrategyLifecycleStage::Live,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: None,
            last_evaluation_score: None,
        }
    }

    #[test]
    fn apply_strategy_deployments_enables_economics_domain() {
        let mut cfg = PlatformBootstrapConfig::default();
        let deployments = vec![economics_deployment(true)];

        apply_strategy_deployments(&mut cfg, &deployments, "default", false);

        assert!(cfg.enable_economics);
        assert!(!cfg.enable_crypto);
        assert!(!cfg.enable_sports);
        assert!(!cfg.enable_politics);
    }

    #[test]
    fn apply_strategy_deployments_ignores_disabled_economics_domain() {
        let mut cfg = PlatformBootstrapConfig::default();
        let deployments = vec![economics_deployment(false)];

        apply_strategy_deployments(&mut cfg, &deployments, "default", false);

        assert!(!cfg.enable_economics);
    }

    #[test]
    fn apply_strategy_deployments_does_not_route_unknown_crypto_strategy_to_momentum() {
        let mut cfg = PlatformBootstrapConfig::default();
        let deployments = vec![crypto_deployment("totally_new_crypto_strategy", true)];

        apply_strategy_deployments(&mut cfg, &deployments, "default", false);

        assert!(
            !cfg.enable_crypto,
            "unknown crypto strategy should not auto-enable crypto domain"
        );
        assert!(
            !cfg.enable_crypto_momentum,
            "unknown crypto strategy must not auto-route to momentum"
        );
        assert!(!cfg.enable_crypto_pattern_memory);
        assert!(!cfg.enable_crypto_split_arb);
        assert!(!cfg.enable_crypto_lob_ml);
    }

    #[test]
    fn apply_strategy_deployments_maps_gamma_scalping_alias_to_split_arb() {
        let mut cfg = PlatformBootstrapConfig::default();
        let deployments = vec![crypto_deployment("gamma_scalping", true)];

        apply_strategy_deployments(&mut cfg, &deployments, "default", false);

        assert!(cfg.enable_crypto);
        assert!(cfg.enable_crypto_split_arb);
        assert!(!cfg.enable_crypto_momentum);
    }

    #[test]
    fn apply_strategy_deployments_enables_crypto_momentum_domain() {
        let mut cfg = PlatformBootstrapConfig::default();
        let deployments = vec![crypto_deployment("momentum", true)];

        apply_strategy_deployments(&mut cfg, &deployments, "default", false);

        assert!(cfg.enable_crypto);
        assert!(cfg.enable_crypto_momentum);
        assert!(!cfg.enable_crypto_split_arb);
    }

    #[test]
    fn build_momentum_runtime_config_overrides_template_symbols() {
        let _guard = ENV_LOCK.lock().unwrap();

        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let rendered = build_momentum_runtime_config(&symbols, &CryptoTradingConfig::default());

        assert!(rendered.contains("symbols = [\"BTCUSDT\", \"ETHUSDT\"]"));
        assert!(
            !rendered.contains("symbols = [\"BTCUSDT\", \"ETHUSDT\", \"SOLUSDT\", \"XRPUSDT\"]"),
            "managed momentum runtime should replace template symbols with deployment-scoped symbols"
        );
        assert!(rendered.contains("name = \"momentum\""));
    }

    #[test]
    fn build_momentum_runtime_config_projects_legacy_crypto_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();

        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let rendered = build_momentum_runtime_config(&symbols, &CryptoTradingConfig::default());

        assert!(rendered.contains("min_move = 0.1"));
        assert!(rendered.contains("min_edge = 2.0"));
        assert!(rendered.contains("min_time_remaining = 60"));
        assert!(rendered.contains("max_time_remaining = 300"));
        assert!(rendered.contains("cooldown_secs = 0"));
        assert!(rendered.contains("shares = 100"));
        assert!(rendered.contains("max_positions = 2"));
        assert!(rendered.contains("max_window_exposure = 100.0"));
        assert!(rendered.contains("exit_edge_floor_pct = 2.0"));
        assert!(rendered.contains("exit_price_band_pct = 5.0"));
        assert!(rendered.contains("require_mtf_agreement = true"));
        assert!(rendered.contains("directional_mode = true"));
        assert!(rendered.contains("directional_entry_threshold = 2.0"));
        assert!(
            !rendered.contains("min_time_remaining = 300.0"),
            "legacy crypto timing should override template defaults"
        );
    }

    #[test]
    fn build_momentum_managed_runtime_spec_projects_canonical_launch() {
        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let crypto_cfg = CryptoTradingConfig::default();

        let spec = build_momentum_managed_runtime_spec(&symbols, &crypto_cfg);

        assert_eq!(spec.strategy_label, "momentum");
        assert_eq!(spec.agent_id, crypto_cfg.agent_id);
        assert_eq!(spec.domain, Domain::Crypto);
        assert!(spec.strategy_config_toml.contains("name = \"momentum\""));
    }

    #[test]
    fn build_event_edge_runtime_config_projects_targets_and_limits() {
        let cfg = crate::config::EventEdgeAgentConfig {
            enabled: true,
            framework: "deterministic".to_string(),
            event_ids: vec!["evt-1".to_string()],
            titles: vec!["Best AI model".to_string()],
            interval_secs: 180,
            min_edge: Decimal::new(8, 2),
            max_entry: Decimal::new(70, 2),
            shares: 25,
            trade: true,
            cooldown_secs: 120,
            max_daily_spend_usd: Decimal::from(55),
            model: None,
            claude_max_turns: 0,
        };

        let rendered = build_event_edge_runtime_config("https://clob.polymarket.com", &cfg);

        assert!(rendered.contains("name = \"event_edge\""));
        assert!(rendered.contains("event_ids = [\"evt-1\"]"));
        assert!(rendered.contains("titles = [\"Best AI model\"]"));
        assert!(rendered.contains("poll_interval_secs = 180"));
        assert!(rendered.contains("cooldown_secs = 120"));
        assert!(rendered.contains("max_daily_spend_usd = 55.0"));
        assert!(rendered.contains("rest_url = \"https://clob.polymarket.com\""));
    }

    #[test]
    fn build_event_edge_managed_runtime_spec_projects_canonical_launch() {
        let politics_cfg = PoliticsTradingConfig::default();
        let cfg = crate::config::EventEdgeAgentConfig {
            enabled: true,
            framework: "deterministic".to_string(),
            event_ids: vec!["evt-1".to_string()],
            titles: vec!["Best AI model".to_string()],
            interval_secs: 180,
            min_edge: Decimal::new(8, 2),
            max_entry: Decimal::new(70, 2),
            shares: 25,
            trade: true,
            cooldown_secs: 120,
            max_daily_spend_usd: Decimal::from(55),
            model: None,
            claude_max_turns: 0,
        };

        let spec = build_event_edge_managed_runtime_spec(
            "https://clob.polymarket.com",
            &politics_cfg,
            &cfg,
        );

        assert_eq!(spec.strategy_label, "event_edge");
        assert_eq!(spec.agent_id, politics_cfg.agent_id);
        assert_eq!(spec.domain, Domain::Politics);
        assert!(spec.strategy_config_toml.contains("name = \"event_edge\""));
        assert!(spec
            .strategy_config_toml
            .contains("rest_url = \"https://clob.polymarket.com\""));
    }

    #[test]
    fn build_nba_comeback_runtime_config_projects_strategy_fields() {
        let cfg = crate::config::NbaComebackConfig {
            enabled: true,
            min_edge: Decimal::new(7, 2),
            max_entry_price: Decimal::new(68, 2),
            shares: 25,
            cooldown_secs: 180,
            max_daily_spend_usd: Decimal::from(55),
            min_deficit: 4,
            max_deficit: 12,
            target_quarter: 3,
            espn_poll_interval_secs: 45,
            min_comeback_rate: 0.22,
            season: "2026-27".to_string(),
            grok_enabled: true,
            grok_interval_secs: 600,
            grok_min_edge: Decimal::new(9, 2),
            grok_min_confidence: 0.72,
            grok_decision_cooldown_secs: 90,
            grok_fallback_enabled: false,
            min_reward_risk_ratio: 5.0,
            min_expected_value: 0.08,
            kelly_fraction_cap: 0.20,
            performance_daily_loss_limit_usd: Decimal::from(25),
            performance_min_settled_trades: 12,
            performance_min_win_rate: 0.48,
            performance_low_winrate_multiplier: 0.55,
            performance_loss_streak_threshold: 4,
            performance_loss_streak_multiplier: 0.40,
            scaling_enabled: true,
            scaling_max_adds: 2,
            scaling_min_price_drop_pct: 7.5,
            scaling_max_game_exposure_usd: Decimal::from(42),
            scaling_min_comeback_retention: 0.75,
            scaling_min_time_remaining_mins: 9.5,
            early_exit_enabled: true,
            early_exit_take_profit_pct: 18.0,
            early_exit_stop_loss_pct: 16.0,
        };

        let rendered =
            build_nba_comeback_runtime_config("postgres://db.example.com/ploy", &cfg);

        assert!(rendered.contains("name = \"nba_comeback\""));
        assert!(rendered.contains("poll_interval_secs = 45"));
        assert!(rendered.contains("min_edge = 0.07"));
        assert!(rendered.contains("max_entry_price = 0.68"));
        assert!(rendered.contains("cooldown_secs = 180"));
        assert!(rendered.contains("max_daily_spend_usd = 55.0"));
        assert!(rendered.contains("min_deficit = 4"));
        assert!(rendered.contains("max_deficit = 12"));
        assert!(rendered.contains("season = \"2026-27\""));
        assert!(rendered.contains("url = \"postgres://db.example.com/ploy\""));
        assert!(rendered.contains("enabled = true"));
        assert!(rendered.contains("interval_secs = 600"));
        assert!(rendered.contains("decision_cooldown_secs = 90"));
        assert!(rendered.contains("max_adds = 2"));
        assert!(rendered.contains("take_profit_pct = 18.0"));
        assert!(rendered.contains("stop_loss_pct = 16.0"));
    }

    fn sample_nba_comeback_config(grok_enabled: bool) -> crate::config::NbaComebackConfig {
        crate::config::NbaComebackConfig {
            enabled: true,
            min_edge: Decimal::new(7, 2),
            max_entry_price: Decimal::new(68, 2),
            shares: 25,
            cooldown_secs: 180,
            max_daily_spend_usd: Decimal::from(55),
            min_deficit: 4,
            max_deficit: 12,
            target_quarter: 3,
            espn_poll_interval_secs: 45,
            min_comeback_rate: 0.22,
            season: "2026-27".to_string(),
            grok_enabled,
            grok_interval_secs: 600,
            grok_min_edge: Decimal::new(9, 2),
            grok_min_confidence: 0.72,
            grok_decision_cooldown_secs: 90,
            grok_fallback_enabled: false,
            min_reward_risk_ratio: 5.0,
            min_expected_value: 0.08,
            kelly_fraction_cap: 0.20,
            performance_daily_loss_limit_usd: Decimal::from(25),
            performance_min_settled_trades: 12,
            performance_min_win_rate: 0.48,
            performance_low_winrate_multiplier: 0.55,
            performance_loss_streak_threshold: 4,
            performance_loss_streak_multiplier: 0.40,
            scaling_enabled: true,
            scaling_max_adds: 2,
            scaling_min_price_drop_pct: 7.5,
            scaling_max_game_exposure_usd: Decimal::from(42),
            scaling_min_comeback_retention: 0.75,
            scaling_min_time_remaining_mins: 9.5,
            early_exit_enabled: true,
            early_exit_take_profit_pct: 18.0,
            early_exit_stop_loss_pct: 16.0,
        }
    }

    #[test]
    fn build_nba_comeback_managed_runtime_spec_projects_canonical_launch() {
        let sports_cfg = SportsTradingConfig::default();
        let nba_cfg = sample_nba_comeback_config(false);

        let spec = build_nba_comeback_managed_runtime_spec(
            "postgres://db.example.com/ploy",
            &sports_cfg,
            &nba_cfg,
        )
        .expect("managed nba runtime spec");

        assert_eq!(spec.strategy_label, "nba_comeback");
        assert_eq!(spec.agent_id, sports_cfg.agent_id);
        assert_eq!(spec.domain, Domain::Sports);
        assert!(spec.strategy_config_toml.contains("name = \"nba_comeback\""));
        assert!(spec.strategy_config_toml.contains("poll_interval_secs = 45"));
        assert!(spec
            .strategy_config_toml
            .contains("url = \"postgres://db.example.com/ploy\""));
    }

    #[test]
    fn build_nba_comeback_managed_runtime_spec_defers_grok_enabled_configs() {
        let sports_cfg = SportsTradingConfig::default();
        let nba_cfg = sample_nba_comeback_config(true);

        assert!(
            build_nba_comeback_managed_runtime_spec(
                "postgres://db.example.com/ploy",
                &sports_cfg,
                &nba_cfg,
            )
            .is_none(),
            "grok-enabled sports configs should stay on the legacy agent path until the canonical strategy bridge absorbs Grok behavior"
        );
    }

    #[test]
    fn build_split_arb_runtime_config_renders_symbols_and_series_ids() {
        let _guard = ENV_LOCK.lock().unwrap();

        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let series_ids = vec!["10192".to_string(), "10684".to_string()];
        let rendered = build_split_arb_runtime_config(&symbols, &series_ids);

        assert!(rendered.contains("symbols = [\"BTCUSDT\", \"ETHUSDT\"]"));
        assert!(rendered.contains("series_ids = [\"10192\", \"10684\"]"));
        assert!(rendered.contains("shares_per_trade = 20"));
        assert!(
            !rendered.contains("fixed_amount_usd"),
            "managed staggered_arb runtime should honor share sizing defaults"
        );
    }

    #[test]
    fn build_split_arb_runtime_config_overrides_template_symbols_and_series_ids() {
        let _guard = ENV_LOCK.lock().unwrap();

        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let series_ids = vec!["10192".to_string(), "10684".to_string()];
        let rendered = build_split_arb_runtime_config(&symbols, &series_ids);

        assert!(rendered.contains("symbols = [\"BTCUSDT\", \"ETHUSDT\"]"));
        assert!(
            !rendered.contains("symbols = [\"BTCUSDT\", \"ETHUSDT\", \"SOLUSDT\"]"),
            "managed runtime should replace template symbols with deployment-scoped symbols"
        );
        assert!(rendered.contains("series_ids = [\"10192\", \"10684\"]"));
    }

    #[test]
    fn build_split_arb_runtime_config_overrides_external_symbols_and_series_ids() {
        let _guard = ENV_LOCK.lock().unwrap();

        let env_key = "PLOY_STAGGERED_ARB_CONFIG";
        let prev = std::env::var(env_key).ok();
        let temp_path = std::env::temp_dir().join(format!(
            "ploy-stag-arb-{}.toml",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(
            &temp_path,
            r#"[strategy]
name = "staggered_arb"

[entry]
symbols = ["SOLUSDT"]
"#,
        )
        .expect("write temp staggered arb config");
        std::env::set_var(env_key, &temp_path);

        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let series_ids = vec!["10192".to_string(), "10684".to_string()];
        let rendered = build_split_arb_runtime_config(&symbols, &series_ids);

        match prev {
            Some(v) => std::env::set_var(env_key, v),
            None => std::env::remove_var(env_key),
        }
        let _ = std::fs::remove_file(&temp_path);

        assert!(rendered.contains("[entry]\nsymbols = [\"BTCUSDT\", \"ETHUSDT\"]"));
        assert!(rendered.contains("[markets]\nseries_ids = [\"10192\", \"10684\"]"));
    }

    #[test]
    fn build_split_arb_managed_runtime_spec_projects_canonical_launch() {
        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let series_ids = vec!["10192".to_string(), "10684".to_string()];

        let spec = build_split_arb_managed_runtime_spec(&symbols, &series_ids);

        assert_eq!(spec.strategy_label, "split_arb");
        assert_eq!(spec.agent_id, "split_arb");
        assert_eq!(spec.domain, Domain::Crypto);
        assert!(spec.strategy_config_toml.contains("enabled = true"));
        assert!(spec
            .strategy_config_toml
            .contains("symbols = [\"BTCUSDT\", \"ETHUSDT\"]"));
    }

    #[tokio::test]
    async fn ensure_pm_market_metadata_table_exists() {
        let _guard = ENV_LOCK.lock().unwrap();

        let db_url = std::env::var("PLOY_TEST_DATABASE_URL")
            .ok()
            .or_else(|| std::env::var("DATABASE_URL").ok());
        let Some(db_url) = db_url else {
            return;
        };

        let pool = match PgPoolOptions::new()
            .max_connections(1)
            .connect(&db_url)
            .await
        {
            Ok(pool) => pool,
            Err(_) => return,
        };

        ensure_pm_market_metadata_table(&pool)
            .await
            .expect("ensure table");

        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT to_regclass('public.pm_market_metadata') IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .expect("check relation exists");
        assert!(exists);

        let cols = sqlx::query_scalar::<_, String>(
            r#"
            SELECT column_name
            FROM information_schema.columns
            WHERE table_schema = 'public' AND table_name = 'pm_market_metadata'
            "#,
        )
        .fetch_all(&pool)
        .await
        .expect("read table columns");

        for col in [
            "market_slug",
            "price_to_beat",
            "start_time",
            "end_time",
            "horizon",
            "symbol",
            "raw_market",
            "updated_at",
        ] {
            assert!(
                cols.iter().any(|c| c == col),
                "missing pm_market_metadata column: {col}"
            );
        }
    }

    #[test]
    fn from_app_config_reads_crypto_lob_ml_model_env_vars() {
        let _guard = ENV_LOCK.lock().unwrap();

        let model_type_key = "PLOY_CRYPTO_LOB_ML__MODEL_TYPE";
        let model_path_key = "PLOY_CRYPTO_LOB_ML__MODEL_PATH";
        let model_version_key = "PLOY_CRYPTO_LOB_ML__MODEL_VERSION";
        let blend_weight_key = "PLOY_CRYPTO_LOB_ML__MODEL_BLEND_WEIGHT";
        let min_direction_strength_key = "PLOY_CRYPTO_LOB_ML__MIN_DIRECTION_STRENGTH";
        let ev_exit_buffer_key = "PLOY_CRYPTO_LOB_ML__EV_EXIT_BUFFER";
        let ev_exit_vol_scale_key = "PLOY_CRYPTO_LOB_ML__EV_EXIT_VOL_SCALE";
        let taker_fee_key = "PLOY_CRYPTO_LOB_ML__TAKER_FEE_RATE";
        let slippage_key = "PLOY_CRYPTO_LOB_ML__ENTRY_SLIPPAGE_BPS";
        let use_threshold_key = "PLOY_CRYPTO_LOB_ML__USE_PRICE_TO_BEAT";
        let require_threshold_key = "PLOY_CRYPTO_LOB_ML__REQUIRE_PRICE_TO_BEAT";
        let exit_mode_key = "PLOY_CRYPTO_LOB_ML__EXIT_MODE";
        let entry_side_policy_key = "PLOY_CRYPTO_LOB_ML__ENTRY_SIDE_POLICY";
        let entry_late_window_5m_key = "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M";
        let entry_late_window_15m_key = "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_15M";

        let prev_model_type = std::env::var(model_type_key).ok();
        let prev_model_path = std::env::var(model_path_key).ok();
        let prev_model_version = std::env::var(model_version_key).ok();
        let prev_blend_weight = std::env::var(blend_weight_key).ok();
        let prev_min_direction_strength = std::env::var(min_direction_strength_key).ok();
        let prev_ev_exit_buffer = std::env::var(ev_exit_buffer_key).ok();
        let prev_ev_exit_vol_scale = std::env::var(ev_exit_vol_scale_key).ok();
        let prev_taker_fee = std::env::var(taker_fee_key).ok();
        let prev_slippage = std::env::var(slippage_key).ok();
        let prev_use_threshold = std::env::var(use_threshold_key).ok();
        let prev_require_threshold = std::env::var(require_threshold_key).ok();
        let prev_exit_mode = std::env::var(exit_mode_key).ok();
        let prev_entry_side_policy = std::env::var(entry_side_policy_key).ok();
        let prev_entry_late_window_5m = std::env::var(entry_late_window_5m_key).ok();
        let prev_entry_late_window_15m = std::env::var(entry_late_window_15m_key).ok();

        set_env(model_type_key, Some("onnx"));
        set_env(model_path_key, Some("/tmp/models/lob_tcn_v2.onnx"));
        set_env(model_version_key, Some("lob_tcn_v2"));
        set_env(blend_weight_key, Some("0.75"));
        set_env(min_direction_strength_key, Some("0.06"));
        set_env(ev_exit_buffer_key, Some("0.01"));
        set_env(ev_exit_vol_scale_key, Some("0.03"));
        set_env(taker_fee_key, Some("0.03"));
        set_env(slippage_key, Some("12"));
        set_env(use_threshold_key, Some("true"));
        set_env(require_threshold_key, Some("false"));
        set_env(exit_mode_key, Some("ev_exit"));
        set_env(entry_side_policy_key, Some("lagging_only"));
        set_env(entry_late_window_5m_key, Some("170"));
        set_env(entry_late_window_15m_key, Some("180"));

        let app = AppConfig::default_config(true, "btc-up-or-down-test");
        let cfg = PlatformBootstrapConfig::from_app_config(&app);

        assert_eq!(cfg.crypto_lob_ml.model_type, "onnx");
        assert_eq!(
            cfg.crypto_lob_ml.model_path.as_deref(),
            Some("/tmp/models/lob_tcn_v2.onnx")
        );
        assert_eq!(
            cfg.crypto_lob_ml.model_version.as_deref(),
            Some("lob_tcn_v2")
        );
        assert_eq!(
            cfg.crypto_lob_ml.model_blend_weight,
            rust_decimal::Decimal::new(75, 2)
        );
        assert_eq!(
            cfg.crypto_lob_ml.min_direction_strength,
            rust_decimal::Decimal::new(6, 2)
        );
        assert_eq!(
            cfg.crypto_lob_ml.ev_exit_buffer,
            rust_decimal::Decimal::new(1, 2)
        );
        assert_eq!(
            cfg.crypto_lob_ml.ev_exit_vol_scale,
            rust_decimal::Decimal::new(3, 2)
        );
        assert_eq!(
            cfg.crypto_lob_ml.taker_fee_rate,
            rust_decimal::Decimal::new(3, 2)
        );
        assert_eq!(
            cfg.crypto_lob_ml.entry_slippage_bps,
            rust_decimal::Decimal::new(12, 0)
        );
        assert!(cfg.crypto_lob_ml.use_price_to_beat);
        assert!(!cfg.crypto_lob_ml.require_price_to_beat);
        assert_eq!(cfg.crypto_lob_ml.exit_mode, CryptoLobMlExitMode::EvExit);
        assert_eq!(
            cfg.crypto_lob_ml.entry_side_policy,
            CryptoLobMlEntrySidePolicy::LaggingOnly
        );
        assert_eq!(cfg.crypto_lob_ml.entry_late_window_secs_5m, 170);
        assert_eq!(cfg.crypto_lob_ml.entry_late_window_secs_15m, 180);

        match prev_model_type.as_deref() {
            Some(v) => set_env(model_type_key, Some(v)),
            None => set_env(model_type_key, None),
        }
        match prev_model_path.as_deref() {
            Some(v) => set_env(model_path_key, Some(v)),
            None => set_env(model_path_key, None),
        }
        match prev_model_version.as_deref() {
            Some(v) => set_env(model_version_key, Some(v)),
            None => set_env(model_version_key, None),
        }
        match prev_blend_weight.as_deref() {
            Some(v) => set_env(blend_weight_key, Some(v)),
            None => set_env(blend_weight_key, None),
        }
        match prev_min_direction_strength.as_deref() {
            Some(v) => set_env(min_direction_strength_key, Some(v)),
            None => set_env(min_direction_strength_key, None),
        }
        match prev_ev_exit_buffer.as_deref() {
            Some(v) => set_env(ev_exit_buffer_key, Some(v)),
            None => set_env(ev_exit_buffer_key, None),
        }
        match prev_ev_exit_vol_scale.as_deref() {
            Some(v) => set_env(ev_exit_vol_scale_key, Some(v)),
            None => set_env(ev_exit_vol_scale_key, None),
        }
        match prev_taker_fee.as_deref() {
            Some(v) => set_env(taker_fee_key, Some(v)),
            None => set_env(taker_fee_key, None),
        }
        match prev_slippage.as_deref() {
            Some(v) => set_env(slippage_key, Some(v)),
            None => set_env(slippage_key, None),
        }
        match prev_use_threshold.as_deref() {
            Some(v) => set_env(use_threshold_key, Some(v)),
            None => set_env(use_threshold_key, None),
        }
        match prev_require_threshold.as_deref() {
            Some(v) => set_env(require_threshold_key, Some(v)),
            None => set_env(require_threshold_key, None),
        }
        match prev_exit_mode.as_deref() {
            Some(v) => set_env(exit_mode_key, Some(v)),
            None => set_env(exit_mode_key, None),
        }
        match prev_entry_side_policy.as_deref() {
            Some(v) => set_env(entry_side_policy_key, Some(v)),
            None => set_env(entry_side_policy_key, None),
        }
        match prev_entry_late_window_5m.as_deref() {
            Some(v) => set_env(entry_late_window_5m_key, Some(v)),
            None => set_env(entry_late_window_5m_key, None),
        }
        match prev_entry_late_window_15m.as_deref() {
            Some(v) => set_env(entry_late_window_15m_key, Some(v)),
            None => set_env(entry_late_window_15m_key, None),
        }
    }

    #[test]
    fn from_app_config_reads_crypto_agent_signal_gate_env_vars() {
        let _guard = ENV_LOCK.lock().unwrap();

        let min_momentum_key = "PLOY_CRYPTO_AGENT__MIN_MOMENTUM_1S";
        let min_window_move_key = "PLOY_CRYPTO_AGENT__MIN_WINDOW_MOVE_PCT";
        let min_signal_score_key = "PLOY_CRYPTO_AGENT__MIN_SIGNAL_SCORE";

        let prev_min_momentum = std::env::var(min_momentum_key).ok();
        let prev_min_window_move = std::env::var(min_window_move_key).ok();
        let prev_min_signal_score = std::env::var(min_signal_score_key).ok();

        set_env(min_momentum_key, Some("0.0025"));
        set_env(min_window_move_key, Some("0.0015"));
        set_env(min_signal_score_key, Some("0.65"));

        let app = AppConfig::default_config(true, "btc-up-or-down-test");
        let cfg = PlatformBootstrapConfig::from_app_config(&app);

        assert!((cfg.crypto.min_momentum_1s - 0.0025).abs() < f64::EPSILON);
        assert_eq!(
            cfg.crypto.min_window_move_pct,
            rust_decimal::Decimal::new(15, 4)
        );
        assert_eq!(
            cfg.crypto.min_signal_score,
            rust_decimal::Decimal::new(65, 2)
        );

        match prev_min_momentum.as_deref() {
            Some(v) => set_env(min_momentum_key, Some(v)),
            None => set_env(min_momentum_key, None),
        }
        match prev_min_window_move.as_deref() {
            Some(v) => set_env(min_window_move_key, Some(v)),
            None => set_env(min_window_move_key, None),
        }
        match prev_min_signal_score.as_deref() {
            Some(v) => set_env(min_signal_score_key, Some(v)),
            None => set_env(min_signal_score_key, None),
        }
    }

    #[test]
    fn from_app_config_ignores_legacy_enable_price_exits_env() {
        let _guard = ENV_LOCK.lock().unwrap();

        let exit_mode_key = "PLOY_CRYPTO_LOB_ML__EXIT_MODE";
        let legacy_price_exits_key = "PLOY_CRYPTO_LOB_ML__ENABLE_PRICE_EXITS";

        let prev_exit_mode = std::env::var(exit_mode_key).ok();
        let prev_legacy_price_exits = std::env::var(legacy_price_exits_key).ok();

        set_env(exit_mode_key, None);
        set_env(legacy_price_exits_key, Some("true"));

        let app = AppConfig::default_config(true, "btc-up-or-down-test");
        let cfg = PlatformBootstrapConfig::from_app_config(&app);

        assert_eq!(cfg.crypto_lob_ml.exit_mode, CryptoLobMlExitMode::EvExit);

        match prev_exit_mode.as_deref() {
            Some(v) => set_env(exit_mode_key, Some(v)),
            None => set_env(exit_mode_key, None),
        }
        match prev_legacy_price_exits.as_deref() {
            Some(v) => set_env(legacy_price_exits_key, Some(v)),
            None => set_env(legacy_price_exits_key, None),
        }
    }
}
