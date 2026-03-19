use super::DataFeedManager;
use chrono::Utc;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, error, info, warn};

use crate::collector::{BinanceDepthStream, BinanceKlineClient};
use crate::error::Result;
use crate::strategy::traits::{DataFeed, KlineBar, MarketUpdate};

impl DataFeedManager {
    async fn backfill_binance_klines(&self) -> Result<()> {
        let limit = self.binance_kline_backfill_limit;
        if limit == 0 {
            return Ok(());
        }

        if self.binance_kline_symbols.is_empty() || self.binance_kline_intervals.is_empty() {
            return Ok(());
        }

        let now = Utc::now();
        info!(
            "Backfilling Binance klines: symbols={:?} intervals={:?} limit={}",
            self.binance_kline_symbols, self.binance_kline_intervals, limit
        );

        let client = BinanceKlineClient::new();
        let mut sent: u64 = 0;

        for sym in &self.binance_kline_symbols {
            for interval in &self.binance_kline_intervals {
                let mut klines = match client.fetch_klines(sym, interval, limit).await {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(
                            "Binance kline backfill failed for {} {}: {}",
                            sym, interval, e
                        );
                        continue;
                    }
                };

                klines.retain(|k| k.close_time <= now);
                klines.sort_by_key(|k| k.open_time);

                if let Some(last) = klines.last() {
                    let mut map = self.binance_kline_last_close.write().await;
                    map.entry(sym.clone())
                        .or_default()
                        .insert(interval.clone(), last.close_time);
                }

                if let Some(ref pool) = self.metadata_pool {
                    match BinanceKlineClient::save_klines_to_db(pool, sym, interval, &klines).await
                    {
                        Ok(n) if n > 0 => {
                            info!("Persisted {} klines for {} {} to DB", n, sym, interval);
                        }
                        Err(e) => {
                            debug!("kline DB persist skipped for {} {}: {}", sym, interval, e);
                        }
                        _ => {}
                    }
                }

                info!(
                    "Backfilled {} klines for {} {}",
                    klines.len(),
                    sym,
                    interval
                );

                let sym_s = sym.clone();
                let interval_s = interval.clone();
                for k in klines {
                    let bar = KlineBar {
                        open_time: k.open_time,
                        close_time: k.close_time,
                        open: k.open,
                        high: k.high,
                        low: k.low,
                        close: k.close,
                        volume: k.volume,
                        is_closed: true,
                    };

                    let market_update = MarketUpdate::BinanceKline {
                        symbol: sym_s.clone(),
                        interval: interval_s.clone(),
                        kline: bar,
                        timestamp: k.close_time,
                    };
                    self.manager.send_market_update(market_update);
                    sent = sent.saturating_add(1);

                    if sent % 50 == 0 {
                        tokio::task::yield_now().await;
                    }
                }
            }
        }

