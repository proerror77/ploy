//! Polymarket CLOB orderbook history collector.
//!
//! Polymarket exposes an (historically undocumented) endpoint:
//!   GET https://clob.polymarket.com/orderbook-history
//! with query parameters:
//!   asset_id, startTs (ms), endTs (ms), limit, offset
//!
//! This collector can backfill or continuously harvest L2 snapshots for one or
//! more assets and persist them into Postgres for research/backtesting.

use chrono::{DateTime, Utc};
use reqwest::Url;
use rust_decimal::Decimal;
use serde::Deserialize;
use sqlx::postgres::PgPool;
use tracing::{debug, info, warn};

use crate::adapters::polymarket_ws::PriceLevel;
use crate::error::Result;

const DEFAULT_CLOB_BASE_URL: &str = "https://clob.polymarket.com";

#[derive(Debug, Clone)]
pub struct OrderbookHistoryCollectorConfig {
    pub clob_base_url: String,
    pub levels: usize,
    pub sample_ms: i64,
    pub page_limit: usize,
    pub max_pages: usize,
    pub source: String,
}

impl Default for OrderbookHistoryCollectorConfig {
    fn default() -> Self {
        Self {
            clob_base_url: DEFAULT_CLOB_BASE_URL.to_string(),
            levels: 20,
            sample_ms: 1000,
            page_limit: 500,
            max_pages: 50,
            source: "polymarket_orderbook_history".to_string(),
        }
    }
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

#[derive(Debug, Clone, Deserialize)]
pub struct OrderbookHistoryResponse {
    pub count: i64,
    #[serde(default)]
    pub data: Vec<OrderbookHistoryEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderbookHistoryEntry {
    pub market: String,
    pub asset_id: String,
    /// Milliseconds since epoch (string in API response)
    pub timestamp: String,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub bids: Vec<PriceLevel>,
    #[serde(default)]
    pub asks: Vec<PriceLevel>,
}

impl OrderbookHistoryEntry {
    pub fn timestamp_ms(&self) -> Option<i64> {
        self.timestamp.parse::<i64>().ok()
    }

    pub fn timestamp_dt(&self) -> Option<DateTime<Utc>> {
        let ts_ms = self.timestamp_ms()?;
        DateTime::<Utc>::from_timestamp_millis(ts_ms)
    }

    pub fn hash_str(&self) -> &str {
        self.hash.as_deref().unwrap_or("")
    }
}

pub struct OrderbookHistoryCollector {
    http: reqwest::Client,
    pool: PgPool,
    cfg: OrderbookHistoryCollectorConfig,
}

impl OrderbookHistoryCollector {
    pub fn new(pool: PgPool, cfg: OrderbookHistoryCollectorConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            pool,
            cfg,
        }
    }

