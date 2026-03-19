use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::domain::Domain;
use crate::domain::Side;

/// RL CLI domain events used by the compatibility runtime.
#[derive(Debug, Clone)]
pub enum DomainEvent {
    Crypto(CryptoEvent),
    Tick(DateTime<Utc>),
}

impl DomainEvent {
    pub fn domain(&self) -> Domain {
        match self {
            DomainEvent::Crypto(_) => Domain::Crypto,
            DomainEvent::Tick(_) => Domain::Crypto,
        }
    }
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
