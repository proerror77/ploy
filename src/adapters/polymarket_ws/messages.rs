use super::{PolymarketWebSocket, QuoteUpdate};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, warn};

/// Order book message from WebSocket
#[derive(Debug, Clone, Deserialize)]
pub struct BookMessage {
    pub asset_id: String,
    pub market: String,
    #[serde(default)]
    pub bids: Vec<PriceLevel>,
    #[serde(default)]
    pub asks: Vec<PriceLevel>,
    pub timestamp: Option<String>,
    pub hash: Option<String>,
}

/// Price change message from WebSocket
#[derive(Debug, Clone, Deserialize)]
pub struct PriceChangesMessage {
    pub market: String,
    pub price_changes: Vec<PriceChangeItem>,
}

/// Individual price change item
#[derive(Debug, Clone, Deserialize)]
pub struct PriceChangeItem {
    pub asset_id: String,
    pub price: String,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct PriceLevel {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceChangeEntry {
    pub interval: String,
    pub change: String,
}

fn parse_price_level(level: &PriceLevel) -> Option<(Decimal, Decimal)> {
    let price = level.price.parse::<Decimal>().ok()?;
    let size = level.size.parse::<Decimal>().ok()?;
    Some((price, size))
}

fn extract_best_and_total(
    levels: &[PriceLevel],
    pick_best: impl Fn(Decimal, Decimal) -> Decimal,
) -> (Option<Decimal>, Decimal) {
    let mut best: Option<Decimal> = None;
    let mut total_size = Decimal::ZERO;

    for lvl in levels {
        let Some((price, size)) = parse_price_level(lvl) else {
            continue;
        };

        total_size += size;
        best = Some(match best {
            Some(current) => pick_best(current, price),
            None => price,
        });
    }

    (best, total_size)
}

pub(super) fn extract_book_top(
    book: &BookMessage,
) -> (
    Option<Decimal>,
    Option<Decimal>,
    Option<Decimal>,
    Option<Decimal>,
) {
    let (best_bid, bid_total) = extract_best_and_total(&book.bids, |a, b| a.max(b));
    let (best_ask, ask_total) = extract_best_and_total(&book.asks, |a, b| a.min(b));

    let bid_total = if bid_total > Decimal::ZERO {
        Some(bid_total)
    } else {
        None
    };
    let ask_total = if ask_total > Decimal::ZERO {
        Some(ask_total)
    } else {
        None
    };

    (best_bid, best_ask, bid_total, ask_total)
}

impl PolymarketWebSocket {
    pub(super) async fn handle_message(&self, text: &str) -> bool {
        // Log first few chars for debugging
        let preview = &text[..text.len().min(200)];
        debug!("WS message received: {}", preview);

        if let Ok(books) = serde_json::from_str::<Vec<BookMessage>>(text) {
            if books.is_empty() {
                debug!("Received empty book updates array");
                return false;
            }
            debug!("Received {} book updates", books.len());
            for book in books {
                self.process_book_message(book).await;
            }
            return true;
        }

        if let Ok(price_msg) = serde_json::from_str::<PriceChangesMessage>(text) {
            debug!("Received price changes for market: {}", price_msg.market);
            let has_data = !price_msg.price_changes.is_empty();
            self.process_price_changes(price_msg).await;
            return has_data;
        }

        if let Ok(book) = serde_json::from_str::<BookMessage>(text) {
            debug!("Received single book update for: {}", book.asset_id);
            self.process_book_message(book).await;
            return true;
        }

        // Unknown format - log for debugging (include more of message)
        warn!("Unknown WS message format: {}", preview);
        false
    }

    /// Process an order book message
    pub(super) async fn process_book_message(&self, book: BookMessage) {
        let asset_id = book.asset_id.clone();

        let (best_bid, best_ask, bid_size, ask_size) = extract_book_top(&book);

        if let Some(side) = self.get_side(&asset_id).await {
            self.quote_cache
                .update_snapshot(&asset_id, side, best_bid, best_ask, bid_size, ask_size);

            if let Some(f) = self.freshness.get() {
                f.record_update(crate::data_plane::DataSource::PolymarketWs, &asset_id);
            }

            if let Some(quote) = self.quote_cache.get(&asset_id) {
                let update = QuoteUpdate {
                    token_id: asset_id.clone(),
                    side,
                    quote,
                };
                match self.update_tx.send(update) {
                    Ok(n) => debug!(
                        "Quote broadcast to {} receivers: {} {:?} bid={:?} ask={:?}",
                        n,
                        side,
                        &asset_id[..8.min(asset_id.len())],
                        best_bid,
                        best_ask
                    ),
                    Err(_) => warn!("No receivers for quote update - channel closed"),
                }
            }

            debug!(
                "Book update {}: bid={:?} ask={:?}",
                side, best_bid, best_ask
            );
        } else {
            let is_extra = {
                let extra = self.extra_tokens.read().await;
                extra.contains(&asset_id)
            };
            if !is_extra {
                let registered_count = self.token_to_side.read().await.len();
                debug!(
                    "Unregistered token in book update: {} (registered tokens: {})",
                    &asset_id[..16.min(asset_id.len())],
                    registered_count
                );
            }
        }

        let _ = self.book_tx.send(Arc::new(book));
    }

    /// Process price changes message
    pub(super) async fn process_price_changes(&self, msg: PriceChangesMessage) {
        for change in msg.price_changes {
            if let (Some(side), Ok(price)) = (
                self.get_side(&change.asset_id).await,
                change.price.parse::<Decimal>(),
            ) {
                debug!("Price change {}: {}", side, price);
                self.quote_cache
                    .update(&change.asset_id, side, None, None, None, None);

                if let Some(quote) = self.quote_cache.get(&change.asset_id) {
                    if quote.best_bid.is_some() || quote.best_ask.is_some() {
                        let update = QuoteUpdate {
                            token_id: change.asset_id.clone(),
                            side,
                            quote,
                        };
                        let _ = self.update_tx.send(update);
                    }
                }
            }
        }
    }
}
