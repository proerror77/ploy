//! Platform Bootstrap — wires up Coordinator + Agents from config
//!
//! Entry point for `ploy platform start`. Creates shared infrastructure,
//! registers agents based on config flags, and runs the coordinator loop.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::agent::PolymarketSportsClient;
use crate::adapters::{BinanceWebSocket, PolymarketClient, PolymarketWebSocket};
use crate::adapters::polymarket_ws::PriceLevel;
use crate::agents::{
    AgentContext, CryptoTradingAgent, CryptoTradingConfig, PoliticsTradingAgent,
    PoliticsTradingConfig, SportsTradingAgent, SportsTradingConfig, TradingAgent,
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
use std::collections::{HashMap, HashSet, VecDeque};
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

pub(crate) async fn ensure_agent_order_executions_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS agent_order_executions (
            id BIGSERIAL PRIMARY KEY,
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
        let mut last_persisted: HashMap<String, (chrono::DateTime<Utc>, Option<rust_decimal::Decimal>, Option<rust_decimal::Decimal>, Option<rust_decimal::Decimal>, Option<rust_decimal::Decimal>)> = HashMap::new();
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
                            let elapsed =
                                now.signed_duration_since(*ts).num_seconds() >= CLOB_PERSIST_MIN_INTERVAL_SECS;
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
                            persisted_count,
                            "persisted clob quote ticks"
                        );
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(agent = agent_id, lagged = n, "clob persistence receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    warn!(agent = agent_id, "clob persistence receiver closed");
                    break;
                }
            }
        }
    });
}

