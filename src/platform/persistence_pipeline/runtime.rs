use super::*;

use tokio::time::{self, Duration};
use tracing::info;

#[derive(Debug, Clone)]
struct QuoteState {
    last_at: DateTime<Utc>,
    best_bid: Option<Decimal>,
    best_ask: Option<Decimal>,
    bid_size: Option<Decimal>,
    ask_size: Option<Decimal>,
}

#[derive(Debug, Clone)]
struct PriceState {
    last_at: DateTime<Utc>,
    price: Option<Decimal>,
    quantity: Option<Decimal>,
}

#[derive(Debug, Clone)]
struct LobState {
    last_at: DateTime<Utc>,
    last_update_id: i64,
}

#[derive(Debug, Clone)]
struct OrderbookState {
    last_at: DateTime<Utc>,
    last_hash: String,
}

/// Internal dedup tracker.
#[derive(Debug, Default)]
struct DedupState {
    quotes: HashMap<String, QuoteState>,
    prices: HashMap<String, PriceState>,
    lobs: HashMap<String, LobState>,
    orderbooks: HashMap<String, OrderbookState>,
}

#[derive(Debug, Default)]
struct PendingBuffers {
    quotes: Vec<ClobQuoteTick>,
    price_changes: Vec<ClobPriceChangeTick>,
    prices: Vec<BinancePriceTick>,
    lobs: Vec<BinanceLobTick>,
    chainlink_prices: Vec<ChainlinkPriceTick>,
    orderbooks: Vec<ClobOrderbookSnapshot>,
}

impl PendingBuffers {
    fn len(&self) -> usize {
        self.quotes.len()
            + self.price_changes.len()
            + self.prices.len()
            + self.lobs.len()
            + self.chainlink_prices.len()
            + self.orderbooks.len()
    }

    fn is_empty(&self) -> bool {
        self.quotes.is_empty()
            && self.price_changes.is_empty()
            && self.prices.is_empty()
            && self.lobs.is_empty()
            && self.chainlink_prices.is_empty()
            && self.orderbooks.is_empty()
    }
}

impl PersistencePipeline {
    pub(super) async fn run(
        mut rx: mpsc::Receiver<PersistenceEvent>,
        pool: PgPool,
        config: PersistenceConfig,
    ) {
        let mut dedup = DedupState::default();
        let mut buffers = PendingBuffers::default();
        let mut stats = PipelineStats::default();
        let mut log_counter: u64 = 0;
        let mut flush_interval =
            time::interval(Duration::from_millis(config.flush_interval_ms.max(1)));
        flush_interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

        info!(
            "persistence pipeline started (capacity={})",
            config.channel_capacity
        );

        loop {
            tokio::select! {
                maybe_event = rx.recv() => {
                    match maybe_event {
                        Some(event) => {
                            Self::enqueue_event(event, &mut dedup, &config, &mut stats, &mut buffers);
                            log_counter += 1;
                        }
                        None => break,
                    }
                }
                _ = flush_interval.tick() => {
                    if !buffers.is_empty() {
                        Self::flush_buffers(&pool, &config, &mut buffers, &mut stats).await;
                    }
                }
            }

            if buffers.len() >= config.max_batch_size.max(1) {
                Self::flush_buffers(&pool, &config, &mut buffers, &mut stats).await;
            }

            if log_counter > 0 && log_counter.is_multiple_of(1000) {
                debug!(
                    quotes = stats.clob_quotes_persisted,
                    quotes_dedup = stats.clob_quotes_deduped,
                    price_changes = stats.clob_price_changes_persisted,
                    prices = stats.binance_prices_persisted,
                    lobs = stats.binance_lobs_persisted,
                    chainlink = stats.chainlink_prices_persisted,
                    orderbooks = stats.clob_orderbooks_persisted,
                    pending = buffers.len(),
                    "persistence pipeline stats"
                );
            }
        }

        if !buffers.is_empty() {
            Self::flush_buffers(&pool, &config, &mut buffers, &mut stats).await;
        }

        info!("persistence pipeline shutting down");
    }

