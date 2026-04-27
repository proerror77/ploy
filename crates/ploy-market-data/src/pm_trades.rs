//! Polymarket public trade-print collector.
//!
//! Polls the Polymarket Data API for trades on active crypto markets and persists
//! tick-level prints into `clob_trade_ticks`. This complements the CLOB quote
//! collector: quotes tell us what was executable, while trade prints let research
//! measure PM trade imbalance, bursts, and trade-to-quote response.

use std::io;
use std::str::FromStr;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use polymarket_client_sdk::data::types::request::TradesRequest;
use polymarket_client_sdk::data::types::response::Trade;
use polymarket_client_sdk::data::types::MarketFilter;
use polymarket_client_sdk::data::types::Side;
use polymarket_client_sdk::data::Client as DataClient;
use polymarket_client_sdk::types::B256;
use sqlx::PgPool;
use tokio::time::sleep;
use tracing::{info, warn};

const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 15;
const DEFAULT_MARKET_LOOKBACK_SECS: i64 = 7_200;
const DEFAULT_MARKET_LOOKAHEAD_SECS: i64 = 7_200;
const DEFAULT_TRADE_LOOKBACK_SECS: i64 = 7_200;
const DEFAULT_API_LIMIT: i32 = 500;
const DEFAULT_PER_MARKET_DELAY_MS: u64 = 250;
const DEFAULT_STALE_AFTER_SECS: u64 = 180;

/// Configuration for public Polymarket trade collection.
#[derive(Debug, Clone)]
pub struct TradeCollectorConfig {
    pub symbols: Vec<String>,
    pub refresh_interval_secs: u64,
    pub market_lookback_secs: i64,
    pub market_lookahead_secs: i64,
    pub trade_lookback_secs: i64,
    pub api_limit: i32,
    pub per_market_delay_ms: u64,
    pub stale_after_secs: u64,
    pub taker_only: bool,
}

impl TradeCollectorConfig {
    #[must_use]
    pub fn with_safe_defaults(mut self) -> Self {
        if self.symbols.is_empty() {
            self.symbols = vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(),
                "SOLUSDT".to_string(),
            ];
        }
        if self.refresh_interval_secs == 0 {
            self.refresh_interval_secs = DEFAULT_REFRESH_INTERVAL_SECS;
        }
        if self.market_lookback_secs <= 0 {
            self.market_lookback_secs = DEFAULT_MARKET_LOOKBACK_SECS;
        }
        if self.market_lookahead_secs <= 0 {
            self.market_lookahead_secs = DEFAULT_MARKET_LOOKAHEAD_SECS;
        }
        if self.trade_lookback_secs <= 0 {
            self.trade_lookback_secs = DEFAULT_TRADE_LOOKBACK_SECS;
        }
        if !(1..=10_000).contains(&self.api_limit) {
            self.api_limit = DEFAULT_API_LIMIT;
        }
        if self.per_market_delay_ms == 0 {
            self.per_market_delay_ms = DEFAULT_PER_MARKET_DELAY_MS;
        }
        if self.stale_after_secs == 0 {
            self.stale_after_secs = DEFAULT_STALE_AFTER_SECS;
        }

        self
    }
}

/// Polling collector for Polymarket Data API trades.
pub struct TradeCollector {
    config: TradeCollectorConfig,
    pool: PgPool,
    data: DataClient,
}

#[derive(Debug, Clone)]
struct ActiveTradeMarket {
    market_id: String,
    market_slug: String,
    symbol: String,
    condition_id: B256,
}

#[derive(sqlx::FromRow)]
struct ActiveTradeMarketRow {
    market_id: String,
    market_slug: String,
    symbol: String,
    condition_id: String,
}

#[derive(Debug, Default)]
struct PollReport {
    markets: usize,
    successful_markets: usize,
    trades_fetched: u64,
    trades_inserted: u64,
    trades_skipped: u64,
    api_errors: u64,
    persist_errors: u64,
}

impl TradeCollector {
    #[must_use]
    pub fn new(config: TradeCollectorConfig, pool: PgPool) -> Self {
        Self {
            config: config.with_safe_defaults(),
            pool,
            data: DataClient::default(),
        }
    }

    /// Run the polling loop until a stale API/persistence failure should be
    /// surfaced to systemd.
    ///
    /// Empty markets or markets with no new trades are healthy. A restart is
    /// only requested when active markets exist but every market poll keeps
    /// failing beyond `stale_after_secs`.
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!(
            symbols = ?self.config.symbols,
            refresh_secs = self.config.refresh_interval_secs,
            "Starting Polymarket trade collector"
        );

