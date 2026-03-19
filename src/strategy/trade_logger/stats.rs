use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Per-symbol statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SymbolStats {
    pub symbol: String,
    pub total_trades: u32,
    pub wins: u32,
    pub losses: u32,
    pub open: u32,
    pub total_cost: Decimal,
    pub total_payout: Decimal,
    pub total_pnl: Decimal,
    pub avg_entry_price: Decimal,
    pub avg_edge: Decimal,
    pub last_trade: Option<DateTime<Utc>>,
}

impl SymbolStats {
    pub fn win_rate(&self) -> Decimal {
        let closed = self.wins + self.losses;
        if closed == 0 {
            return Decimal::ZERO;
        }
        Decimal::from(self.wins) / Decimal::from(closed)
    }

    pub fn roi(&self) -> Decimal {
        if self.total_cost == Decimal::ZERO {
            return Decimal::ZERO;
        }
        self.total_pnl / self.total_cost
    }
}

/// Overall trading statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TradingStats {
    pub total_trades: u32,
    pub wins: u32,
    pub losses: u32,
    pub open: u32,
    pub total_cost: Decimal,
    pub total_payout: Decimal,
    pub total_pnl: Decimal,
    pub by_symbol: HashMap<String, SymbolStats>,
    /// Stats by time bucket (0-2, 2-5, 5-10, 10-15)
    #[serde(default)]
    pub by_time_bucket: HashMap<String, BucketStats>,
    /// Stats by strategy mode (early_mispricing, late_reversal)
    #[serde(default)]
    pub by_strategy_mode: HashMap<String, BucketStats>,
}

impl TradingStats {
    pub fn win_rate(&self) -> Decimal {
        let closed = self.wins + self.losses;
        if closed == 0 {
            return Decimal::ZERO;
        }
        Decimal::from(self.wins) / Decimal::from(closed)
    }

    pub fn roi(&self) -> Decimal {
        if self.total_cost == Decimal::ZERO {
            return Decimal::ZERO;
        }
        self.total_pnl / self.total_cost
    }
}

/// Statistics for a time bucket or strategy mode
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BucketStats {
    pub trades: u32,
    pub wins: u32,
    pub losses: u32,
    pub cost: Decimal,
    pub pnl: Decimal,
}

impl BucketStats {
    pub fn win_rate(&self) -> Decimal {
        let closed = self.wins + self.losses;
        if closed == 0 {
            return Decimal::ZERO;
        }
        Decimal::from(self.wins) / Decimal::from(closed)
    }

    pub fn roi(&self) -> Decimal {
        if self.cost == Decimal::ZERO {
            return Decimal::ZERO;
        }
        self.pnl / self.cost
    }

    pub fn ev_per_trade(&self) -> Decimal {
        if self.trades == 0 {
            return Decimal::ZERO;
        }
        self.pnl / Decimal::from(self.trades)
    }
}

#[cfg(test)]
mod tests {
    use super::SymbolStats;
    use rust_decimal_macros::dec;

    #[test]
    fn test_symbol_stats() {
        let stats = SymbolStats {
            symbol: "BTCUSDT".to_string(),
            total_trades: 10,
            wins: 7,
            losses: 3,
            open: 0,
            total_cost: dec!(35),
            total_payout: dec!(70),
            total_pnl: dec!(35),
            ..Default::default()
        };

        assert_eq!(stats.win_rate(), dec!(0.7));
        assert_eq!(stats.roi(), dec!(1));
    }
}
