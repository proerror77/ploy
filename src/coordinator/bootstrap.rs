//! Platform Bootstrap — wires up Coordinator + Agents from config
//!
//! Entry point for `ploy platform start`. Creates shared infrastructure,
//! registers agents based on config flags, and runs the coordinator loop.

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::adapters::polymarket_clob::POLYGON_CHAIN_ID;
use crate::adapters::polymarket_ws::PriceLevel;
use crate::adapters::{BinanceWebSocket, PolymarketClient, PolymarketWebSocket, PostgresStore};
use crate::ai_clients::PolymarketSportsClient;
use crate::agents::{
    AgentContext, CryptoLobMlAgent, CryptoLobMlConfig, CryptoLobMlEntrySidePolicy,
    CryptoLobMlExitMode, CryptoTradingAgent, CryptoTradingConfig, PoliticsTradingAgent,
    PoliticsTradingConfig, SportsTradingAgent, SportsTradingConfig, TradingAgent,
};
#[cfg(feature = "rl")]
use crate::agents::{CryptoRlPolicyAgent, CryptoRlPolicyConfig};
use crate::config::AppConfig;
use crate::coordinator::config::DuplicateGuardScope;
use crate::coordinator::{
    AgentHealthResponse, AgentSnapshot, Coordinator, CoordinatorCommand, CoordinatorConfig,
    GlobalState,
};
use crate::domain::{OrderStatus, Side};
use crate::error::Result;
use crate::exchange::{build_exchange_client, parse_exchange_kind, ExchangeKind};
use crate::platform::{AgentRiskParams, AgentStatus, Domain, MarketSelector, StrategyDeployment};
use crate::signing::Wallet;
use crate::strategy::event_edge::core::EventEdgeCore;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::idempotency::IdempotencyManager;
use crate::strategy::momentum::EventMatcher;
use crate::strategy::{
    DataFeed, DataFeedManager, StrategyAction, StrategyFactory, StrategyManager,
};
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

        let req_builder = DataTradesRequest::builder()
            .filter(DataMarketFilter::markets([condition_id.to_string()]));
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

                b.push_bind(domain)
                    .push_bind(&t.condition_id)
                    .push_bind(&t.asset)
                    .push_bind(side)
                    .push_bind(t.size)
                    .push_bind(t.price)
                    .push_bind(trade_ts.unwrap_or_else(Utc::now))
                    .push_bind(t.timestamp)
                    .push_bind(&t.transaction_hash)
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
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(ToString::to_string)
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
    let clob_token_ids = market
        .clob_token_ids
        .as_deref()
        .and_then(|s| parse_json_array_strings_relaxed(s).ok())
        .unwrap_or_default();
    let outcomes = market
        .outcomes
        .as_deref()
        .and_then(|s| parse_json_array_strings_relaxed(s).ok())
        .unwrap_or_default();
    let outcome_prices = market
        .outcome_prices
        .as_deref()
        .and_then(|s| parse_json_array_strings_relaxed(s).ok())
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
        .bind(market.condition_id.as_deref())
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
    if let Ok(items) = serde_json::from_str::<Vec<StrategyDeployment>>(raw) {
        for mut dep in items {
            if dep.id.trim().is_empty() {
                continue;
            }
            dep.normalize_account_ids_in_place();
            out.push(dep);
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

fn strategy_is_momentum(strategy_key: &str) -> bool {
    strategy_key.contains("momentum") || strategy_key.contains("mom")
}

fn strategy_is_pattern_memory(strategy_key: &str) -> bool {
    strategy_key.contains("pattern")
        || strategy_key.contains("memory")
        || strategy_key.contains("pattenmem")
}

fn strategy_is_split_arb(strategy_key: &str) -> bool {
    strategy_key.contains("splitarb")
        || (strategy_key.contains("split") && strategy_key.contains("arb"))
}

fn strategy_is_lob_ml(strategy_key: &str) -> bool {
    strategy_key.contains("lob")
        || strategy_key.contains("ml")
        || strategy_key.contains("dl")
        || strategy_key.contains("deep")
        || strategy_key.contains("learning")
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

#[derive(Debug, Default)]
struct RuntimeCryptoStrategyTargets {
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

        let strategy_key = normalize_strategy_key(&dep.strategy);
        if strategy_is_pattern_memory(&strategy_key) {
            add_coins_from_selector(&dep.market_selector, &mut out.pattern_memory_coins);
        }
        if strategy_is_split_arb(&strategy_key) {
            add_coins_from_selector(&dep.market_selector, &mut out.split_arb_coins);
            if let Some(h) = normalize_horizon(dep.timeframe.as_str()) {
                out.split_arb_horizons.insert(h.to_string());
            }
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

fn build_split_arb_runtime_config(series_ids: &[String]) -> String {
    let rendered_series = series_ids
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        r#"# Auto-generated by platform bootstrap
[strategy]
name = "split_arb"
enabled = true
mode = "arbitrage"

[entry]
target_sum = 98
min_profit = 2
min_liquidity = 100

[timing]
min_time_remaining = 60
max_time_remaining = 3600

[risk]
shares = 50
max_unhedged = 10
max_exposure = 500
daily_loss_limit = 100

[markets]
series_ids = [{series_ids}]
"#,
        series_ids = rendered_series
    )
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
                cfg.enable_crypto = true;
                let strategy_key = normalize_strategy_key(&dep.strategy);

                let mut matched = false;
                if strategy_is_momentum(&strategy_key) {
                    cfg.enable_crypto_momentum = true;
                    matched = true;
                }
                if strategy_is_pattern_memory(&strategy_key) {
                    cfg.enable_crypto_pattern_memory = true;
                    matched = true;
                }
                if strategy_is_split_arb(&strategy_key) {
                    cfg.enable_crypto_split_arb = true;
                    matched = true;
                }
                if strategy_is_lob_ml(&strategy_key) {
                    cfg.enable_crypto_lob_ml = true;
                    matched = true;
                }
                #[cfg(feature = "rl")]
                if strategy_key.contains("rl") || strategy_key.contains("policy") {
                    cfg.enable_crypto_rl_policy = true;
                    matched = true;
                }
                if !matched {
                    cfg.enable_crypto_momentum = true;
                }

                add_coins_from_selector(&dep.market_selector, &mut coins);
            }
            Domain::Sports => cfg.enable_sports = true,
            Domain::Politics => cfg.enable_politics = true,
            Domain::Economics => cfg.enable_economics = true,
            Domain::Custom(ref custom_domain) => {
                custom_domains.insert(format!("custom:{}", custom_domain));
            }
        }
    }

    if cfg.enable_crypto
        && !cfg.enable_crypto_momentum
        && !cfg.enable_crypto_pattern_memory
        && !cfg.enable_crypto_split_arb
        && !cfg.enable_crypto_lob_ml
        && {
            #[cfg(feature = "rl")]
            {
                !cfg.enable_crypto_rl_policy
            }
            #[cfg(not(feature = "rl"))]
            {
                true
            }
        }
    {
        cfg.enable_crypto_momentum = true;
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

fn spawn_clob_orderbook_persistence(
    pm_ws: Arc<PolymarketWebSocket>,
    pool: PgPool,
    agent_id: String,
    domain: Domain,
    max_levels_default: usize,
    min_interval_secs_default: i64,
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
        let max_levels = env_usize("PM_ORDERBOOK_LEVELS", max_levels_default).clamp(1, 200);
        let min_interval_secs =
            env_i64("PM_ORDERBOOK_SNAPSHOT_SECS", min_interval_secs_default).max(1);

        let mut last_persisted: HashMap<String, chrono::DateTime<Utc>> = HashMap::new();
        let mut persisted_count: u64 = 0;

        loop {
            match rx.recv().await {
                Ok(book) => {
                    let now = Utc::now();
                    let token_id = book.asset_id.clone();

                    let should_persist = match last_persisted.get(&token_id) {
                        None => true,
                        Some(ts) => {
                            now.signed_duration_since(*ts).num_seconds() >= min_interval_secs
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

                    last_persisted.insert(token_id, now);
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
    pub dry_run: bool,
    pub crypto: CryptoTradingConfig,
    pub crypto_lob_ml: CryptoLobMlConfig,
    #[serde(default)]
    #[cfg(feature = "rl")]
    pub crypto_rl_policy: CryptoRlPolicyConfig,
    pub sports: SportsTradingConfig,
    pub politics: PoliticsTradingConfig,
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
            dry_run: true,
            crypto: CryptoTradingConfig::default(),
            crypto_lob_ml: CryptoLobMlConfig::default(),
            #[cfg(feature = "rl")]
            crypto_rl_policy: CryptoRlPolicyConfig::default(),
            sports: SportsTradingConfig::default(),
            politics: PoliticsTradingConfig::default(),
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
                "price_exit" | "price" | "mtm" => {
                    cfg.crypto_lob_ml.exit_mode = CryptoLobMlExitMode::PriceExit
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
        cfg.crypto_lob_ml.threshold_prob_weight = env_decimal(
            "PLOY_CRYPTO_LOB_ML__THRESHOLD_PROB_WEIGHT",
            cfg.crypto_lob_ml.threshold_prob_weight,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::new(90, 2));
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
        cfg.crypto_lob_ml.window_fallback_weight = env_decimal(
            "PLOY_CRYPTO_LOB_ML__WINDOW_FALLBACK_WEIGHT",
            cfg.crypto_lob_ml.window_fallback_weight,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::new(49, 2));
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

async fn handle_strategy_actions_runtime(
    strategy_label: &str,
    manager: Arc<StrategyManager>,
    mut rx: mpsc::Receiver<(String, StrategyAction)>,
    executor: Arc<OrderExecutor>,
    paused: Arc<AtomicBool>,
    orders_submitted: Arc<AtomicU64>,
    orders_filled: Arc<AtomicU64>,
) {
    while let Some((strategy_id, action)) = rx.recv().await {
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
                    manager.send_order_update(crate::strategy::OrderUpdate {
                        order_id: client_order_id.clone(),
                        client_order_id: Some(client_order_id),
                        status: OrderStatus::Rejected,
                        filled_qty: 0,
                        avg_fill_price: None,
                        timestamp: Utc::now(),
                        error: Some("strategy paused by coordinator".to_string()),
                    });
                    continue;
                }

                orders_submitted.fetch_add(1, Ordering::Relaxed);
                match executor.execute(&order).await {
                    Ok(result) => {
                        if matches!(result.status, OrderStatus::Filled) {
                            orders_filled.fetch_add(1, Ordering::Relaxed);
                        }
                        manager.send_order_update(crate::strategy::OrderUpdate {
                            order_id: result.order_id,
                            client_order_id: Some(client_order_id),
                            status: result.status,
                            filled_qty: result.filled_shares,
                            avg_fill_price: result.avg_fill_price,
                            timestamp: Utc::now(),
                            error: None,
                        });
                    }
                    Err(e) => {
                        warn!(
                            strategy = strategy_label,
                            strategy_id = %strategy_id,
                            error = %e,
                            "strategy action order execution failed"
                        );
                        manager.send_order_update(crate::strategy::OrderUpdate {
                            order_id: client_order_id.clone(),
                            client_order_id: Some(client_order_id),
                            status: OrderStatus::Failed,
                            filled_qty: 0,
                            avg_fill_price: None,
                            timestamp: Utc::now(),
                            error: Some(e.to_string()),
                        });
                    }
                };
            }
            StrategyAction::CancelOrder { order_id } => match executor.cancel(&order_id).await {
                Ok(cancelled) => {
                    manager.send_order_update(crate::strategy::OrderUpdate {
                        order_id: order_id.clone(),
                        client_order_id: None,
                        status: if cancelled {
                            OrderStatus::Cancelled
                        } else {
                            OrderStatus::Rejected
                        },
                        filled_qty: 0,
                        avg_fill_price: None,
                        timestamp: Utc::now(),
                        error: if cancelled {
                            None
                        } else {
                            Some("order not found or already closed".to_string())
                        },
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
            }
            StrategyAction::UpdateRisk { level, reason } => {
                info!(
                    strategy = strategy_label,
                    strategy_id = %strategy_id,
                    risk_level = ?level,
                    reason = reason,
                    "strategy risk update"
                );
            }
            StrategyAction::SubscribeFeed { feed } => {
                warn!(
                    strategy = strategy_label,
                    strategy_id = %strategy_id,
                    feed = ?feed,
                    "dynamic subscribe-feed action is not implemented in platform mode"
                );
            }
            StrategyAction::UnsubscribeFeed { feed } => {
                warn!(
                    strategy = strategy_label,
                    strategy_id = %strategy_id,
                    feed = ?feed,
                    "dynamic unsubscribe-feed action is not implemented in platform mode"
                );
            }
        }
    }
}

async fn run_managed_strategy_runtime(
    strategy_label: &str,
    agent_id: &str,
    strategy_config_toml: String,
    dry_run: bool,
    pm_client: PolymarketClient,
    pm_ws_url: String,
    mut cmd_rx: mpsc::Receiver<CoordinatorCommand>,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<()> {
    let strategy = StrategyFactory::from_toml(&strategy_config_toml, dry_run)?;
    let strategy_id = strategy.id().to_string();
    let required_feeds = strategy.required_feeds();
    let started_at = Utc::now();
    let paused = Arc::new(AtomicBool::new(false));
    let orders_submitted = Arc::new(AtomicU64::new(0));
    let orders_filled = Arc::new(AtomicU64::new(0));
    let mut status = AgentStatus::Running;

    let manager = Arc::new(StrategyManager::new(1000));
    let action_rx = manager.take_action_receiver().await.ok_or_else(|| {
        crate::error::PloyError::Internal(format!(
            "strategy {} failed to take action receiver",
            strategy_label
        ))
    })?;

    let mut binance_spot_symbols: Vec<String> = Vec::new();
    let mut binance_kline_symbols: Vec<String> = Vec::new();
    let mut binance_kline_intervals: Vec<String> = Vec::new();
    let mut binance_kline_closed_only = true;

    for feed in &required_feeds {
        match feed {
            DataFeed::BinanceSpot { symbols } => {
                binance_spot_symbols.extend(symbols.clone());
            }
            DataFeed::BinanceKlines {
                symbols,
                intervals,
                closed_only,
            } => {
                binance_kline_symbols.extend(symbols.clone());
                binance_kline_intervals.extend(intervals.clone());
                if !*closed_only {
                    binance_kline_closed_only = false;
                }
            }
            _ => {}
        }
    }

    binance_spot_symbols.sort();
    binance_spot_symbols.dedup();
    binance_kline_symbols.sort();
    binance_kline_symbols.dedup();
    binance_kline_intervals.sort();
    binance_kline_intervals.dedup();

    let mut feed_manager = DataFeedManager::new(manager.clone());
    if !binance_spot_symbols.is_empty() {
        feed_manager = feed_manager.with_binance(binance_spot_symbols.clone());
    }

    if !binance_kline_symbols.is_empty() && !binance_kline_intervals.is_empty() {
        let backfill_limit = std::env::var("PLOY_BINANCE_KLINE_BACKFILL_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(300);
        feed_manager = feed_manager.with_binance_klines(
            binance_kline_symbols.clone(),
            binance_kline_intervals.clone(),
            binance_kline_closed_only,
            backfill_limit,
        );
    }

    let has_polymarket_feed = required_feeds.iter().any(|f| {
        matches!(
            f,
            DataFeed::PolymarketEvents { .. } | DataFeed::PolymarketQuotes { .. }
        )
    });
    if has_polymarket_feed {
        let pm_ws = PolymarketWebSocket::new(&pm_ws_url);
        feed_manager = feed_manager.with_polymarket(pm_ws, pm_client.clone());
    }

    manager.start_strategy(strategy, None).await?;
    feed_manager.start().await?;
    let subscribed_tokens = feed_manager.start_for_feeds(required_feeds).await?;

    let executor = Arc::new(OrderExecutor::new(
        pm_client.clone(),
        crate::config::ExecutionConfig::default(),
    ));
    let manager_for_actions = manager.clone();
    let paused_for_actions = paused.clone();
    let orders_submitted_for_actions = orders_submitted.clone();
    let orders_filled_for_actions = orders_filled.clone();
    let strategy_label_owned = strategy_label.to_string();
    let action_task = tokio::spawn(async move {
        handle_strategy_actions_runtime(
            &strategy_label_owned,
            manager_for_actions,
            action_rx,
            executor,
            paused_for_actions,
            orders_submitted_for_actions,
            orders_filled_for_actions,
        )
        .await;
    });

    info!(
        strategy = strategy_label,
        agent_id = agent_id,
        strategy_id = %strategy_id,
        subscribed_tokens = subscribed_tokens.len(),
        dry_run = dry_run,
        "managed strategy runtime started"
    );

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                info!(
                    strategy = strategy_label,
                    agent_id = agent_id,
                    strategy_id = %strategy_id,
                    "managed strategy runtime shutdown requested"
                );
                break;
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(CoordinatorCommand::Pause) => {
                        paused.store(true, Ordering::Relaxed);
                        status = AgentStatus::Paused;
                        info!(
                            strategy = strategy_label,
                            agent_id = agent_id,
                            strategy_id = %strategy_id,
                            "managed strategy runtime paused"
                        );
                    }
                    Some(CoordinatorCommand::Resume) => {
                        paused.store(false, Ordering::Relaxed);
                        status = AgentStatus::Running;
                        info!(
                            strategy = strategy_label,
                            agent_id = agent_id,
                            strategy_id = %strategy_id,
                            "managed strategy runtime resumed"
                        );
                    }
                    Some(CoordinatorCommand::ForceClose) => {
                        warn!(
                            strategy = strategy_label,
                            agent_id = agent_id,
                            strategy_id = %strategy_id,
                            "managed strategy runtime force-close requested"
                        );
                        break;
                    }
                    Some(CoordinatorCommand::Shutdown) => {
                        info!(
                            strategy = strategy_label,
                            agent_id = agent_id,
                            strategy_id = %strategy_id,
                            "managed strategy runtime shutdown command received"
                        );
                        break;
                    }
                    Some(CoordinatorCommand::HealthCheck(tx)) => {
                        let position_count = manager
                            .get_strategy_status(&strategy_id)
                            .await
                            .map(|s| s.position_count)
                            .unwrap_or(0);
                        let snapshot = AgentSnapshot {
                            agent_id: agent_id.to_string(),
                            name: strategy_label.to_string(),
                            domain: Domain::Crypto,
                            status,
                            position_count,
                            exposure: rust_decimal::Decimal::ZERO,
                            daily_pnl: rust_decimal::Decimal::ZERO,
                            unrealized_pnl: rust_decimal::Decimal::ZERO,
                            metrics: HashMap::new(),
                            last_heartbeat: Utc::now(),
                            error_message: None,
                        };
                        let uptime_secs = (Utc::now() - started_at).num_seconds().max(0) as u64;
                        let _ = tx.send(AgentHealthResponse {
                            snapshot,
                            is_healthy: matches!(status, AgentStatus::Running | AgentStatus::Paused),
                            uptime_secs,
                            orders_submitted: orders_submitted.load(Ordering::Relaxed),
                            orders_filled: orders_filled.load(Ordering::Relaxed),
                        });
                    }
                    None => {
                        warn!(
                            strategy = strategy_label,
                            agent_id = agent_id,
                            strategy_id = %strategy_id,
                            "managed strategy runtime command channel closed"
                        );
                        break;
                    }
                }
            }
        }
    }

    if let Err(e) = manager.stop_all(true).await {
        warn!(
            strategy = strategy_label,
            agent_id = agent_id,
            strategy_id = %strategy_id,
            error = %e,
            "managed strategy runtime stop_all failed"
        );
    }
    action_task.abort();

    Ok(())
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
    let needs_polymarket_client = config.enable_crypto || config.enable_sports || config.enable_politics;
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
        executor_builder = executor_builder.with_idempotency(idem_mgr);
        info!("order executor idempotency enabled");
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

        let cmd_rx_opt = if momentum_enabled {
            let risk_params = crypto_cfg.risk_params.clone();
            Some(coordinator.register_agent(
                crypto_cfg.agent_id.clone(),
                Domain::Crypto,
                risk_params,
            ))
        } else {
            None
        };

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
        let mut all_coins: Vec<String> = Vec::new();
        if momentum_enabled {
            for coin in &crypto_cfg.coins {
                if !all_coins.contains(coin) {
                    all_coins.push(coin.clone());
                }
            }
        }
        if lob_agent_enabled {
            for coin in &lob_cfg.coins {
                if !all_coins.contains(coin) {
                    all_coins.push(coin.clone());
                }
            }
        }
        #[cfg(feature = "rl")]
        if rl_agent_enabled {
            for coin in &rl_cfg.coins {
                if !all_coins.contains(coin) {
                    all_coins.push(coin.clone());
                }
            }
        }
        if all_coins.is_empty() {
            warn!("crypto domain enabled but no crypto agents are active (coins set is empty)");
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
            let (orderbook_levels_default, orderbook_snapshot_secs_default) = (20usize, 60i64);

            spawn_clob_quote_persistence(pm_ws.clone(), pool.clone(), crypto_cfg.agent_id.clone());
            spawn_clob_orderbook_persistence(
                pm_ws.clone(),
                pool.clone(),
                crypto_cfg.agent_id.clone(),
                Domain::Crypto,
                orderbook_levels_default,
                orderbook_snapshot_secs_default,
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

        if momentum_enabled {
            if let Some(cmd_rx) = cmd_rx_opt {
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
                info!("crypto momentum agent spawned");
            } else {
                warn!(
                    agent = crypto_cfg.agent_id,
                    "crypto momentum agent enabled but coordinator cmd_rx is missing"
                );
            }
        } else {
            info!(
                agent = crypto_cfg.agent_id,
                "crypto momentum agent disabled"
            );
        }

        if pattern_memory_enabled {
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

            match build_pattern_memory_runtime_config(&coins) {
                Ok(toml_cfg) => {
                    if let Some(strategy_pm_client) = pm_client.clone() {
                        let strategy_agent_id = "pattern_memory".to_string();
                        let strategy_cmd_rx = coordinator.register_agent(
                            strategy_agent_id.clone(),
                            Domain::Crypto,
                            crypto_cfg.risk_params.clone(),
                        );
                        let strategy_ws_url = app_config.market.ws_url.clone();
                        let strategy_shutdown_rx = shutdown_tx.subscribe();
                        let strategy_dry_run = config.dry_run;
                        let jh = tokio::spawn(async move {
                            if let Err(e) = run_managed_strategy_runtime(
                                "pattern_memory",
                                &strategy_agent_id,
                                toml_cfg,
                                strategy_dry_run,
                                strategy_pm_client,
                                strategy_ws_url,
                                strategy_cmd_rx,
                                strategy_shutdown_rx,
                            )
                            .await
                            {
                                error!(agent = "pattern_memory", error = %e, "managed strategy runtime exited with error");
                            }
                        });
                        agent_handles.push(jh);
                        info!("pattern_memory strategy runtime spawned");
                    } else {
                        warn!(
                            agent = "pattern_memory",
                            "pattern_memory enabled but pm client not configured; skipping"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        agent = "pattern_memory",
                        error = %e,
                        "pattern_memory enabled but no valid runtime config could be built"
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

            if series_ids.is_empty() {
                warn!(
                    agent = "split_arb",
                    "split_arb enabled but no recognized coin/horizon series ids were resolved"
                );
            } else {
                let toml_cfg = build_split_arb_runtime_config(&series_ids);
                let strategy_agent_id = "split_arb".to_string();
                if let Some(strategy_pm_client) = pm_client.clone() {
                    let strategy_cmd_rx = coordinator.register_agent(
                        strategy_agent_id.clone(),
                        Domain::Crypto,
                        crypto_cfg.risk_params.clone(),
                    );
                    let strategy_ws_url = app_config.market.ws_url.clone();
                    let strategy_shutdown_rx = shutdown_tx.subscribe();
                    let strategy_dry_run = config.dry_run;
                    let jh = tokio::spawn(async move {
                        if let Err(e) = run_managed_strategy_runtime(
                            "split_arb",
                            &strategy_agent_id,
                            toml_cfg,
                            strategy_dry_run,
                            strategy_pm_client,
                            strategy_ws_url,
                            strategy_cmd_rx,
                            strategy_shutdown_rx,
                        )
                        .await
                        {
                            error!(agent = "split_arb", error = %e, "managed strategy runtime exited with error");
                        }
                    });
                    agent_handles.push(jh);
                    info!("split_arb strategy runtime spawned");
                } else {
                    warn!(
                        agent = "split_arb",
                        "split_arb enabled but pm client not configured; skipping"
                    );
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
                        binance_ws.clone(),
                        pm_ws.clone(),
                        event_matcher.clone(),
                        lob_cache,
                    )?;
                    let cmd_rx = coordinator.register_agent(
                        lob_cfg.agent_id.clone(),
                        Domain::Crypto,
                        risk_params,
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
                let cmd_rx = coordinator.register_agent(
                    rl_cfg.agent_id.clone(),
                    Domain::Crypto,
                    risk_params,
                );

                let agent = CryptoRlPolicyAgent::new(
                    rl_cfg.clone(),
                    binance_ws.clone(),
                    pm_ws.clone(),
                    event_matcher.clone(),
                    lob_cache,
                );
                let ctx = AgentContext::new(
                    rl_cfg.agent_id.clone(),
                    Domain::Crypto,
                    handle.clone(),
                    cmd_rx,
                );

                let jh = tokio::spawn(async move {
                    if let Err(e) = agent.run(ctx).await {
                        error!(agent = "crypto_rl_policy", error = %e, "agent exited with error");
                    }
                });
                agent_handles.push(jh);
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
            let risk_params = sports_cfg.risk_params.clone();
            let cmd_rx = coordinator.register_agent(
                sports_cfg.agent_id.clone(),
                Domain::Sports,
                risk_params,
            );

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
            let cmd_rx = coordinator.register_agent(
                politics_cfg.agent_id.clone(),
                Domain::Politics,
                risk_params,
            );

            let pm_client_ref = pm_client.as_ref().ok_or_else(|| {
                crate::error::PloyError::Validation(
                    "politics domain requires a Polymarket client, but none was initialized"
                        .to_string(),
                )
            })?;
            let core = EventEdgeCore::new(pm_client_ref.clone(), ee_cfg.clone());
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
        let window_weight_key = "PLOY_CRYPTO_LOB_ML__WINDOW_FALLBACK_WEIGHT";
        let ev_exit_buffer_key = "PLOY_CRYPTO_LOB_ML__EV_EXIT_BUFFER";
        let ev_exit_vol_scale_key = "PLOY_CRYPTO_LOB_ML__EV_EXIT_VOL_SCALE";
        let taker_fee_key = "PLOY_CRYPTO_LOB_ML__TAKER_FEE_RATE";
        let slippage_key = "PLOY_CRYPTO_LOB_ML__ENTRY_SLIPPAGE_BPS";
        let use_threshold_key = "PLOY_CRYPTO_LOB_ML__USE_PRICE_TO_BEAT";
        let require_threshold_key = "PLOY_CRYPTO_LOB_ML__REQUIRE_PRICE_TO_BEAT";
        let threshold_weight_key = "PLOY_CRYPTO_LOB_ML__THRESHOLD_PROB_WEIGHT";
        let exit_mode_key = "PLOY_CRYPTO_LOB_ML__EXIT_MODE";
        let entry_side_policy_key = "PLOY_CRYPTO_LOB_ML__ENTRY_SIDE_POLICY";
        let entry_late_window_5m_key = "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M";
        let entry_late_window_15m_key = "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_15M";

        let prev_model_type = std::env::var(model_type_key).ok();
        let prev_model_path = std::env::var(model_path_key).ok();
        let prev_model_version = std::env::var(model_version_key).ok();
        let prev_window_weight = std::env::var(window_weight_key).ok();
        let prev_ev_exit_buffer = std::env::var(ev_exit_buffer_key).ok();
        let prev_ev_exit_vol_scale = std::env::var(ev_exit_vol_scale_key).ok();
        let prev_taker_fee = std::env::var(taker_fee_key).ok();
        let prev_slippage = std::env::var(slippage_key).ok();
        let prev_use_threshold = std::env::var(use_threshold_key).ok();
        let prev_require_threshold = std::env::var(require_threshold_key).ok();
        let prev_threshold_weight = std::env::var(threshold_weight_key).ok();
        let prev_exit_mode = std::env::var(exit_mode_key).ok();
        let prev_entry_side_policy = std::env::var(entry_side_policy_key).ok();
        let prev_entry_late_window_5m = std::env::var(entry_late_window_5m_key).ok();
        let prev_entry_late_window_15m = std::env::var(entry_late_window_15m_key).ok();

        set_env(model_type_key, Some("onnx"));
        set_env(model_path_key, Some("/tmp/models/lob_tcn_v2.onnx"));
        set_env(model_version_key, Some("lob_tcn_v2"));
        set_env(window_weight_key, Some("0.15"));
        set_env(ev_exit_buffer_key, Some("0.01"));
        set_env(ev_exit_vol_scale_key, Some("0.03"));
        set_env(taker_fee_key, Some("0.03"));
        set_env(slippage_key, Some("12"));
        set_env(use_threshold_key, Some("true"));
        set_env(require_threshold_key, Some("false"));
        set_env(threshold_weight_key, Some("0.40"));
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
            cfg.crypto_lob_ml.window_fallback_weight,
            rust_decimal::Decimal::new(15, 2)
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
        assert_eq!(
            cfg.crypto_lob_ml.threshold_prob_weight,
            rust_decimal::Decimal::new(40, 2)
        );
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
        match prev_window_weight.as_deref() {
            Some(v) => set_env(window_weight_key, Some(v)),
            None => set_env(window_weight_key, None),
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
        match prev_threshold_weight.as_deref() {
            Some(v) => set_env(threshold_weight_key, Some(v)),
            None => set_env(threshold_weight_key, None),
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
