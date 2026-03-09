use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::domain::Side;
use crate::platform::Domain;

/// RL CLI domain events used by the compatibility runtime.
#[derive(Debug, Clone)]
pub enum DomainEvent {
    Sports(SportsEvent),
    Crypto(CryptoEvent),
    Politics(PoliticsEvent),
    QuoteUpdate(QuoteUpdateEvent),
    OrderUpdate(OrderUpdateEvent),
    Tick(DateTime<Utc>),
}

impl DomainEvent {
    pub fn domain(&self) -> Domain {
        match self {
            DomainEvent::Sports(_) => Domain::Sports,
            DomainEvent::Crypto(_) => Domain::Crypto,
            DomainEvent::Politics(_) => Domain::Politics,
            DomainEvent::QuoteUpdate(e) => e.domain,
            DomainEvent::OrderUpdate(e) => e.domain,
            DomainEvent::Tick(_) => Domain::Crypto,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SportsEvent {
    pub game_id: String,
    pub market_slug: String,
    pub teams: (String, String),
    pub league: String,
    pub game_time: Option<DateTime<Utc>>,
    pub quotes: Option<QuoteData>,
    pub odds_update: Option<OddsData>,
    pub injury_news: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct CryptoEvent {
    pub symbol: String,
    pub spot_price: Decimal,
    pub round_slug: Option<String>,
    pub quotes: Option<QuoteData>,
    pub momentum: Option<[f64; 4]>,
}

#[derive(Debug, Clone)]
pub struct PoliticsEvent {
    pub event_id: String,
    pub market_slug: String,
    pub description: String,
    pub quotes: Option<QuoteData>,
    pub poll_data: Option<PollData>,
}

#[derive(Debug, Clone)]
pub struct QuoteData {
    pub up_bid: Decimal,
    pub up_ask: Decimal,
    pub down_bid: Decimal,
    pub down_ask: Decimal,
    pub timestamp: DateTime<Utc>,
}

impl QuoteData {
    pub fn sum_of_asks(&self) -> Decimal {
        self.up_ask + self.down_ask
    }

    pub fn spread(&self, side: Side) -> Decimal {
        match side {
            Side::Up => self.up_ask - self.up_bid,
            Side::Down => self.down_ask - self.down_bid,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OddsData {
    pub spread: Option<f64>,
    pub over_under: Option<f64>,
    pub moneyline: Option<(i32, i32)>,
}

#[derive(Debug, Clone)]
pub struct PollData {
    pub candidate1_pct: f64,
    pub candidate2_pct: f64,
    pub margin_of_error: f64,
    pub source: String,
    pub date: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct QuoteUpdateEvent {
    pub domain: Domain,
    pub market_slug: String,
    pub token_id: String,
    pub side: Side,
    pub bid: Decimal,
    pub ask: Decimal,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct OrderUpdateEvent {
    pub domain: Domain,
    pub order_id: String,
    pub client_order_id: String,
    pub status: String,
    pub filled_shares: u64,
    pub avg_price: Option<Decimal>,
    pub timestamp: DateTime<Utc>,
}
