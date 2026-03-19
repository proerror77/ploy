use super::*;
use crate::collector::token_targets::{
    ensure_collector_token_targets_table, upsert_collector_token_targets,
};

impl SyncCollector {
    pub(super) async fn persist_quote_tick(
        &self,
        token_id: &str,
        side: &str,
        best_bid: Option<Decimal>,
        best_ask: Option<Decimal>,
        bid_size: Option<Decimal>,
        ask_size: Option<Decimal>,
        received_at: DateTime<Utc>,
    ) -> Result<()> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };
        sqlx::query(
            r#"
            INSERT INTO clob_quote_ticks (
                token_id, side, best_bid, best_ask, bid_size, ask_size, source, received_at, domain
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(token_id)
        .bind(side)
        .bind(best_bid)
        .bind(best_ask)
        .bind(bid_size)
        .bind(ask_size)
        .bind("sync_collector")
        .bind(received_at)
        .bind("crypto")
        .execute(pool)
        .await?;
        Ok(())
    }

    pub(super) async fn persist_token_targets(
        &self,
        targets: &[CollectorTokenTarget],
    ) -> Result<()> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };
        ensure_collector_token_targets_table(pool).await?;
        upsert_collector_token_targets(pool, targets).await
    }

    pub(super) async fn initialize_schema(&self, pool: &PgPool) -> Result<()> {
        crate::persistence::ensure_binance_lob_ticks_table(pool).await?;
        crate::persistence::ensure_clob_quote_ticks_table(pool).await?;

        if self.persist_sync_records {
            sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS sync_records (
                    id BIGSERIAL PRIMARY KEY,
                    timestamp TIMESTAMPTZ NOT NULL,
                    symbol VARCHAR(20) NOT NULL,
                    bn_mid_price DECIMAL(20, 8) NOT NULL,
                    bn_best_bid DECIMAL(20, 8) NOT NULL,
                    bn_best_ask DECIMAL(20, 8) NOT NULL,
                    bn_spread_bps DECIMAL(10, 4) NOT NULL,
                    bn_obi_5 DECIMAL(10, 6) NOT NULL,
                    bn_obi_10 DECIMAL(10, 6) NOT NULL,
                    bn_bid_volume DECIMAL(20, 8) NOT NULL,
                    bn_ask_volume DECIMAL(20, 8) NOT NULL,
                    pm_yes_price DECIMAL(10, 4),
                    pm_no_price DECIMAL(10, 4),
                    pm_market_slug VARCHAR(100),
                    pm_yes_token_id TEXT,
                    pm_no_token_id TEXT,
                    bn_price_change_1s DECIMAL(10, 6),
                    bn_price_change_5s DECIMAL(10, 6),
                    bn_momentum DECIMAL(10, 6),
                    created_at TIMESTAMPTZ DEFAULT NOW()
                )
                "#,
            )
            .execute(pool)
            .await?;

            sqlx::query(
                r#"
                ALTER TABLE sync_records
                    ADD COLUMN IF NOT EXISTS pm_yes_token_id TEXT,
                    ADD COLUMN IF NOT EXISTS pm_no_token_id TEXT
                "#,
            )
            .execute(pool)
            .await?;

            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_sync_records_ts ON sync_records(timestamp)",
            )
            .execute(pool)
            .await?;
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_sync_records_symbol ON sync_records(symbol)",
            )
            .execute(pool)
            .await?;
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_sync_records_symbol_ts ON sync_records(symbol, timestamp)",
            )
            .execute(pool)
            .await?;
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_sync_records_pm_market_slug ON sync_records(pm_market_slug)",
            )
            .execute(pool)
            .await?;
        }

        self.ensure_sync_records_derived_view(pool).await?;
        info!(
            persist_sync_records = self.persist_sync_records,
            "sync collector schema initialized"
        );
        Ok(())
    }

    async fn ensure_sync_records_derived_view(&self, pool: &PgPool) -> Result<()> {
        sqlx::query(
            r#"
            CREATE OR REPLACE VIEW sync_records_derived AS
            WITH token_pairs AS (
                SELECT
                    NULLIF(BTRIM(metadata->>'symbol'), '') AS symbol,
                    NULLIF(BTRIM(metadata->>'slug'), '') AS market_slug,
                    MAX(CASE WHEN UPPER(COALESCE(metadata->>'side', '')) = 'UP' THEN token_id END) AS pm_yes_token_id,
                    MAX(CASE WHEN UPPER(COALESCE(metadata->>'side', '')) = 'DOWN' THEN token_id END) AS pm_no_token_id,
                    MAX(updated_at) AS updated_at
                FROM collector_token_targets
                GROUP BY 1, 2
            ),
            token_choice AS (
                SELECT DISTINCT ON (symbol)
                    symbol,
                    market_slug,
                    pm_yes_token_id,
                    pm_no_token_id
                FROM token_pairs
                WHERE symbol IS NOT NULL
                  AND pm_yes_token_id IS NOT NULL
                  AND pm_no_token_id IS NOT NULL
                ORDER BY symbol, updated_at DESC
            )
            SELECT
                b.event_time AS timestamp,
                b.symbol,
                b.mid_price AS bn_mid_price,
                b.best_bid AS bn_best_bid,
                b.best_ask AS bn_best_ask,
                b.spread_bps AS bn_spread_bps,
                b.obi_5 AS bn_obi_5,
                b.obi_10 AS bn_obi_10,
                b.bid_volume_5 AS bn_bid_volume,
                b.ask_volume_5 AS bn_ask_volume,
                qy.best_ask AS pm_yes_price,
                qn.best_ask AS pm_no_price,
                tc.market_slug AS pm_market_slug,
                tc.pm_yes_token_id,
                tc.pm_no_token_id,
                CASE
                    WHEN p1.mid_price IS NULL OR p1.mid_price = 0 THEN NULL
                    ELSE (b.mid_price - p1.mid_price) / p1.mid_price
                END AS bn_price_change_1s,
                CASE
                    WHEN p5.mid_price IS NULL OR p5.mid_price = 0 THEN NULL
                    ELSE (b.mid_price - p5.mid_price) / p5.mid_price
                END AS bn_price_change_5s,
                CASE
                    WHEN p10.mid_price IS NULL OR p10.mid_price = 0 THEN NULL
                    ELSE (b.mid_price - p10.mid_price) / p10.mid_price
                END AS bn_momentum
            FROM binance_lob_ticks b
            LEFT JOIN token_choice tc
              ON tc.symbol = b.symbol
            LEFT JOIN LATERAL (
                SELECT q.best_ask
                FROM clob_quote_ticks q
                WHERE q.token_id = tc.pm_yes_token_id
                  AND q.side = 'UP'
                  AND q.received_at <= b.event_time
                ORDER BY q.received_at DESC
                LIMIT 1
            ) qy ON true
            LEFT JOIN LATERAL (
                SELECT q.best_ask
                FROM clob_quote_ticks q
                WHERE q.token_id = tc.pm_no_token_id
                  AND q.side = 'DOWN'
                  AND q.received_at <= b.event_time
                ORDER BY q.received_at DESC
                LIMIT 1
            ) qn ON true
            LEFT JOIN LATERAL (
                SELECT b1.mid_price
                FROM binance_lob_ticks b1
                WHERE b1.symbol = b.symbol
                  AND b1.event_time <= (b.event_time - INTERVAL '1 second')
                ORDER BY b1.event_time DESC
                LIMIT 1
            ) p1 ON true
            LEFT JOIN LATERAL (
                SELECT b1.mid_price
                FROM binance_lob_ticks b1
                WHERE b1.symbol = b.symbol
                  AND b1.event_time <= (b.event_time - INTERVAL '5 second')
                ORDER BY b1.event_time DESC
                LIMIT 1
            ) p5 ON true
            LEFT JOIN LATERAL (
                SELECT b1.mid_price
                FROM binance_lob_ticks b1
                WHERE b1.symbol = b.symbol
                  AND b1.event_time <= (b.event_time - INTERVAL '10 second')
                ORDER BY b1.event_time DESC
                LIMIT 1
            ) p10 ON true
            "#,
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    pub(super) async fn persist_binance_lob_tick(
        &self,
        pool: &PgPool,
        update: &LobUpdate,
    ) -> Result<()> {
        let bids = depth_levels_json(&update.raw_state, true, 20);
        let asks = depth_levels_json(&update.raw_state, false, 20);

        sqlx::query(
            r#"
            INSERT INTO binance_lob_ticks (
                symbol, update_id, best_bid, best_ask, mid_price, spread_bps,
                obi_5, obi_10, bid_volume_5, ask_volume_5,
                bids, asks, event_time, source
            ) VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10,
                $11, $12, $13, $14
            )
            "#,
        )
        .bind(&update.symbol)
        .bind(update.snapshot.update_id)
        .bind(update.snapshot.best_bid)
        .bind(update.snapshot.best_ask)
        .bind(update.snapshot.mid_price)
        .bind(update.snapshot.spread_bps)
        .bind(update.snapshot.obi_5)
        .bind(update.snapshot.obi_10)
        .bind(update.snapshot.bid_volume_5)
        .bind(update.snapshot.ask_volume_5)
        .bind(bids)
        .bind(asks)
        .bind(update.snapshot.timestamp)
        .bind("sync_collector")
        .execute(pool)
        .await?;
        Ok(())
    }

    pub(super) async fn persist_sync_record(
        &self,
        pool: &PgPool,
        record: &SyncRecord,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO sync_records (
                timestamp, symbol, bn_mid_price, bn_best_bid, bn_best_ask,
                bn_spread_bps, bn_obi_5, bn_obi_10, bn_bid_volume, bn_ask_volume,
                pm_yes_price, pm_no_price, pm_market_slug,
                pm_yes_token_id, pm_no_token_id,
                bn_price_change_1s, bn_price_change_5s, bn_momentum
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
            "#,
        )
        .bind(record.timestamp)
        .bind(&record.symbol)
        .bind(record.bn_mid_price)
        .bind(record.bn_best_bid)
        .bind(record.bn_best_ask)
        .bind(record.bn_spread_bps)
        .bind(record.bn_obi_5)
        .bind(record.bn_obi_10)
        .bind(record.bn_bid_volume)
        .bind(record.bn_ask_volume)
        .bind(record.pm_yes_price)
        .bind(record.pm_no_price)
        .bind(&record.pm_market_slug)
        .bind(&record.pm_yes_token_id)
        .bind(&record.pm_no_token_id)
        .bind(record.bn_price_change_1s)
        .bind(record.bn_price_change_5s)
        .bind(record.bn_momentum)
        .execute(pool)
        .await?;

        Ok(())
    }
}

fn depth_levels_json(
    state: &super::super::binance_depth::OrderBookState,
    is_bid: bool,
    max_levels: usize,
) -> serde_json::Value {
    let levels: Vec<serde_json::Value> = if is_bid {
        state
            .bids
            .iter()
            .rev()
            .take(max_levels)
            .map(|(p, q)| {
                let price = Decimal::from(*p) / Decimal::from(100);
                serde_json::json!({
                    "price": price.to_string(),
                    "size": q.to_string(),
                })
            })
            .collect()
    } else {
        state
            .asks
            .iter()
            .take(max_levels)
            .map(|(p, q)| {
                let price = Decimal::from(*p) / Decimal::from(100);
                serde_json::json!({
                    "price": price.to_string(),
                    "size": q.to_string(),
                })
            })
            .collect()
    };
    serde_json::Value::Array(levels)
}
