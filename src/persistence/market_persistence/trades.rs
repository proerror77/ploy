use super::alerts::{
    InsertedTradeTickRow, TradeAlertConfig, TradeAlertState, maybe_emit_trade_alerts,
};
use super::{env_i64, env_u64, env_usize};
use crate::domain::Domain;
use crate::error::Result;
use crate::strategy::momentum::EventMatcher;
use chrono::Utc;
use futures_util::StreamExt;
use polymarket_client_sdk::data::Client as DataApiClient;
use polymarket_client_sdk::data::types::MarketFilter as DataMarketFilter;
use polymarket_client_sdk::data::types::request::TradesRequest as DataTradesRequest;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, instrument, warn};

pub(super) async fn ensure_clob_trade_ticks_table(pool: &PgPool) -> Result<()> {
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

#[instrument(skip(data_client, pool, last_seen_by_market))]
pub(super) async fn collect_trades_for_market(
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

        if min_ts_in_page < target_min_ts {
            break;
        }

        if trades.len() < page_limit {
            break;
        }
    }

    if max_ts_seen > last_seen_ts {
        let mut map = last_seen_by_market.write().await;
        map.insert(condition_id.to_string(), max_ts_seen);
    }
}

pub(crate) fn spawn_polymarket_trade_persistence(
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

        let last_seen_by_market: Arc<tokio::sync::RwLock<HashMap<String, i64>>> =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        let end_grace_secs = env_i64("PM_TRADES_END_GRACE_SECS", 600).max(0);
        let min_remaining_for_collection = env_i64("PM_TRADES_MIN_REMAINING_SECS", 0)
            .max(-86400)
            .min(86400);
        let mut tracked_markets: HashMap<String, i64> = HashMap::new();

        let mut tick = tokio::time::interval(Duration::from_secs(poll_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tick.tick().await;
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
