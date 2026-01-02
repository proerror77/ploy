use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Side of the binary market (UP or DOWN)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Up,
    Down,
}

impl Side {
    /// Get the opposite side
    pub fn opposite(&self) -> Self {
        match self {
            Side::Up => Side::Down,
            Side::Down => Side::Up,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Up => "UP",
            Side::Down => "DOWN",
        }
    }
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A trading round (15-minute window)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Round {
    pub id: Option<i32>,
    pub slug: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub outcome: Option<Side>,
}

impl Round {
    /// Get token ID for a given side
    pub fn token_id(&self, side: Side) -> &str {
        match side {
            Side::Up => &self.up_token_id,
            Side::Down => &self.down_token_id,
        }
    }

    /// Seconds remaining until round ends
    pub fn seconds_remaining(&self) -> i64 {
        (self.end_time - Utc::now()).num_seconds().max(0)
    }

    /// Check if round is still active
    pub fn is_active(&self) -> bool {
        let now = Utc::now();
        now >= self.start_time && now < self.end_time
    }

    /// Check if round has ended
    pub fn has_ended(&self) -> bool {
        Utc::now() >= self.end_time
    }

    /// Minutes elapsed since round start
    pub fn minutes_elapsed(&self) -> i64 {
        (Utc::now() - self.start_time).num_minutes().max(0)
    }
}

/// Best bid/ask quote for one side
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Quote {
    pub side: Side,
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub bid_size: Option<Decimal>,
    pub ask_size: Option<Decimal>,
    pub timestamp: DateTime<Utc>,
}

impl Quote {
    /// Calculate spread in basis points
    pub fn spread_bps(&self) -> Option<u32> {
        match (self.best_bid, self.best_ask) {
            (Some(bid), Some(ask)) if bid > Decimal::ZERO => {
                let spread = (ask - bid) / bid * Decimal::from(10000);
                Some(spread.to_string().parse::<f64>().unwrap_or(0.0) as u32)
            }
            _ => None,
        }
    }

    /// Get mid price
    pub fn mid_price(&self) -> Option<Decimal> {
        match (self.best_bid, self.best_ask) {
            (Some(bid), Some(ask)) => Some((bid + ask) / Decimal::from(2)),
            (Some(bid), None) => Some(bid),
            (None, Some(ask)) => Some(ask),
            (None, None) => None,
        }
    }
}

/// Market snapshot with quotes for both sides
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshot {
    pub round: Round,
    pub up_quote: Quote,
    pub down_quote: Quote,
    pub timestamp: DateTime<Utc>,
}

impl MarketSnapshot {
    /// Get quote for a specific side
    pub fn quote(&self, side: Side) -> &Quote {
        match side {
            Side::Up => &self.up_quote,
            Side::Down => &self.down_quote,
        }
    }

    /// Get best ask for a side
    pub fn best_ask(&self, side: Side) -> Option<Decimal> {
        self.quote(side).best_ask
    }

    /// Get best bid for a side
    pub fn best_bid(&self, side: Side) -> Option<Decimal> {
        self.quote(side).best_bid
    }

    /// Calculate the sum of best asks for both sides
    pub fn ask_sum(&self) -> Option<Decimal> {
        match (self.up_quote.best_ask, self.down_quote.best_ask) {
            (Some(up), Some(down)) => Some(up + down),
            _ => None,
        }
    }

    /// Check if both sides have valid quotes
    pub fn is_valid(&self) -> bool {
        self.up_quote.best_ask.is_some()
            && self.down_quote.best_ask.is_some()
            && self.up_quote.best_bid.is_some()
            && self.down_quote.best_bid.is_some()
    }
}

/// Tick data for storage/backtesting
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tick {
    pub id: Option<i64>,
    pub round_id: i32,
    pub timestamp: DateTime<Utc>,
    pub side: Side,
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub bid_size: Option<Decimal>,
    pub ask_size: Option<Decimal>,
}

/// Signal indicating a dump was detected
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DumpSignal {
    /// Which side experienced the dump
    pub side: Side,
    /// Current price (best_ask) that triggered the signal
    pub trigger_price: Decimal,
    /// Reference price (rolling high)
    pub reference_price: Decimal,
    /// Percentage drop from reference
    pub drop_pct: Decimal,
    /// When the signal was detected
    pub timestamp: DateTime<Utc>,
    /// Current spread in bps
    pub spread_bps: u32,
}

impl DumpSignal {
    /// Check if the signal passes anti-fake-dump filters
    pub fn is_valid(&self, max_spread_bps: u32) -> bool {
        self.spread_bps <= max_spread_bps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_side_opposite() {
        assert_eq!(Side::Up.opposite(), Side::Down);
        assert_eq!(Side::Down.opposite(), Side::Up);
    }

    #[test]
    fn test_quote_spread_bps() {
        let quote = Quote {
            side: Side::Up,
            best_bid: Some(dec!(0.45)),
            best_ask: Some(dec!(0.50)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp: Utc::now(),
        };

        // (0.50 - 0.45) / 0.45 * 10000 = 1111 bps
        let spread = quote.spread_bps().unwrap();
        assert!(spread > 1000 && spread < 1200);
    }

    #[test]
    fn test_market_snapshot_ask_sum() {
        let now = Utc::now();
        let snapshot = MarketSnapshot {
            round: Round {
                id: Some(1),
                slug: "test".to_string(),
                up_token_id: "up".to_string(),
                down_token_id: "down".to_string(),
                start_time: now,
                end_time: now + chrono::Duration::minutes(15),
                outcome: None,
            },
            up_quote: Quote {
                side: Side::Up,
                best_bid: Some(dec!(0.44)),
                best_ask: Some(dec!(0.45)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                timestamp: now,
            },
            down_quote: Quote {
                side: Side::Down,
                best_bid: Some(dec!(0.54)),
                best_ask: Some(dec!(0.56)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                timestamp: now,
            },
            timestamp: now,
        };

        assert_eq!(snapshot.ask_sum(), Some(dec!(1.01)));
    }
}