        let mut last_success_at: Option<DateTime<Utc>> = None;

        loop {
            match self.poll_once().await {
                Ok(report) => {
                    if report.markets == 0 || report.successful_markets > 0 {
                        last_success_at = Some(Utc::now());
                    }

                    info!(
                        markets = report.markets,
                        successful_markets = report.successful_markets,
                        trades_fetched = report.trades_fetched,
                        trades_inserted = report.trades_inserted,
                        trades_skipped = report.trades_skipped,
                        api_errors = report.api_errors,
                        persist_errors = report.persist_errors,
                        "Polymarket trade collector poll complete"
                    );
                }
                Err(error) => {
                    warn!(error = %error, "Polymarket trade collector poll failed");
                    if stale_for_too_long(last_success_at, self.config.stale_after_secs) {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!(
                                "Polymarket trade collector stale for more than {}s",
                                self.config.stale_after_secs
                            ),
                        )
                        .into());
                    }
                }
            }

            sleep(StdDuration::from_secs(self.config.refresh_interval_secs)).await;
        }
    }

    async fn poll_once(&self) -> Result<PollReport, Box<dyn std::error::Error>> {
        let markets = self.active_markets().await?;
        let mut report = PollReport {
            markets: markets.len(),
            ..PollReport::default()
        };

        for market in markets {
            match self.collect_market(&market).await {
                Ok((fetched, inserted, skipped)) => {
                    report.successful_markets += 1;
                    report.trades_fetched += fetched;
                    report.trades_inserted += inserted;
                    report.trades_skipped += skipped;
                }
                Err(error) => {
                    report.api_errors += 1;
                    warn!(
                        market_id = %market.market_id,
                        market_slug = %market.market_slug,
                        symbol = %market.symbol,
                        condition_id = %market.condition_id,
                        error = %error,
                        "Failed to collect Polymarket trades for market"
                    );
                }
            }

            sleep(StdDuration::from_millis(self.config.per_market_delay_ms)).await;
        }

        if report.markets > 0 && report.successful_markets == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "all active Polymarket trade market polls failed",
            )
            .into());
        }

        Ok(report)
    }

    async fn active_markets(&self) -> Result<Vec<ActiveTradeMarket>, sqlx::Error> {
        let placeholders = (1..=self.config.symbols.len())
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let lookback_idx = self.config.symbols.len() + 1;
        let lookahead_idx = self.config.symbols.len() + 2;

        let query = format!(
            r#"
            SELECT
                market_id,
                COALESCE(market_slug, market_id) AS market_slug,
                strategy_symbol AS symbol,
                raw_market->>'conditionId' AS condition_id
            FROM pm_market_catalog
            WHERE LOWER(market_family) = 'crypto'
              AND strategy_symbol IN ({})
              AND raw_market->>'conditionId' IS NOT NULL
              AND COALESCE(end_time, NOW()) >= NOW() - (${}::bigint * INTERVAL '1 second')
              AND COALESCE(start_time, NOW()) <= NOW() + (${}::bigint * INTERVAL '1 second')
            ORDER BY start_time NULLS LAST, end_time NULLS LAST, market_id
            "#,
            placeholders, lookback_idx, lookahead_idx
        );

        let mut q = sqlx::query_as::<_, ActiveTradeMarketRow>(&query);
        for symbol in &self.config.symbols {
            q = q.bind(symbol);
        }
        q = q
            .bind(self.config.market_lookback_secs)
            .bind(self.config.market_lookahead_secs);

        let rows = q.fetch_all(&self.pool).await?;
        let mut markets = Vec::with_capacity(rows.len());

        for row in rows {
            match B256::from_str(&row.condition_id) {
                Ok(condition_id) => markets.push(ActiveTradeMarket {
                    market_id: row.market_id,
                    market_slug: row.market_slug,
                    symbol: row.symbol,
                    condition_id,
                }),
                Err(error) => {
                    warn!(
                        market_id = %row.market_id,
                        condition_id = %row.condition_id,
                        error = %error,
                        "Skipping market with invalid conditionId"
                    );
                }
            }
        }

        Ok(markets)
    }

    async fn collect_market(
        &self,
        market: &ActiveTradeMarket,
    ) -> Result<(u64, u64, u64), Box<dyn std::error::Error>> {
        let request = TradesRequest::builder()
            .filter(MarketFilter::markets([market.condition_id]))
            .limit(self.config.api_limit)?
            .taker_only(self.config.taker_only)
            .build();

        let trades = self.data.trades(&request).await?;
        let fetched = trades.len() as u64;
        let (inserted, skipped) = persist_trades(
            &self.pool,
            &trades,
            self.config.trade_lookback_secs,
            Utc::now(),
        )
        .await?;

        Ok((fetched, inserted, skipped))
    }
}