    fn enqueue_event(
        event: PersistenceEvent,
        dedup: &mut DedupState,
        config: &PersistenceConfig,
        stats: &mut PipelineStats,
        buffers: &mut PendingBuffers,
    ) {
        match event {
            PersistenceEvent::ClobQuote(tick) => {
                if Self::should_persist_quote(&tick, dedup, config) {
                    buffers.quotes.push(tick);
                } else {
                    stats.clob_quotes_deduped += 1;
                }
            }
            PersistenceEvent::ClobPriceChange(tick) => {
                buffers.price_changes.push(tick);
            }
            PersistenceEvent::BinancePrice(tick) => {
                if Self::should_persist_price(&tick, dedup, config) {
                    buffers.prices.push(tick);
                } else {
                    stats.binance_prices_deduped += 1;
                }
            }
            PersistenceEvent::BinanceLob(tick) => {
                if Self::should_persist_lob(&tick, dedup, config) {
                    buffers.lobs.push(tick);
                } else {
                    stats.binance_lobs_deduped += 1;
                }
            }
            PersistenceEvent::ChainlinkPrice(tick) => {
                buffers.chainlink_prices.push(tick);
            }
            PersistenceEvent::ClobOrderbook(snap) => {
                if Self::should_persist_orderbook(&snap, dedup, config) {
                    buffers.orderbooks.push(snap);
                } else {
                    stats.clob_orderbooks_deduped += 1;
                }
            }
        }
    }

    async fn flush_buffers(
        pool: &PgPool,
        config: &PersistenceConfig,
        buffers: &mut PendingBuffers,
        stats: &mut PipelineStats,
    ) {
        for tick in buffers.quotes.drain(..) {
            if let Err(e) = Self::write_clob_quote(pool, &tick).await {
                warn!(error = %e, token = %tick.token_id, "clob quote persist failed");
            } else {
                stats.clob_quotes_persisted += 1;
            }
        }

        for tick in buffers.price_changes.drain(..) {
            if let Err(e) = Self::write_clob_price_change(pool, &tick).await {
                warn!(error = %e, token = %tick.token_id, "clob price-change persist failed");
            } else {
                stats.clob_price_changes_persisted += 1;
            }
        }

        for tick in buffers.prices.drain(..) {
            if let Err(e) = Self::write_binance_price(pool, &tick).await {
                warn!(error = %e, symbol = %tick.symbol, "binance price persist failed");
            } else {
                stats.binance_prices_persisted += 1;
            }
        }

        for tick in buffers.lobs.drain(..) {
            if let Err(e) =
                Self::write_binance_lob(pool, &tick, config.binance_lob_max_levels).await
            {
                warn!(error = %e, symbol = %tick.symbol, "binance lob persist failed");
            } else {
                stats.binance_lobs_persisted += 1;
            }
        }

        for tick in buffers.chainlink_prices.drain(..) {
            if let Err(e) = Self::write_chainlink_price(pool, &tick).await {
                warn!(error = %e, symbol = %tick.symbol, "chainlink price persist failed");
            } else {
                stats.chainlink_prices_persisted += 1;
            }
        }

        for snap in buffers.orderbooks.drain(..) {
            if let Err(e) =
                Self::write_clob_orderbook(pool, &snap, config.clob_orderbook_max_levels).await
            {
                warn!(error = %e, token = %snap.token_id, "clob orderbook persist failed");
            } else {
                stats.clob_orderbooks_persisted += 1;
            }
        }
    }

    fn should_persist_quote(
        tick: &ClobQuoteTick,
        dedup: &mut DedupState,
        config: &PersistenceConfig,
    ) -> bool {
        if tick.best_bid.is_none() && tick.best_ask.is_none() {
            return false;
        }

        let now = tick.received_at;
        if let Some(prev) = dedup.quotes.get(&tick.token_id) {
            let elapsed = (now - prev.last_at).num_seconds();
            let changed = prev.best_bid != tick.best_bid
                || prev.best_ask != tick.best_ask
                || prev.bid_size != tick.bid_size
                || prev.ask_size != tick.ask_size;
            if !changed || elapsed < config.clob_quote_min_interval_secs {
                return false;
            }
        }

        dedup.quotes.insert(
            tick.token_id.clone(),
            QuoteState {
                last_at: now,
                best_bid: tick.best_bid,
                best_ask: tick.best_ask,
                bid_size: tick.bid_size,
                ask_size: tick.ask_size,
            },
        );
        true
    }

