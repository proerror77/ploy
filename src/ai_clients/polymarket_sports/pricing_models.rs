use rust_decimal::Decimal;
use serde::Deserialize;

use super::PolymarketSportsMarket;

/// Order book level from CLOB
#[derive(Debug, Clone, Deserialize)]
pub struct OrderBookLevel {
    pub price: String,
    pub size: String,
}

/// Order book response from CLOB API
#[derive(Debug, Clone, Deserialize)]
pub struct SportsOrderBook {
    pub market: Option<String>,
    pub asset_id: String,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub timestamp: Option<String>,
}

impl SportsOrderBook {
    /// Get best bid price
    pub fn best_bid(&self) -> Option<Decimal> {
        self.bids.first()?.price.parse().ok()
    }

    /// Get best ask price
    pub fn best_ask(&self) -> Option<Decimal> {
        self.asks.first()?.price.parse().ok()
    }

    /// Get mid price
    pub fn mid_price(&self) -> Option<Decimal> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some((bid + ask) / Decimal::from(2))
    }

    /// Get spread
    pub fn spread(&self) -> Option<Decimal> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some(ask - bid)
    }

    /// Calculate implied probability from YES token price
    pub fn implied_probability(&self) -> Option<Decimal> {
        self.mid_price()
    }
}

/// Sports market with full trading details
#[derive(Debug, Clone)]
pub struct SportsMarketDetails {
    pub market: PolymarketSportsMarket,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub yes_book: Option<SportsOrderBook>,
    pub no_book: Option<SportsOrderBook>,
}

impl SportsMarketDetails {
    /// Get current YES price (implied probability for home/favorite)
    pub fn yes_price(&self) -> Option<Decimal> {
        self.yes_book.as_ref()?.mid_price()
    }

    /// Get current NO price (implied probability against)
    pub fn no_price(&self) -> Option<Decimal> {
        self.no_book.as_ref()?.mid_price()
    }
}