fn spawn_binance_price_persistence(binance_ws: Arc<BinanceWebSocket>, pool: PgPool, agent_id: String) {
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
        let mut last_persisted: HashMap<String, (chrono::DateTime<Utc>, Option<rust_decimal::Decimal>, Option<rust_decimal::Decimal>)> = HashMap::new();
        let mut persisted_count: u64 = 0;

        loop {
            match rx.recv().await {
                Ok(update) => {
                    let now = Utc::now();
                    let should_persist = match last_persisted.get(&update.symbol) {
                        None => true,
                        Some((ts, prev_price, prev_qty)) => {
                            let changed = *prev_price != Some(update.price) || *prev_qty != update.quantity;
                            let elapsed =
                                now.signed_duration_since(*ts).num_seconds() >= BINANCE_PERSIST_MIN_INTERVAL_SECS;
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

                    last_persisted.insert(update.symbol.clone(), (now, Some(update.price), update.quantity));
                    persisted_count = persisted_count.saturating_add(1);

                    if persisted_count % 10_000 == 0 {
                        info!(
                            agent = agent_id,
                            persisted_count,
                            "persisted Binance price ticks"
                        );
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(agent = agent_id, lagged = n, "binance persistence receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    warn!(agent = agent_id, "binance persistence receiver closed");
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

fn deserialize_decimal<'de, D>(deserializer: D) -> std::result::Result<rust_decimal::Decimal, D::Error>
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
    String,                 // token_id
    String,                 // side
    rust_decimal::Decimal,  // size
    rust_decimal::Decimal,  // price
    chrono::DateTime<Utc>,  // trade_ts
    i64,                    // trade_ts_unix
    String,                 // transaction_hash
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
        let burst_cooldown_secs =
            env_i64("PM_TRADE_BURST_COOLDOWN_SECS", burst_window_secs).max(1);

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
        map.get(condition_id).copied().unwrap_or(0)
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

                match qb.build_query_as::<InsertedTradeTickRow>().fetch_all(pool).await {
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

            let size_trigger =
                alert_cfg.burst_min_size > Decimal::ZERO && token_state.sum_size >= alert_cfg.burst_min_size;
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
        let mut alert_state: Option<Arc<tokio::sync::Mutex<TradeAlertState>>> = if alert_cfg
            .burst_enabled()
        {
            Some(Arc::new(tokio::sync::Mutex::new(TradeAlertState::default())))
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

            // Build a stable market list from the cached Gamma snapshots (EventMatcher).
            let mut markets: Vec<String> = Vec::new();
            for coin in &coins {
                let symbol = format!("{}USDT", coin.to_uppercase());
                for ev in event_matcher.get_events(&symbol).await {
                    if !ev.condition_id.trim().is_empty() {
                        markets.push(ev.condition_id);
                    }
                }
            }
            markets.sort();
            markets.dedup();

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

fn parse_depth_levels(levels: &[PriceLevel], is_bid: bool, max_levels: usize) -> Vec<DepthLevelJson> {
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
    pub enable_sports: bool,
    pub enable_politics: bool,
    pub dry_run: bool,
    pub crypto: CryptoTradingConfig,
    pub sports: SportsTradingConfig,
    pub politics: PoliticsTradingConfig,
}

impl Default for PlatformBootstrapConfig {
    fn default() -> Self {
        Self {
            coordinator: CoordinatorConfig::default(),
            enable_crypto: true,
            enable_sports: false,
            enable_politics: false,
            dry_run: true,
            crypto: CryptoTradingConfig::default(),
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

        // Coordinator risk from app config
        cfg.coordinator.risk = crate::platform::RiskConfig {
            max_platform_exposure: app.risk.max_single_exposure_usd,
            max_consecutive_failures: app.risk.max_consecutive_failures,
            daily_loss_limit: app.risk.daily_loss_limit_usd,
            max_spread_bps: 500,
            critical_bypass_exposure: true,
        };

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
    info!(
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
    let mut coordinator = Coordinator::new(config.coordinator.clone(), executor);
    if let Some(pool) = shared_pool.as_ref() {
        if let Err(e) = ensure_agent_order_executions_table(pool).await {
            warn!(error = %e, "failed to ensure agent_order_executions table; execution logging disabled");
        } else {
            coordinator.set_execution_log_pool(pool.clone());
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

        // Discover active crypto events and token IDs (Gamma API) via EventMatcher
        let event_matcher = Arc::new(EventMatcher::new(pm_client.clone()));
        if let Err(e) = event_matcher.refresh().await {
            warn!(error = %e, "crypto event matcher refresh failed (continuing)");
        }

        // Create WebSocket feeds
        let symbols: Vec<String> = crypto_cfg
            .coins
            .iter()
            .map(|c| format!("{}USDT", c))
            .collect();
        let binance_ws = Arc::new(BinanceWebSocket::new(symbols));
        let pm_ws = Arc::new(PolymarketWebSocket::new(&app_config.market.ws_url));

        // Seed PM token → side mapping for data collection, so QuoteUpdates carry the correct
        // UP/DOWN side and can be persisted to Postgres.
        let mut collector_tokens: HashSet<String> = HashSet::new();
        for coin in &crypto_cfg.coins {
            let symbol = format!("{}USDT", coin.to_uppercase());
            for ev in event_matcher.get_events(&symbol).await {
                pm_ws.register_token(&ev.up_token_id, Side::Up).await;
                pm_ws.register_token(&ev.down_token_id, Side::Down).await;
                collector_tokens.insert(ev.up_token_id);
                collector_tokens.insert(ev.down_token_id);
            }
        }
        info!(
            agent = %crypto_cfg.agent_id,
            token_count = collector_tokens.len(),
            "seeded PM token mappings for crypto data collection"
        );

        // Keep expanding the subscription token set over time so 5m + 15m markets continue
        // to be recorded throughout the day, independent of which single market the agent
        // is currently trading.
        let pm_ws_collector = pm_ws.clone();
        let matcher_collector = event_matcher.clone();
        let coins_collector = crypto_cfg.coins.clone();
        let agent_id_collector = crypto_cfg.agent_id.clone();
        tokio::spawn(async move {
            let mut known = collector_tokens;
            let mut tick = tokio::time::interval(Duration::from_secs(PM_COLLECTOR_REFRESH_SECS));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                tick.tick().await;

                if let Err(e) = matcher_collector.refresh().await {
                    warn!(agent = %agent_id_collector, error = %e, "pm token collector refresh failed");
                    continue;
                }

                let mut added: usize = 0;
                for coin in &coins_collector {
                    let symbol = format!("{}USDT", coin.to_uppercase());
                    for ev in matcher_collector.get_events(&symbol).await {
                        if known.insert(ev.up_token_id.clone()) {
                            pm_ws_collector
                                .register_token(&ev.up_token_id, Side::Up)
                                .await;
                            added = added.saturating_add(1);
                        }
                        if known.insert(ev.down_token_id.clone()) {
                            pm_ws_collector
                                .register_token(&ev.down_token_id, Side::Down)
                                .await;
                            added = added.saturating_add(1);
                        }
                    }
                }

                if added > 0 {
                    pm_ws_collector.request_resubscribe();
                    info!(
                        agent = %agent_id_collector,
                        added,
                        known_tokens = known.len(),
                        "pm token collector registered new tokens; resubscribe requested"
                    );
                }
            }
        });

        // Optional persistence pipeline for CLOB quotes (best-effort).
        // Do not block agent startup if DB is temporarily unavailable.
        if let Some(pool) = shared_pool.as_ref() {
            spawn_clob_quote_persistence(
                pm_ws.clone(),
                pool.clone(),
                crypto_cfg.agent_id.clone(),
            );
            spawn_clob_orderbook_persistence(
                pm_ws.clone(),
                pool.clone(),
                crypto_cfg.agent_id.clone(),
                Domain::Crypto,
            );
            spawn_binance_price_persistence(binance_ws.clone(), pool.clone(), crypto_cfg.agent_id.clone());
            spawn_polymarket_trade_persistence(
                event_matcher.clone(),
                pool.clone(),
                crypto_cfg.agent_id.clone(),
                crypto_cfg.coins.clone(),
                Domain::Crypto,
            );
            info!(agent = crypto_cfg.agent_id, "market data persistence tasks started");
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

        let agent = CryptoTradingAgent::new(crypto_cfg.clone(), binance_ws, pm_ws, event_matcher);
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
    }

    if config.enable_sports {
        if let Some(ref nba_cfg) = app_config.nba_comeback {
            let sports_cfg = config.sports.clone();
            let risk_params = sports_cfg.risk_params.clone();
            let cmd_rx = coordinator.register_agent(sports_cfg.agent_id.clone(), risk_params);

            let pool = match shared_pool.as_ref() {
                Some(pool) => pool.clone(),
                None => PgPoolOptions::new()
                    .max_connections(app_config.database.max_connections)
                    .connect(&app_config.database.url)
                    .await?,
            };
            if let Err(e) = ensure_clob_orderbook_snapshots_table(&pool).await {
                warn!(agent = sports_cfg.agent_id, error = %e, "failed to ensure clob_orderbook_snapshots table");
            }

            let espn = crate::strategy::nba_comeback::espn::EspnClient::new();
            let stats = crate::strategy::nba_comeback::ComebackStatsProvider::new(
                pool.clone(),
                nba_cfg.season.clone(),
            );
            let core =
                crate::strategy::nba_comeback::NbaComebackCore::new(espn, stats, nba_cfg.clone());
            let mut agent = SportsTradingAgent::new(sports_cfg.clone(), core)
                .with_observation_pool(pool);
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