    fn should_persist_price(
        tick: &BinancePriceTick,
        dedup: &mut DedupState,
        config: &PersistenceConfig,
    ) -> bool {
        let now = tick.trade_time;
        if let Some(prev) = dedup.prices.get(&tick.symbol) {
            let elapsed = (now - prev.last_at).num_seconds();
            let changed = prev.price != tick.price || prev.quantity != tick.quantity;
            if !changed || elapsed < config.binance_price_min_interval_secs {
                return false;
            }
        }

        dedup.prices.insert(
            tick.symbol.clone(),
            PriceState {
                last_at: now,
                price: tick.price,
                quantity: tick.quantity,
            },
        );
        true
    }

    fn should_persist_lob(
        tick: &BinanceLobTick,
        dedup: &mut DedupState,
        config: &PersistenceConfig,
    ) -> bool {
        let now = tick.event_time;
        if let Some(prev) = dedup.lobs.get(&tick.symbol) {
            let elapsed_ms = (now - prev.last_at).num_milliseconds();
            if elapsed_ms < config.binance_lob_snapshot_interval_ms
                || prev.last_update_id == tick.update_id
            {
                return false;
            }
        }

        dedup.lobs.insert(
            tick.symbol.clone(),
            LobState {
                last_at: now,
                last_update_id: tick.update_id,
            },
        );
        true
    }

    fn should_persist_orderbook(
        snap: &ClobOrderbookSnapshot,
        dedup: &mut DedupState,
        config: &PersistenceConfig,
    ) -> bool {
        let now = Utc::now();
        if let Some(prev) = dedup.orderbooks.get(&snap.token_id) {
            let elapsed_ms = (now - prev.last_at).num_milliseconds();
            if elapsed_ms < config.clob_orderbook_snapshot_interval_ms {
                return false;
            }
            if config.clob_orderbook_require_hash_change && prev.last_hash == snap.hash {
                return false;
            }
        }

        dedup.orderbooks.insert(
            snap.token_id.clone(),
            OrderbookState {
                last_at: now,
                last_hash: snap.hash.clone(),
            },
        );
        true
    }