async fn persist_trades(
    pool: &PgPool,
    trades: &[Trade],
    trade_lookback_secs: i64,
    now: DateTime<Utc>,
) -> Result<(u64, u64), sqlx::Error> {
    let mut inserted = 0_u64;
    let mut skipped = 0_u64;

    for trade in trades {
        let Some(side) = trade_side(&trade.side) else {
            skipped += 1;
            continue;
        };
        let Some(trade_ts) = trade_timestamp(trade.timestamp) else {
            skipped += 1;
            continue;
        };
        if (now - trade_ts).num_seconds() > trade_lookback_secs {
            skipped += 1;
            continue;
        }

        let result = sqlx::query(
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
                source,
                received_at
            ) VALUES (
                'Crypto',
                $1,
                $2,
                $3,
                $4,
                $5,
                $6,
                $7,
                $8,
                $9,
                $10,
                $11,
                $12,
                $13,
                'polymarket_data_api',
                NOW()
            )
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(trade.condition_id.to_string())
        .bind(trade.asset.to_string())
        .bind(side)
        .bind(trade.size)
        .bind(trade.price)
        .bind(trade_ts)
        .bind(trade.timestamp)
        .bind(trade.transaction_hash.to_string())
        .bind(trade.proxy_wallet.to_string())
        .bind(&trade.title)
        .bind(&trade.slug)
        .bind(&trade.outcome)
        .bind(trade.outcome_index)
        .execute(pool)
        .await?;

        inserted += result.rows_affected();
    }

    Ok((inserted, skipped))
}

fn trade_side(side: &Side) -> Option<&'static str> {
    match side {
        Side::Buy => Some("BUY"),
        Side::Sell => Some("SELL"),
        Side::Unknown(_) => None,
        _ => None,
    }
}

fn trade_timestamp(timestamp: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(timestamp, 0)
}

fn stale_for_too_long(last_success_at: Option<DateTime<Utc>>, stale_after_secs: u64) -> bool {
    match last_success_at {
        None => true,
        Some(last_success_at) => {
            (Utc::now() - last_success_at).num_seconds() >= stale_after_secs as i64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        trade_side, trade_timestamp, TradeCollectorConfig, DEFAULT_API_LIMIT,
        DEFAULT_MARKET_LOOKAHEAD_SECS, DEFAULT_MARKET_LOOKBACK_SECS, DEFAULT_REFRESH_INTERVAL_SECS,
        DEFAULT_STALE_AFTER_SECS, DEFAULT_TRADE_LOOKBACK_SECS,
    };
    use polymarket_client_sdk::data::types::Side;

    #[test]
    fn trade_collector_config_fills_safe_defaults() {
        let config = TradeCollectorConfig {
            symbols: Vec::new(),
            refresh_interval_secs: 0,
            market_lookback_secs: 0,
            market_lookahead_secs: 0,
            trade_lookback_secs: 0,
            api_limit: 0,
            per_market_delay_ms: 0,
            stale_after_secs: 0,
            taker_only: true,
        }
        .with_safe_defaults();

        assert_eq!(
            config.symbols,
            vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(),
                "SOLUSDT".to_string()
            ]
        );
        assert_eq!(config.refresh_interval_secs, DEFAULT_REFRESH_INTERVAL_SECS);
        assert_eq!(config.market_lookback_secs, DEFAULT_MARKET_LOOKBACK_SECS);
        assert_eq!(config.market_lookahead_secs, DEFAULT_MARKET_LOOKAHEAD_SECS);
        assert_eq!(config.trade_lookback_secs, DEFAULT_TRADE_LOOKBACK_SECS);
        assert_eq!(config.api_limit, DEFAULT_API_LIMIT);
        assert_eq!(config.stale_after_secs, DEFAULT_STALE_AFTER_SECS);
    }

    #[test]
    fn trade_side_maps_only_table_supported_sides() {
        assert_eq!(trade_side(&Side::Buy), Some("BUY"));
        assert_eq!(trade_side(&Side::Sell), Some("SELL"));
        assert_eq!(trade_side(&Side::Unknown("MINT".to_string())), None);
    }

    #[test]
    fn trade_timestamp_accepts_unix_seconds() {
        let ts = trade_timestamp(1_712_205_600).expect("valid timestamp");
        assert_eq!(ts.to_rfc3339(), "2024-04-04T04:40:00+00:00");
    }
}