    pub async fn ensure_tables(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS clob_orderbook_history_ticks (
                id BIGSERIAL PRIMARY KEY,
                token_id TEXT NOT NULL,
                condition_id TEXT NOT NULL,
                book_ts_ms BIGINT NOT NULL,
                book_ts TIMESTAMPTZ NOT NULL,
                hash TEXT NOT NULL,
                bids JSONB NOT NULL,
                asks JSONB NOT NULL,
                source TEXT NOT NULL DEFAULT 'polymarket_orderbook_history',
                collected_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Deduplicate re-runs with the same snapshot identity.
        sqlx::query(
            r#"
            CREATE UNIQUE INDEX IF NOT EXISTS uniq_clob_orderbook_history_ticks
              ON clob_orderbook_history_ticks(token_id, book_ts_ms, hash)
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_clob_orderbook_history_ticks_token_time
              ON clob_orderbook_history_ticks(token_id, book_ts_ms DESC)
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_clob_orderbook_history_ticks_condition_time
              ON clob_orderbook_history_ticks(condition_id, book_ts_ms DESC)
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_clob_orderbook_history_ticks_time
              ON clob_orderbook_history_ticks(book_ts_ms DESC)
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn fetch_page(
        &self,
        asset_id: &str,
        start_ts_ms: i64,
        end_ts_ms: i64,
        limit: usize,
        offset: usize,
    ) -> Result<OrderbookHistoryResponse> {
        let mut url = Url::parse(&self.cfg.clob_base_url).map_err(|e| {
            crate::error::PloyError::Internal(format!("invalid clob_base_url: {e}"))
        })?;
        url.set_path("orderbook-history");

        let resp = self
            .http
            .get(url)
            .query(&[
                ("asset_id", asset_id),
                ("startTs", &start_ts_ms.to_string()),
                ("endTs", &end_ts_ms.to_string()),
                ("limit", &limit.to_string()),
                ("offset", &offset.to_string()),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(crate::error::PloyError::Internal(format!(
                "orderbook-history request failed (status={status}): {body}"
            )));
        }

        Ok(resp.json::<OrderbookHistoryResponse>().await?)
    }

    async fn insert_rows(
        &self,
        rows: &[(
            String,
            String,
            i64,
            DateTime<Utc>,
            String,
            Vec<DepthLevelJson>,
            Vec<DepthLevelJson>,
        )],
    ) -> Result<u64> {
        if rows.is_empty() {
            return Ok(0);
        }

        let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            r#"
            INSERT INTO clob_orderbook_history_ticks (
                token_id,
                condition_id,
                book_ts_ms,
                book_ts,
                hash,
                bids,
                asks,
                source
            )
            "#,
        );

        qb.push_values(rows.iter(), |mut b, row| {
            b.push_bind(&row.0) // token_id
                .push_bind(&row.1) // condition_id
                .push_bind(row.2) // book_ts_ms
                .push_bind(row.3) // book_ts
                .push_bind(&row.4) // hash
                .push_bind(sqlx::types::Json(&row.5)) // bids
                .push_bind(sqlx::types::Json(&row.6)) // asks
                .push_bind(&self.cfg.source);
        });

        qb.push(" ON CONFLICT DO NOTHING");

        let result = qb.build().execute(&self.pool).await?;
        Ok(result.rows_affected())
    }

    /// Backfill a single asset over a time range.
    ///
    /// Returns the number of rows inserted.
    pub async fn backfill_asset(
        &self,
        asset_id: &str,
        start_ts_ms: i64,
        end_ts_ms: i64,
    ) -> Result<u64> {
        let limit = self.cfg.page_limit.clamp(1, 5000);
        let max_pages = self.cfg.max_pages.max(1);
        let levels = self.cfg.levels.clamp(1, 2000);
        let sample_ms = self.cfg.sample_ms.max(0);

        let mut inserted: u64 = 0;
        let mut offset: usize = 0;

        // Sampling state (per asset backfill).
        let mut next_allowed_ts: Option<i64> = None;
        let mut last_hash: Option<String> = None;

        for _page in 0..max_pages {
            let resp = self
                .fetch_page(asset_id, start_ts_ms, end_ts_ms, limit, offset)
                .await?;

            if resp.data.is_empty() {
                break;
            }

            let mut batch: Vec<(
                String,
                String,
                i64,
                DateTime<Utc>,
                String,
                Vec<DepthLevelJson>,
                Vec<DepthLevelJson>,
            )> = Vec::with_capacity(resp.data.len());

            for entry in resp.data {
                let Some(ts_ms) = entry.timestamp_ms() else {
                    continue;
                };
                let Some(ts) = entry.timestamp_dt() else {
                    continue;
                };
                let hash = entry.hash_str().to_string();

                if sample_ms > 0 {
                    if let Some(next) = next_allowed_ts {
                        if ts_ms < next && last_hash.as_deref() == Some(hash.as_str()) {
                            continue;
                        }
                    }
                }

                let bids = parse_depth_levels(&entry.bids, true, levels);
                let asks = parse_depth_levels(&entry.asks, false, levels);

                batch.push((
                    entry.asset_id,
                    entry.market,
                    ts_ms,
                    ts,
                    hash.clone(),
                    bids,
                    asks,
                ));

                if sample_ms > 0 {
                    next_allowed_ts = Some(ts_ms.saturating_add(sample_ms));
                    last_hash = Some(hash);
                }
            }

            inserted = inserted.saturating_add(self.insert_rows(&batch).await?);

            offset = offset.saturating_add(limit);
            if resp.count > 0 && offset as i64 >= resp.count {
                break;
            }

            // Safety: avoid infinite loops if API doesn't provide stable paging.
            if batch.is_empty() {
                debug!(
                    asset_id,
                    offset,
                    limit,
                    "orderbook-history page produced no usable rows; stopping pagination"
                );
                break;
            }
        }

        Ok(inserted)
    }

    /// Resume point helper: get last persisted `book_ts_ms` for an asset.
    pub async fn last_ts_ms_for_asset(&self, asset_id: &str) -> Result<i64> {
        let v = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COALESCE(MAX(book_ts_ms), 0)
            FROM clob_orderbook_history_ticks
            WHERE token_id = $1
            "#,
        )
        .bind(asset_id)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        Ok(v)
    }

    /// Convenience helper: backfill a list of assets (sequential, with per-asset stats).
    pub async fn backfill_assets(
        &self,
        asset_ids: &[String],
        start_ts_ms: i64,
        end_ts_ms: i64,
    ) -> Result<()> {
        self.ensure_tables().await?;

        for asset_id in asset_ids {
            let started = std::time::Instant::now();
            match self.backfill_asset(asset_id, start_ts_ms, end_ts_ms).await {
                Ok(n) => {
                    info!(
                        asset_id = asset_id.as_str(),
                        inserted = n,
                        elapsed_ms = started.elapsed().as_millis(),
                        "orderbook-history backfill complete"
                    );
                }
                Err(e) => {
                    warn!(asset_id = asset_id.as_str(), error = %e, "orderbook-history backfill failed");
                }
            }
        }

        Ok(())
    }
}