        info!("Binance kline backfill complete ({} updates)", sent);
        Ok(())
    }

    /// Start all configured data feeds
    pub async fn start(&self) -> Result<()> {
        info!("Starting data feed manager");

        if let Some(ref binance_ws) = self.binance_ws {
            let manager = self.manager.clone();
            let freshness = self.data_plane.as_ref().map(|dp| dp.freshness());
            let mut rx = binance_ws.subscribe();

            tokio::spawn(async move {
                info!("Binance price feed started");
                loop {
                    match rx.recv().await {
                        Ok(update) => {
                            let symbol = update.symbol;
                            let price = update.price;
                            let quantity = update.quantity;
                            let is_buyer_maker = update.is_buyer_maker;
                            let timestamp = update.timestamp;

                            let market_update = MarketUpdate::BinancePrice {
                                symbol: symbol.clone(),
                                price,
                                timestamp,
                            };
                            manager.send_market_update(market_update);

                            if let (Some(qty), Some(is_buyer_maker)) = (quantity, is_buyer_maker) {
                                manager.send_market_update(MarketUpdate::BinanceTrade {
                                    symbol,
                                    qty,
                                    is_buyer_maker,
                                    timestamp,
                                });
                            }
                        }
                        Err(RecvError::Lagged(n)) => {
                            warn!("Binance price feed lagged by {} messages", n);
                            if let Some(ref f) = freshness {
                                f.record_broadcast_lag(n as u64);
                            }
                            continue;
                        }
                        Err(RecvError::Closed) => {
                            warn!("Binance price feed ended");
                            break;
                        }
                    }
                }
            });

            if self.data_plane.is_none() {
                let ws = binance_ws.clone();
                tokio::spawn(async move {
                    if let Err(e) = ws.run().await {
                        error!("Binance WebSocket error: {}", e);
                    }
                });
            }
        }

        if let Some(ref binance_ws) = self.binance_kline_ws {
            self.backfill_binance_klines().await?;

            let manager = self.manager.clone();
            let freshness = self.data_plane.as_ref().map(|dp| dp.freshness());
            let mut rx = binance_ws.subscribe();
            let last_close = self.binance_kline_last_close.clone();

            tokio::spawn(async move {
                info!("Binance kline feed started");
                loop {
                    match rx.recv().await {
                        Ok(update) => {
                            let should_skip = {
                                let map = last_close.read().await;
                                map.get(&update.symbol)
                                    .and_then(|m| m.get(&update.interval))
                                    .map(|t| update.kline.close_time <= *t)
                                    .unwrap_or(false)
                            };
                            if should_skip {
                                continue;
                            }

                            {
                                let mut map = last_close.write().await;
                                map.entry(update.symbol.clone())
                                    .or_default()
                                    .insert(update.interval.clone(), update.kline.close_time);
                            }

                            let bar = KlineBar {
                                open_time: update.kline.open_time,
                                close_time: update.kline.close_time,
                                open: update.kline.open,
                                high: update.kline.high,
                                low: update.kline.low,
                                close: update.kline.close,
                                volume: update.kline.volume,
                                is_closed: update.kline.is_closed,
                            };

                            let market_update = MarketUpdate::BinanceKline {
                                symbol: update.symbol,
                                interval: update.interval,
                                kline: bar,
                                timestamp: update.event_time,
                            };
                            manager.send_market_update(market_update);
                        }
                        Err(RecvError::Lagged(n)) => {
                            warn!("Binance kline feed lagged by {} messages", n);
                            if let Some(ref f) = freshness {
                                f.record_broadcast_lag(n as u64);
                            }
                            continue;
                        }
                        Err(RecvError::Closed) => {
                            warn!("Binance kline feed ended");
                            break;
                        }
                    }
                }
            });

            if self.data_plane.is_none() {
                let ws = binance_ws.clone();
                tokio::spawn(async move {
                    if let Err(e) = ws.run().await {
                        error!("Binance kline WebSocket error: {}", e);
                    }
                });
            }
        }

        if let Some(ref pm_ws) = self.polymarket_ws {
            let manager = self.manager.clone();
            let freshness = self.data_plane.as_ref().map(|dp| dp.freshness());
            let mut rx = pm_ws.subscribe_updates();

            tokio::spawn(async move {
                info!("Polymarket quote feed started - waiting for quotes");
                let mut quote_count = 0u64;
                loop {
                    match rx.recv().await {
                        Ok(update) => {
                            quote_count += 1;
                            if quote_count <= 10 || quote_count % 5000 == 0 {
                                info!(
                                    "Feed forwarding quote #{}: {} {:?} bid={:?} ask={:?}",
                                    quote_count,
                                    &update.token_id[..8.min(update.token_id.len())],
                                    update.side,
                                    update.quote.best_bid,
                                    update.quote.best_ask
                                );
                            } else {
                                debug!(
                                    "Feed forwarding quote #{}: {} {:?} bid={:?} ask={:?}",
                                    quote_count,
                                    &update.token_id[..8.min(update.token_id.len())],
                                    update.side,
                                    update.quote.best_bid,
                                    update.quote.best_ask
                                );
                            }
                            let market_update = MarketUpdate::PolymarketQuote {
                                token_id: update.token_id,
                                side: update.side,
                                quote: update.quote,
                                timestamp: Utc::now(),
                            };
                            manager.send_market_update(market_update);
                        }
                        Err(RecvError::Lagged(n)) => {
                            warn!("Quote feed lagged by {} messages", n);
                            if let Some(ref f) = freshness {
                                f.record_broadcast_lag(n as u64);
                            }
                            continue;
                        }
                        Err(RecvError::Closed) => {
                            warn!("Quote feed recv error: channel closed");
                            break;
                        }
                    }
                }
                warn!("Polymarket quote feed ended");
            });
        }

        Ok(())
    }

    pub async fn subscribe_tokens(&self, token_ids: Vec<String>) -> Result<()> {
        if let Some(ref pm_ws) = self.polymarket_ws {
            info!("Subscribing to {} Polymarket tokens", token_ids.len());

            if self.data_plane.is_some() {
                debug!(
                    "Polymarket WS run is managed by PlatformDataPlane; skipping local ws.run()"
                );
                pm_ws.request_resubscribe();
                return Ok(());
            }

            let ws = pm_ws.clone();
            tokio::spawn(async move {
                if let Err(e) = ws.run(token_ids).await {
                    error!("Polymarket WebSocket error: {}", e);
                }
            });
        }
        Ok(())
    }

    pub async fn start_for_feeds(&self, feeds: Vec<DataFeed>) -> Result<Vec<String>> {
        let mut all_tokens = Vec::new();
        let mut series_ids_to_refresh: Vec<String> = Vec::new();
        let mut binance_l2_symbols: Vec<String> = Vec::new();

        for feed in feeds {
            match feed {
                DataFeed::BinanceSpot { symbols } => {
                    if self.binance_ws.is_some() {
                        info!("Starting Binance feed for: {:?}", symbols);
                    }
                    binance_l2_symbols.extend(symbols);
                }
                DataFeed::BinanceKlines {
                    symbols,
                    intervals,
                    closed_only,
                } => {
                    if self.binance_kline_ws.is_some() {
                        info!(
                            "Starting Binance kline feed for: symbols={:?} intervals={:?} closed_only={}",
                            symbols, intervals, closed_only
                        );
                    }
                }
                DataFeed::PolymarketEvents { series_ids } => {
                    for series_id in series_ids {
                        series_ids_to_refresh.push(series_id.clone());
                        let tokens = self.discover_series_events(&series_id).await?;
                        all_tokens.extend(tokens);
                    }
                }
                DataFeed::PolymarketQuotes { tokens } => {
                    all_tokens.extend(tokens);
                }
                DataFeed::Tick { interval_ms } => {
                    debug!("Tick feed configured: {}ms", interval_ms);
                }
            }
        }

        self.ensure_binance_l2_feed_started(binance_l2_symbols)
            .await;

        if self.polymarket_ws.is_some() {
            if series_ids_to_refresh.is_empty() {
                if !all_tokens.is_empty() {
                    self.subscribe_tokens(all_tokens.clone()).await?;
                }
            } else {
                self.subscribe_tokens(Vec::new()).await?;
            }
        }

        if !series_ids_to_refresh.is_empty() {
            self.spawn_polymarket_refresh(series_ids_to_refresh).await;
        }

        Ok(all_tokens)
    }

    async fn ensure_binance_l2_feed_started(&self, mut symbols: Vec<String>) {
        if !super::l2_feed_enabled() {
            return;
        }

        symbols.retain(|s| !s.trim().is_empty());
        symbols.sort();
        symbols.dedup();
        if symbols.is_empty() {
            return;
        }

        {
            let mut started = self.binance_l2_started.write().await;
            if *started {
                return;
            }
            *started = true;
        }

        let manager = self.manager.clone();
        let freshness = self.data_plane.as_ref().map(|dp| dp.freshness());
        let depth_ws = std::sync::Arc::new(match freshness.clone() {
            Some(f) => BinanceDepthStream::new(symbols.clone()).with_freshness(f),
            None => BinanceDepthStream::new(symbols.clone()),
        });
        let mut rx = depth_ws.subscribe();

        tokio::spawn(async move {
            info!("Binance L2 depth feed started for {:?}", symbols);
            loop {
                match rx.recv().await {
                    Ok(update) => {
                        let market_update = MarketUpdate::BinanceL2 {
                            symbol: update.symbol,
                            obi_1: update.snapshot.obi_1,
                            obi_2: update.snapshot.obi_2,
                            obi_3: update.snapshot.obi_3,
                            obi_5: update.snapshot.obi_5,
                            obi_10: update.snapshot.obi_10,
                            obi_20: update.snapshot.obi_20,
                            bid_volume_5: update.snapshot.bid_volume_5,
                            ask_volume_5: update.snapshot.ask_volume_5,
                            spread_bps: update.snapshot.spread_bps,
                            timestamp: update.snapshot.timestamp,
                        };
                        manager.send_market_update(market_update);
                    }
                    Err(RecvError::Lagged(n)) => {
                        warn!("Binance L2 depth feed lagged by {} messages", n);
                        if let Some(ref f) = freshness {
                            f.record_broadcast_lag(n as u64);
                        }
                    }
                    Err(RecvError::Closed) => {
                        warn!("Binance L2 depth feed ended");
                        break;
                    }
                }
            }
        });

        tokio::spawn(async move {
            if let Err(e) = depth_ws.run().await {
                error!("Binance L2 depth WebSocket error: {}", e);
            }
        });
    }
}
