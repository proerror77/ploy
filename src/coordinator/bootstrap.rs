//! Platform Bootstrap — wires up Coordinator + Agents from config
//!
//! Entry point for `ploy platform start`. Creates shared infrastructure,
//! registers agents based on config flags, and runs the coordinator loop.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, instrument, trace, warn};

use crate::adapters::polymarket_clob::POLYGON_CHAIN_ID;
use crate::adapters::{BinanceWebSocket, PolymarketClient, PolymarketWebSocket, PostgresStore};
use crate::agents::governance_context::GovernanceContext;
use crate::agents::openclaw::OpenClawAgent;
use crate::agents::governance_agent::GovernanceAgent;
use crate::config::{
    AppConfig, CryptoEntryMode, CryptoTradingConfig, PoliticsTradingConfig, SportsTradingConfig,
};
use crate::coordinator::config::DuplicateGuardScope;
use crate::coordinator::{Coordinator, CoordinatorConfig, CoordinatorHandle, GlobalState};
use crate::domain::{OrderStatus, Side};
use crate::error::Result;
use crate::exchange::{build_exchange_client, parse_exchange_kind, ExchangeKind};
use crate::agent_runtime::AgentRiskParams;
use crate::control_plane::{MarketSelector, StrategyDeployment};
use crate::platform::{
    BinanceDataPlaneHandle, DataPlaneConfig, Domain, PlatformDataPlane,
};
use crate::plugins::{
    ComposableCryptoSpec, DeploymentState as PluginDeploymentState, PluginDefinition,
    PluginDeployment, PluginKind, PluginRegistry, PluginSpec, RegisteredStrategySpec,
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
use std::time::Duration;

use super::strategy_runtime::run_managed_strategy_runtime;

mod bootstrap_config;
mod coordinator_bootstrap;
mod crypto_runtime_support;
mod managed_crypto;
mod openclaw_config;
mod runtime_config;
mod runtime_orchestration;
mod runtime_spawns;
mod schema;
mod sports_runtime_support;
mod startup_context;
mod status;
mod strategy_deployments;
mod support;
#[cfg(test)]
mod tests;

pub use self::bootstrap_config::PlatformBootstrapConfig;
use self::coordinator_bootstrap::initialize_coordinator_runtime;
use self::crypto_runtime_support::initialize_crypto_runtime_support;
pub use self::openclaw_config::{AllocatorConfig, OpenClawConfig, RegimeConfig, StraddleConfig};
use self::runtime_orchestration::run_platform_runtime;
use self::runtime_spawns::{spawn_managed_strategy_runtime_task, spawn_openclaw_governance_agent};
use self::schema::{
    ensure_accounts_table, ensure_binance_lob_ticks_table, ensure_binance_price_ticks_table,
    ensure_clob_quote_ticks_table, ensure_schema_repairs, upsert_account_from_config,
};
pub(crate) use self::schema::{
    ensure_agent_order_executions_table, ensure_clob_orderbook_snapshots_table,
    ensure_coordinator_governance_policies_table,
    ensure_coordinator_governance_policy_history_table, ensure_coordinator_ingress_state_table,
    ensure_pm_market_metadata_table, ensure_pm_token_settlements_table,
    ensure_risk_runtime_state_table, ensure_strategy_observability_tables,
};
use self::sports_runtime_support::prepare_sports_runtime_support;
use self::startup_context::initialize_startup_context;
pub use self::status::print_platform_status;
#[cfg(all(test, feature = "rl"))]
use self::strategy_deployments::build_crypto_rl_policy_runtime_config;
#[cfg(test)]
use self::strategy_deployments::{
    build_crypto_lob_ml_runtime_config, build_event_edge_runtime_config,
    build_momentum_runtime_config, build_nba_comeback_runtime_config,
    build_split_arb_runtime_config,
};
use self::strategy_deployments::{
    collect_managed_strategy_runtime_plans, collect_runtime_crypto_strategy_targets,
    ManagedRuntimeBootstrapStep, ManagedRuntimeDataPlaneKind,
};
use self::support::{env_bool, env_i64, env_u64, env_usize, lob_levels_json};

#[cfg(test)]
use super::runtime_specs::{
    build_event_edge_managed_runtime_spec, build_event_edge_runtime_config,
    build_momentum_managed_runtime_spec, build_momentum_runtime_config,
    build_nba_comeback_managed_runtime_spec, build_nba_comeback_runtime_config,
    build_split_arb_managed_runtime_spec, build_split_arb_runtime_config,
};
use super::runtime_specs::ManagedStrategyBootstrapSpec;
use super::strategy_runtime::{
    run_managed_strategy_runtime as run_managed_strategy_runtime_module,
    ManagedStrategyRuntimeConfig,
};

const CLOB_PERSIST_MIN_INTERVAL_SECS: i64 = 2;
const BINANCE_PERSIST_MIN_INTERVAL_SECS: i64 = 1;
const PM_COLLECTOR_REFRESH_SECS: u64 = 300;


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

pub(crate) fn crypto_series_id_for(coin: &str, horizon: &str) -> Option<&'static str> {
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

pub(crate) fn coin_symbol_for(coin: &str) -> Option<String> {
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
        cfg.crypto.coins = sorted;
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
        sports = cfg.enable_sports,
        politics = cfg.enable_politics,
        economics = cfg.enable_economics,
        coins = ?cfg.crypto.coins,
        timeframes = ?tf,
        "applied strategy deployment matrix to platform runtime"
    );
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
                ..
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

fn spawn_governance_agent_task<A: GovernanceAgent>(
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
    let ctx = GovernanceContext::new(agent_id, domain, handle.clone(), cmd_rx);

    let jh = tokio::spawn(async move {
        if let Err(e) = agent.run(ctx).await {
            error!(agent = error_label, error = %e, "governance agent exited with error");
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

fn spawn_openclaw_agent(
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    coordinator: &mut Coordinator,
    handle: &CoordinatorHandle,
    openclaw_cfg: OpenClawConfig,
    freshness: Arc<crate::platform::DataPlaneFreshness>,
) {
    let oc_symbols = vec![openclaw_cfg.btc_symbol.clone()];
    let oc_binance_ws = Arc::new(BinanceWebSocket::new(oc_symbols));
    oc_binance_ws.set_freshness(Arc::clone(&freshness));

    let oc_ws = oc_binance_ws.clone();
    tokio::spawn(async move {
        if let Err(e) = oc_ws.run().await {
            tracing::error!(error = %e, "openclaw binance ws exited");
        }
    });

    let oc_risk_params = AgentRiskParams::governance_only();
    let oc_agent_id = openclaw_cfg.agent_id.clone();
    let oc_regime_tick_secs = openclaw_cfg.regime_tick_secs;
    let oc_market_data = BinanceDataPlaneHandle::new(oc_binance_ws);
    let agent = OpenClawAgent::new(openclaw_cfg, oc_market_data);
    spawn_governance_agent_task(
        agent_handles,
        coordinator,
        handle,
        agent,
        oc_risk_params,
        "openclaw",
    );
    info!(
        agent_id = %oc_agent_id,
        regime_tick = oc_regime_tick_secs,
        "openclaw meta-agent spawned"
    );
}

fn builtin_runtime_plugin_definition(plugin_id: &str) -> Result<PluginDefinition> {
    let registry = PluginRegistry::builtin_runtime_registry()?;
    registry
        .plugin(plugin_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing {plugin_id} plugin definition").into())
}

fn builtin_runtime_plugin_deployment(
    plugin_id: &str,
    deployment_id: impl Into<String>,
    account_id: &str,
) -> PluginDeployment {
    PluginDeployment {
        deployment_id: deployment_id.into(),
        plugin_id: plugin_id.to_string(),
        account_id: account_id.to_string(),
        state: PluginDeploymentState::Enabled,
    }
}

fn project_pattern_memory_plugin_runtime_spec(
    coins: &[String],
    account_id: &str,
) -> Result<ManagedStrategyBootstrapSpec> {
    let definition = builtin_runtime_plugin_definition("crypto.pattern_memory.v1")?;
    crate::plugins::projector::project_pattern_memory_runtime_spec(
        &definition,
        &PluginSpec::ComposableCrypto(ComposableCryptoSpec {
            signal_blocks: vec!["pattern_memory".to_string()],
        }),
        &builtin_runtime_plugin_deployment(
            "crypto.pattern_memory.v1",
            "managed-runtime.pattern_memory",
            account_id,
        ),
        coins,
    )
}

fn project_split_arb_plugin_runtime_spec(
    symbols: &[String],
    series_ids: &[String],
    account_id: &str,
) -> Result<ManagedStrategyBootstrapSpec> {
    let definition = builtin_runtime_plugin_definition("crypto.split_arb.v1")?;
    crate::plugins::projector::project_split_arb_runtime_spec(
        &definition,
        &PluginSpec::ComposableCrypto(ComposableCryptoSpec {
            signal_blocks: vec!["split_arb".to_string()],
        }),
        &builtin_runtime_plugin_deployment(
            "crypto.split_arb.v1",
            "managed-runtime.split_arb",
            account_id,
        ),
        symbols,
        series_ids,
    )
}

#[allow(clippy::too_many_arguments)]
async fn spawn_canonical_crypto_strategy_runtimes(
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    coordinator: &mut Coordinator,
    shutdown_tx: &broadcast::Sender<()>,
    pm_client: Option<PolymarketClient>,
    pm_ws_url: &str,
    data_plane: Option<Arc<PlatformDataPlane>>,
    shared_pool: Option<PgPool>,
    account_id: &str,
    dry_run: bool,
    crypto_cfg: &CryptoTradingConfig,
    runtime_crypto_targets: &RuntimeCryptoStrategyTargets,
    momentum_enabled: bool,
    momentum_symbols: &[String],
    pattern_memory_enabled: bool,
    split_arb_enabled: bool,
) {
    if momentum_enabled {
        if momentum_symbols.is_empty() {
            warn!(
                agent = crypto_cfg.agent_id,
                "crypto momentum enabled but no recognized symbols were resolved"
            );
        } else {
            let strategy_agent_id = crypto_cfg.agent_id.clone();
            let runtime_spec = crate::plugins::projector::project_momentum_runtime_spec(
                &PluginDefinition {
                    plugin_id: "crypto.momentum.v1".to_string(),
                    kind: PluginKind::ComposableCrypto,
                    version: "v1".to_string(),
                    domain: Domain::Crypto,
                },
                &PluginSpec::ComposableCrypto(ComposableCryptoSpec {
                    signal_blocks: vec!["momentum".to_string()],
                }),
                &PluginDeployment {
                    deployment_id: format!("deploy.crypto.momentum.{}", strategy_agent_id),
                    plugin_id: "crypto.momentum.v1".to_string(),
                    account_id: account_id.to_string(),
                    state: PluginDeploymentState::Enabled,
                },
                momentum_symbols,
                crypto_cfg,
            );
            if let Err(e) = runtime_spec.and_then(|runtime_spec| {
                spawn_managed_strategy_runtime_spec(
                    agent_handles,
                    coordinator,
                    shutdown_tx,
                    runtime_spec,
                    crypto_cfg.risk_params.clone(),
                    dry_run,
                    pm_client.clone(),
                    pm_ws_url,
                    data_plane.clone(),
                    shared_pool.clone(),
                    account_id,
                )
            }) {
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
        if let Some(ref pool) = shared_pool {
            if let Err(e) = crate::strategy::pattern_memory::persistence::ensure_table(pool).await {
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

        match project_pattern_memory_plugin_runtime_spec(&coins, account_id) {
            Ok(runtime_spec) => {
                if let Err(e) = spawn_managed_strategy_runtime_spec(
                    agent_handles,
                    coordinator,
                    shutdown_tx,
                    runtime_spec,
                    crypto_cfg.risk_params.clone(),
                    dry_run,
                    pm_client.clone(),
                    pm_ws_url,
                    data_plane.clone(),
                    shared_pool.clone(),
                    account_id,
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

        let mut horizons: Vec<String> = if runtime_crypto_targets.split_arb_horizons.is_empty() {
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
                .filter_map(|series_id| symbol_for_crypto_series_id(series_id).map(str::to_string))
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
            match project_split_arb_plugin_runtime_spec(&symbols, &series_ids, account_id) {
                Ok(runtime_spec) => {
                    if let Err(e) = spawn_managed_strategy_runtime_spec(
                        agent_handles,
                        coordinator,
                        shutdown_tx,
                        runtime_spec,
                        crypto_cfg.risk_params.clone(),
                        dry_run,
                        pm_client,
                        pm_ws_url,
                        data_plane,
                        shared_pool,
                        account_id,
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
                Err(e) => {
                    warn!(
                        agent = "split_arb",
                        error = %e,
                        "split_arb enabled but no valid plugin runtime spec could be built"
                    );
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_sports_strategy_runtime(
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    coordinator: &mut Coordinator,
    shutdown_tx: &broadcast::Sender<()>,
    app_config: &AppConfig,
    shared_pool: Option<PgPool>,
    freshness: Arc<crate::platform::DataPlaneFreshness>,
    pm_client: Option<PolymarketClient>,
    account_id: &str,
    dry_run: bool,
    sports_cfg: SportsTradingConfig,
    nba_cfg: &crate::config::NbaComebackConfig,
) -> Result<()> {
    let registry = PluginRegistry::builtin_runtime_registry()?;
    let definition = registry
        .plugin("sports.nba_comeback.v1")
        .ok_or_else(|| anyhow::anyhow!("missing sports.nba_comeback.v1 plugin definition"))?;
    let managed_runtime_spec = crate::plugins::projector::project_nba_comeback_runtime_spec(
        definition,
        &PluginSpec::RegisteredStrategy(RegisteredStrategySpec::nba_comeback()),
        &PluginDeployment {
            deployment_id: format!("deploy.sports.nba_comeback.{}", sports_cfg.agent_id),
            plugin_id: definition.plugin_id.clone(),
            account_id: account_id.to_string(),
            state: crate::plugins::DeploymentState::Enabled,
        },
        &app_config.database.url,
        &sports_cfg,
        nba_cfg,
    )?;

    let pool = start_sports_market_data_support(shared_pool, app_config, freshness, &sports_cfg)
        .await?;

    spawn_managed_strategy_runtime_spec(
        agent_handles,
        coordinator,
        shutdown_tx,
        managed_runtime_spec,
        sports_cfg.risk_params.clone(),
        dry_run,
        pm_client,
        &app_config.market.ws_url,
        None,
        Some(pool.clone()),
        account_id,
    )?;
    info!(
        agent = %sports_cfg.agent_id,
        "sports nba_comeback strategy runtime spawned"
    );

    Ok(())
}

fn spawn_politics_strategy_runtime(
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    coordinator: &mut Coordinator,
    shutdown_tx: &broadcast::Sender<()>,
    pm_client: Option<PolymarketClient>,
    app_config: &AppConfig,
    shared_pool: Option<PgPool>,
    account_id: &str,
    dry_run: bool,
    politics_cfg: PoliticsTradingConfig,
    ee_cfg: &crate::config::EventEdgeAgentConfig,
) -> Result<()> {
    let strategy_agent_id = politics_cfg.agent_id.clone();
    let registry = PluginRegistry::builtin_runtime_registry()?;
    let definition = registry
        .plugin("politics.event_edge.v1")
        .ok_or_else(|| anyhow::anyhow!("missing politics.event_edge.v1 plugin definition"))?;
    let runtime_spec = crate::plugins::projector::project_event_edge_runtime_spec(
        definition,
        &PluginSpec::RegisteredStrategy(RegisteredStrategySpec::event_edge()),
        &PluginDeployment {
            deployment_id: format!("deploy.politics.event_edge.{}", politics_cfg.agent_id),
            plugin_id: definition.plugin_id.clone(),
            account_id: account_id.to_string(),
            state: crate::plugins::DeploymentState::Enabled,
        },
        &app_config.market.rest_url,
        &politics_cfg,
        ee_cfg,
    )?;
    spawn_managed_strategy_runtime_spec(
        agent_handles,
        coordinator,
        shutdown_tx,
        runtime_spec,
        politics_cfg.risk_params.clone(),
        dry_run,
        pm_client,
        &app_config.market.ws_url,
        None,
        shared_pool,
        account_id,
    )?;
    info!(agent = %strategy_agent_id, "politics event_edge strategy runtime spawned");
    Ok(())
}

fn compat_sports_runtimes_enabled() -> bool {
    env_bool("PLOY_ENABLE_COMPAT_SPORTS_RUNTIMES", false)
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
    Err(crate::error::PloyError::Internal("start_platform: merge conflict stub — restore after resolving bootstrap.rs".to_string()))
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
            state: crate::platform::DeploymentState::Enabled,
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
            state: crate::platform::DeploymentState::Enabled,
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
    }

    #[test]
    fn apply_strategy_deployments_does_not_route_retired_lob_ml_runtime() {
        let mut cfg = PlatformBootstrapConfig::default();
        let deployments = vec![crypto_deployment("crypto_lob_ml", true)];

        apply_strategy_deployments(&mut cfg, &deployments, "default", false);

        assert!(
            !cfg.enable_crypto,
            "retired lob_ml runtime should no longer auto-enable crypto domain"
        );
        assert!(!cfg.enable_crypto_momentum);
        assert!(!cfg.enable_crypto_pattern_memory);
        assert!(!cfg.enable_crypto_split_arb);
    }

    #[cfg(feature = "rl")]
    #[test]
    fn apply_strategy_deployments_does_not_route_retired_rl_policy_runtime() {
        let mut cfg = PlatformBootstrapConfig::default();
        let deployments = vec![crypto_deployment("crypto_rl_policy", true)];

        apply_strategy_deployments(&mut cfg, &deployments, "default", false);

        assert!(
            !cfg.enable_crypto,
            "retired rl_policy runtime should no longer auto-enable crypto domain"
        );
        assert!(!cfg.enable_crypto_momentum);
        assert!(!cfg.enable_crypto_pattern_memory);
        assert!(!cfg.enable_crypto_split_arb);
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

        assert_eq!(spec.strategy_label, "composable_crypto");
        assert_eq!(spec.agent_id, crypto_cfg.agent_id);
        assert_eq!(spec.domain, Domain::Crypto);
        assert!(spec
            .strategy_config_toml
            .contains("name = \"composable_crypto\""));
        assert!(spec.strategy_config_toml.contains("[composable_crypto]"));
    }

    #[test]
    fn project_momentum_plugin_runtime_spec_projects_canonical_launch() {
        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let crypto_cfg = CryptoTradingConfig::default();
        let definition = crate::plugins::PluginDefinition {
            plugin_id: "crypto.momentum.v1".to_string(),
            kind: crate::plugins::PluginKind::ComposableCrypto,
            version: "v1".to_string(),
            domain: Domain::Crypto,
        };
        let spec =
            crate::plugins::PluginSpec::ComposableCrypto(crate::plugins::ComposableCryptoSpec {
                signal_blocks: vec!["momentum".to_string()],
            });
        let deployment = crate::plugins::PluginDeployment {
            deployment_id: "deploy.crypto.momentum.default".to_string(),
            plugin_id: definition.plugin_id.clone(),
            account_id: "default".to_string(),
            state: crate::plugins::DeploymentState::Enabled,
        };

        let projected = crate::plugins::projector::project_momentum_runtime_spec(
            &definition,
            &spec,
            &deployment,
            &symbols,
            &crypto_cfg,
        )
        .expect("project momentum runtime spec");

        assert_eq!(projected.strategy_label, "composable_crypto");
        assert_eq!(projected.agent_id, crypto_cfg.agent_id);
        assert_eq!(projected.domain, Domain::Crypto);
        assert!(projected
            .strategy_config_toml
            .contains("name = \"composable_crypto\""));
        assert!(projected
            .strategy_config_toml
            .contains("[composable_crypto]"));
    }

    #[test]
    fn build_momentum_managed_runtime_spec_matches_plugin_projection() {
        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let crypto_cfg = CryptoTradingConfig::default();
        let definition = crate::plugins::PluginDefinition {
            plugin_id: "crypto.momentum.v1".to_string(),
            kind: crate::plugins::PluginKind::ComposableCrypto,
            version: "v1".to_string(),
            domain: Domain::Crypto,
        };
        let spec =
            crate::plugins::PluginSpec::ComposableCrypto(crate::plugins::ComposableCryptoSpec {
                signal_blocks: vec!["momentum".to_string()],
            });
        let deployment = crate::plugins::PluginDeployment {
            deployment_id: "deploy.crypto.momentum.default".to_string(),
            plugin_id: definition.plugin_id.clone(),
            account_id: "default".to_string(),
            state: crate::plugins::DeploymentState::Enabled,
        };

        let managed = build_momentum_managed_runtime_spec(&symbols, &crypto_cfg);
        let projected = crate::plugins::projector::project_momentum_runtime_spec(
            &definition,
            &spec,
            &deployment,
            &symbols,
            &crypto_cfg,
        )
        .expect("project momentum runtime spec");

        assert_eq!(managed.strategy_label, projected.strategy_label);
        assert_eq!(managed.agent_id, projected.agent_id);
        assert_eq!(managed.domain, projected.domain);
        assert_eq!(managed.strategy_config_toml, projected.strategy_config_toml);
    }

    #[test]
    fn project_momentum_plugin_runtime_spec_projects_composable_crypto_launch() {
        let symbols = vec!["BTCUSDT".to_string()];
        let crypto_cfg = CryptoTradingConfig::default();
        let definition = crate::plugins::PluginDefinition {
            plugin_id: "crypto.momentum.v1".to_string(),
            kind: crate::plugins::PluginKind::ComposableCrypto,
            version: "v1".to_string(),
            domain: Domain::Crypto,
        };
        let spec =
            crate::plugins::PluginSpec::ComposableCrypto(crate::plugins::ComposableCryptoSpec {
                signal_blocks: vec!["momentum".to_string()],
            });
        let deployment = crate::plugins::PluginDeployment {
            deployment_id: "deploy.crypto.momentum.default".to_string(),
            plugin_id: definition.plugin_id.clone(),
            account_id: "default".to_string(),
            state: crate::plugins::DeploymentState::Enabled,
        };

        let projected = crate::plugins::projector::project_momentum_runtime_spec(
            &definition,
            &spec,
            &deployment,
            &symbols,
            &crypto_cfg,
        )
        .expect("project momentum runtime spec");

        assert_eq!(projected.strategy_label, "composable_crypto");
        assert!(projected
            .strategy_config_toml
            .contains("name = \"composable_crypto\""));
        assert!(projected
            .strategy_config_toml
            .contains("[composable_crypto]"));
        assert!(projected
            .strategy_config_toml
            .contains("signal_blocks = [\"momentum\"]"));
    }

    #[test]
    fn project_event_edge_plugin_runtime_spec_stamps_plugin_identity() {
        let registry = crate::plugins::PluginRegistry::builtin_runtime_registry()
            .expect("builtin plugin registry");
        let definition = registry
            .plugin("politics.event_edge.v1")
            .expect("event edge plugin definition");
        let spec = crate::plugins::PluginSpec::RegisteredStrategy(
            crate::plugins::RegisteredStrategySpec::event_edge(),
        );
        let deployment = crate::plugins::PluginDeployment {
            deployment_id: "deploy.politics.event_edge.default".to_string(),
            plugin_id: definition.plugin_id.clone(),
            account_id: "default".to_string(),
            state: crate::plugins::DeploymentState::Enabled,
        };
        let politics_cfg = PoliticsTradingConfig::default();
        let cfg = crate::config::EventEdgeAgentConfig {
            enabled: true,
            event_ids: vec!["evt-1".to_string()],
            titles: vec!["Best AI model".to_string()],
            interval_secs: 180,
            min_edge: Decimal::new(8, 2),
            max_entry: Decimal::new(70, 2),
            shares: 25,
            trade: true,
            cooldown_secs: 120,
            max_daily_spend_usd: Decimal::from(55),
        };

        let projected = crate::plugins::projector::project_event_edge_runtime_spec(
            definition,
            &spec,
            &deployment,
            "https://clob.polymarket.com",
            &politics_cfg,
            &cfg,
        )
        .expect("project event_edge runtime spec");

        assert!(projected
            .strategy_config_toml
            .contains("plugin_id = \"politics.event_edge.v1\""));
    }

    #[test]
    fn project_nba_comeback_plugin_runtime_spec_stamps_plugin_identity() {
        let registry = crate::plugins::PluginRegistry::builtin_runtime_registry()
            .expect("builtin plugin registry");
        let definition = registry
            .plugin("sports.nba_comeback.v1")
            .expect("nba plugin definition");
        let spec = crate::plugins::PluginSpec::RegisteredStrategy(
            crate::plugins::RegisteredStrategySpec::nba_comeback(),
        );
        let deployment = crate::plugins::PluginDeployment {
            deployment_id: "deploy.sports.nba_comeback.default".to_string(),
            plugin_id: definition.plugin_id.clone(),
            account_id: "default".to_string(),
            state: crate::plugins::DeploymentState::Enabled,
        };
        let sports_cfg = SportsTradingConfig::default();
        let cfg = sample_nba_comeback_config(false);

        let projected = crate::plugins::projector::project_nba_comeback_runtime_spec(
            definition,
            &spec,
            &deployment,
            "postgres://db.example.com/ploy",
            &sports_cfg,
            &cfg,
        )
        .expect("project nba runtime spec");

        assert!(projected
            .strategy_config_toml
            .contains("plugin_id = \"sports.nba_comeback.v1\""));
    }

    #[test]
    fn project_pattern_memory_plugin_runtime_spec_projects_canonical_launch() {
        let coins = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];

        let projected = project_pattern_memory_plugin_runtime_spec(&coins, "default")
            .expect("project pattern_memory runtime spec");

        assert_eq!(projected.strategy_label, "pattern_memory");
        assert_eq!(projected.agent_id, "pattern_memory");
        assert_eq!(projected.domain, Domain::Crypto);
        assert!(projected
            .strategy_config_toml
            .contains("name = \"pattern_memory\""));
    }

    #[test]
    fn project_split_arb_plugin_runtime_spec_projects_canonical_launch() {
        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let series_ids = vec!["10192".to_string(), "10684".to_string()];

        let projected =
            project_split_arb_plugin_runtime_spec(&symbols, &series_ids, "default")
                .expect("project split_arb runtime spec");

        assert_eq!(projected.strategy_label, "split_arb");
        assert_eq!(projected.agent_id, "split_arb");
        assert_eq!(projected.domain, Domain::Crypto);
        assert!(projected
            .strategy_config_toml
            .contains("symbols = [\"BTCUSDT\", \"ETHUSDT\"]"));
    }

    #[test]
    fn build_event_edge_runtime_config_projects_targets_and_limits() {
        let cfg = crate::config::EventEdgeAgentConfig {
            enabled: true,
            event_ids: vec!["evt-1".to_string()],
            titles: vec!["Best AI model".to_string()],
            interval_secs: 180,
            min_edge: Decimal::new(8, 2),
            max_entry: Decimal::new(70, 2),
            shares: 25,
            trade: true,
            cooldown_secs: 120,
            max_daily_spend_usd: Decimal::from(55),
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
            event_ids: vec!["evt-1".to_string()],
            titles: vec!["Best AI model".to_string()],
            interval_secs: 180,
            min_edge: Decimal::new(8, 2),
            max_entry: Decimal::new(70, 2),
            shares: 25,
            trade: true,
            cooldown_secs: 120,
            max_daily_spend_usd: Decimal::from(55),
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

        let rendered = build_nba_comeback_runtime_config("postgres://db.example.com/ploy", &cfg);

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
        );

        assert_eq!(spec.strategy_label, "nba_comeback");
        assert_eq!(spec.agent_id, sports_cfg.agent_id);
        assert_eq!(spec.domain, Domain::Sports);
        assert!(spec
            .strategy_config_toml
            .contains("name = \"nba_comeback\""));
        assert!(spec
            .strategy_config_toml
            .contains("poll_interval_secs = 45"));
        assert!(spec
            .strategy_config_toml
            .contains("url = \"postgres://db.example.com/ploy\""));
    }

    #[test]
    fn build_nba_comeback_managed_runtime_spec_projects_grok_enabled_configs() {
        let sports_cfg = SportsTradingConfig::default();
        let nba_cfg = sample_nba_comeback_config(true);

        let spec = build_nba_comeback_managed_runtime_spec(
            "postgres://db.example.com/ploy",
            &sports_cfg,
            &nba_cfg,
        );

        assert_eq!(spec.strategy_label, "nba_comeback");
        assert!(spec.strategy_config_toml.contains("[grok]"));
        assert!(spec.strategy_config_toml.contains("enabled = true"));
    }

    #[test]
    fn neutral_config_types_can_drive_runtime_projection() {
        let symbols = vec!["BTCUSDT".to_string()];
        let crypto_cfg = crate::config::CryptoTradingConfig::default();
        let politics_cfg = crate::config::PoliticsTradingConfig::default();
        let sports_cfg = crate::config::SportsTradingConfig::default();

        let momentum = build_momentum_runtime_config(&symbols, &crypto_cfg);
        assert!(momentum.contains("[strategy]"));

        let nba_cfg = sample_nba_comeback_config(false);
        let event_edge = build_event_edge_managed_runtime_spec(
            "https://clob.polymarket.com",
            &politics_cfg,
            &crate::config::EventEdgeAgentConfig::default(),
        );
        assert_eq!(event_edge.strategy_label, "event_edge");

        let nba = build_nba_comeback_managed_runtime_spec(
            "postgres://db.example.com/ploy",
            &sports_cfg,
            &nba_cfg,
        );
        assert_eq!(nba.strategy_label, "nba_comeback");
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

}