    async fn write_clob_quote(pool: &PgPool, tick: &ClobQuoteTick) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO clob_quote_ticks
               (token_id, side, best_bid, best_ask, bid_size, ask_size, source, domain)
               VALUES ($1, $2, $3, $4, $5, $6, 'polymarket_ws', $7)
               ON CONFLICT DO NOTHING"#,
        )
        .bind(&tick.token_id)
        .bind(&tick.side)
        .bind(tick.best_bid)
        .bind(tick.best_ask)
        .bind(tick.bid_size)
        .bind(tick.ask_size)
        .bind(tick.domain.to_string())
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn write_binance_price(
        pool: &PgPool,
        tick: &BinancePriceTick,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO binance_price_ticks
               (symbol, price, quantity, trade_time)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT DO NOTHING"#,
        )
        .bind(&tick.symbol)
        .bind(tick.price)
        .bind(tick.quantity)
        .bind(tick.trade_time)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn write_clob_price_change(
        pool: &PgPool,
        tick: &ClobPriceChangeTick,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO clob_price_change_ticks
               (token_id, market, side, price, domain, received_at)
               VALUES ($1, $2, $3, $4, $5, $6)
               ON CONFLICT DO NOTHING"#,
        )
        .bind(&tick.token_id)
        .bind(&tick.market)
        .bind(&tick.side)
        .bind(tick.price)
        .bind(tick.domain.to_string())
        .bind(tick.received_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn write_binance_lob(
        pool: &PgPool,
        tick: &BinanceLobTick,
        _max_levels: usize,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO binance_lob_ticks
               (symbol, update_id, best_bid, best_ask, mid_price, spread_bps,
                obi_5, obi_10, bid_volume_5, ask_volume_5, bids, asks, event_time)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
               ON CONFLICT DO NOTHING"#,
        )
        .bind(&tick.symbol)
        .bind(tick.update_id)
        .bind(tick.best_bid)
        .bind(tick.best_ask)
        .bind(tick.mid_price)
        .bind(tick.spread_bps)
        .bind(tick.obi_5)
        .bind(tick.obi_10)
        .bind(tick.bid_volume_5)
        .bind(tick.ask_volume_5)
        .bind(&tick.bids)
        .bind(&tick.asks)
        .bind(tick.event_time)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn write_chainlink_price(
        pool: &PgPool,
        tick: &ChainlinkPriceTick,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO chainlink_price_ticks
               (symbol, price, source_timestamp)
               VALUES ($1, $2, $3)
               ON CONFLICT DO NOTHING"#,
        )
        .bind(&tick.symbol)
        .bind(tick.price)
        .bind(tick.source_timestamp)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn write_clob_orderbook(
        pool: &PgPool,
        snap: &ClobOrderbookSnapshot,
        _max_levels: usize,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO clob_orderbook_snapshots
               (domain, token_id, market, bids, asks, book_timestamp, hash, source, context)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               ON CONFLICT DO NOTHING"#,
        )
        .bind(snap.domain.to_string())
        .bind(&snap.token_id)
        .bind(&snap.market)
        .bind(&snap.bids)
        .bind(&snap.asks)
        .bind(snap.book_timestamp)
        .bind(&snap.hash)
        .bind(&snap.source)
        .bind(&snap.context)
        .execute(pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn default_config() -> PersistenceConfig {
        PersistenceConfig::default()
    }

    fn make_quote(
        token: &str,
        bid: Option<f64>,
        ask: Option<f64>,
        at: DateTime<Utc>,
    ) -> ClobQuoteTick {
        ClobQuoteTick {
            token_id: token.into(),
            side: "UP".into(),
            best_bid: bid.map(|v| Decimal::from_f64_retain(v).unwrap()),
            best_ask: ask.map(|v| Decimal::from_f64_retain(v).unwrap()),
            bid_size: Some(Decimal::from(100)),
            ask_size: Some(Decimal::from(100)),
            domain: Domain::Crypto,
            received_at: at,
        }
    }

    fn make_price(symbol: &str, price: f64, at: DateTime<Utc>) -> BinancePriceTick {
        BinancePriceTick {
            symbol: symbol.into(),
            price: Some(Decimal::from_f64_retain(price).unwrap()),
            quantity: Some(Decimal::from(1)),
            trade_time: at,
        }
    }

    #[test]
    fn quote_dedup_skips_unchanged_within_interval() {
        let config = default_config();
        let mut dedup = DedupState::default();
        let t0 = Utc::now();

        let q1 = make_quote("tok-1", Some(0.42), Some(0.45), t0);
        assert!(PersistencePipeline::should_persist_quote(
            &q1, &mut dedup, &config
        ));

        let q2 = make_quote("tok-1", Some(0.42), Some(0.45), t0 + Duration::seconds(1));
        assert!(!PersistencePipeline::should_persist_quote(
            &q2, &mut dedup, &config
        ));

        let q3 = make_quote("tok-1", Some(0.42), Some(0.45), t0 + Duration::seconds(3));
        assert!(!PersistencePipeline::should_persist_quote(
            &q3, &mut dedup, &config
        ));

        let q4 = make_quote("tok-1", Some(0.43), Some(0.45), t0 + Duration::seconds(3));
        assert!(PersistencePipeline::should_persist_quote(
            &q4, &mut dedup, &config
        ));
    }

    #[test]
    fn quote_dedup_skips_both_none() {
        let config = default_config();
        let mut dedup = DedupState::default();
        let q = ClobQuoteTick {
            token_id: "tok-1".into(),
            side: "UP".into(),
            best_bid: None,
            best_ask: None,
            bid_size: None,
            ask_size: None,
            domain: Domain::Crypto,
            received_at: Utc::now(),
        };
        assert!(!PersistencePipeline::should_persist_quote(
            &q, &mut dedup, &config
        ));
    }

    #[test]
    fn price_dedup_respects_interval_and_change() {
        let config = default_config();
        let mut dedup = DedupState::default();
        let t0 = Utc::now();

        let p1 = make_price("BTCUSDT", 50000.0, t0);
        assert!(PersistencePipeline::should_persist_price(
            &p1, &mut dedup, &config
        ));

        let p2 = make_price("BTCUSDT", 50000.0, t0 + Duration::milliseconds(500));
        assert!(!PersistencePipeline::should_persist_price(
            &p2, &mut dedup, &config
        ));

        let p3 = make_price("BTCUSDT", 50001.0, t0 + Duration::seconds(2));
        assert!(PersistencePipeline::should_persist_price(
            &p3, &mut dedup, &config
        ));
    }

    #[test]
    fn lob_dedup_requires_interval_and_new_update_id() {
        let config = default_config();
        let mut dedup = DedupState::default();
        let t0 = Utc::now();

        let l1 = BinanceLobTick {
            symbol: "BTCUSDT".into(),
            update_id: 100,
            best_bid: Some(Decimal::from(50000)),
            best_ask: Some(Decimal::from(50001)),
            mid_price: None,
            spread_bps: None,
            obi_5: None,
            obi_10: None,
            bid_volume_5: None,
            ask_volume_5: None,
            bids: serde_json::json!([]),
            asks: serde_json::json!([]),
            event_time: t0,
        };
        assert!(PersistencePipeline::should_persist_lob(
            &l1, &mut dedup, &config
        ));

        let mut l2 = l1.clone();
        l2.event_time = t0 + Duration::seconds(2);
        assert!(!PersistencePipeline::should_persist_lob(
            &l2, &mut dedup, &config
        ));

        let mut l3 = l1.clone();
        l3.update_id = 101;
        l3.event_time = t0 + Duration::seconds(2);
        assert!(PersistencePipeline::should_persist_lob(
            &l3, &mut dedup, &config
        ));
    }

    #[test]
    fn orderbook_dedup_respects_hash_change() {
        let config = default_config();
        let mut dedup = DedupState::default();

        let s1 = ClobOrderbookSnapshot {
            domain: Domain::Crypto,
            token_id: "tok-1".into(),
            market: None,
            bids: serde_json::json!([]),
            asks: serde_json::json!([]),
            book_timestamp: None,
            hash: "abc123".into(),
            source: "polymarket_ws".into(),
            context: None,
        };
        assert!(PersistencePipeline::should_persist_orderbook(
            &s1, &mut dedup, &config
        ));

        std::thread::sleep(std::time::Duration::from_millis(10));
        dedup.orderbooks.get_mut("tok-1").unwrap().last_at = Utc::now() - Duration::seconds(10);
        assert!(!PersistencePipeline::should_persist_orderbook(
            &s1, &mut dedup, &config
        ));

        let mut s2 = s1.clone();
        s2.hash = "def456".into();
        assert!(PersistencePipeline::should_persist_orderbook(
            &s2, &mut dedup, &config
        ));
    }

    #[test]
    fn different_tokens_tracked_independently() {
        let config = default_config();
        let mut dedup = DedupState::default();
        let t0 = Utc::now();

        let q1 = make_quote("tok-1", Some(0.42), Some(0.45), t0);
        let q2 = make_quote("tok-2", Some(0.55), Some(0.58), t0);

        assert!(PersistencePipeline::should_persist_quote(
            &q1, &mut dedup, &config
        ));
        assert!(PersistencePipeline::should_persist_quote(
            &q2, &mut dedup, &config
        ));

        let q3 = make_quote("tok-1", Some(0.42), Some(0.45), t0 + Duration::seconds(1));
        let q4 = make_quote("tok-2", Some(0.56), Some(0.58), t0 + Duration::seconds(1));
        assert!(!PersistencePipeline::should_persist_quote(
            &q3, &mut dedup, &config
        ));
        assert!(!PersistencePipeline::should_persist_quote(
            &q4, &mut dedup, &config
        ));
    }
}
