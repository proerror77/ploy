use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::Side;

/// Order side (buy or sell)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderSide {
    Buy,
    Sell,
}

impl std::fmt::Display for OrderSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderSide::Buy => write!(f, "BUY"),
            OrderSide::Sell => write!(f, "SELL"),
        }
    }
}

/// Order type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderType {
    Limit,
    Market,
}

/// Time in force
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    /// Good Till Cancelled
    GTC,
    /// Fill Or Kill
    FOK,
    /// Immediate Or Cancel
    IOC,
}

/// Order status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderStatus {
    /// Order created but not yet submitted
    Pending,
    /// Order submitted to exchange
    Submitted,
    /// Order partially filled
    PartiallyFilled,
    /// Order fully filled
    Filled,
    /// Order cancelled
    Cancelled,
    /// Order rejected by exchange
    Rejected,
    /// Order expired
    Expired,
    /// Order failed (internal error)
    Failed,
}

impl OrderStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Expired
                | OrderStatus::Failed
        )
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            OrderStatus::Pending | OrderStatus::Submitted | OrderStatus::PartiallyFilled
        )
    }
}

/// Order request (what we want to do)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    pub client_order_id: String,
    pub token_id: String,
    pub market_side: Side,
    pub order_side: OrderSide,
    pub shares: u64,
    pub limit_price: Decimal,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
}

impl OrderRequest {
    pub fn buy_limit(token_id: String, market_side: Side, shares: u64, price: Decimal) -> Self {
        Self {
            client_order_id: Uuid::new_v4().to_string(),
            token_id,
            market_side,
            order_side: OrderSide::Buy,
            shares,
            limit_price: price,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
        }
    }

    pub fn sell_limit(token_id: String, market_side: Side, shares: u64, price: Decimal) -> Self {
        Self {
            client_order_id: Uuid::new_v4().to_string(),
            token_id,
            market_side,
            order_side: OrderSide::Sell,
            shares,
            limit_price: price,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
        }
    }
}

/// Order (tracked in our system)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: Option<i32>,
    pub cycle_id: Option<i32>,
    pub leg: u8,
    pub client_order_id: String,
    pub exchange_order_id: Option<String>,
    pub token_id: String,
    pub market_side: Side,
    pub order_side: OrderSide,
    pub shares: u64,
    pub limit_price: Decimal,
    pub avg_fill_price: Option<Decimal>,
    pub filled_shares: u64,
    pub status: OrderStatus,
    pub submitted_at: Option<DateTime<Utc>>,
    pub filled_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Order {
    pub fn from_request(request: &OrderRequest, cycle_id: Option<i32>, leg: u8) -> Self {
        let now = Utc::now();
        Self {
            id: None,
            cycle_id,
            leg,
            client_order_id: request.client_order_id.clone(),
            exchange_order_id: None,
            token_id: request.token_id.clone(),
            market_side: request.market_side,
            order_side: request.order_side,
            shares: request.shares,
            limit_price: request.limit_price,
            avg_fill_price: None,
            filled_shares: 0,
            status: OrderStatus::Pending,
            submitted_at: None,
            filled_at: None,
            error: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Calculate the dollar value of the order
    pub fn value(&self) -> Decimal {
        self.limit_price * Decimal::from(self.shares)
    }

    /// Calculate fill percentage
    pub fn fill_pct(&self) -> Decimal {
        if self.shares == 0 {
            return Decimal::ZERO;
        }
        Decimal::from(self.filled_shares) / Decimal::from(self.shares) * Decimal::from(100)
    }

    /// Check if fully filled
    pub fn is_fully_filled(&self) -> bool {
        self.status == OrderStatus::Filled && self.filled_shares >= self.shares
    }

    /// Calculate actual fill value
    pub fn fill_value(&self) -> Decimal {
        match self.avg_fill_price {
            Some(price) => price * Decimal::from(self.filled_shares),
            None => Decimal::ZERO,
        }
    }
}

/// Fill event from the exchange
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub order_id: String,
    pub trade_id: String,
    pub price: Decimal,
    pub shares: u64,
    pub timestamp: DateTime<Utc>,
    pub fee: Decimal,
}

/// Position in a specific token
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub token_id: String,
    pub side: Side,
    pub shares: u64,
    pub avg_entry_price: Decimal,
    pub current_value: Decimal,
    pub unrealized_pnl: Decimal,
}

/// A complete trading cycle (both legs)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cycle {
    pub id: Option<i32>,
    pub round_id: i32,
    pub state: String,
    pub leg1_side: Option<Side>,
    pub leg1_entry_price: Option<Decimal>,
    pub leg1_shares: Option<u64>,
    pub leg1_filled_at: Option<DateTime<Utc>>,
    pub leg2_entry_price: Option<Decimal>,
    pub leg2_shares: Option<u64>,
    pub leg2_filled_at: Option<DateTime<Utc>>,
    pub pnl: Option<Decimal>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Cycle {
    pub fn new(round_id: i32, state: &str) -> Self {
        let now = Utc::now();
        Self {
            id: None,
            round_id,
            state: state.to_string(),
            leg1_side: None,
            leg1_entry_price: None,
            leg1_shares: None,
            leg1_filled_at: None,
            leg2_entry_price: None,
            leg2_shares: None,
            leg2_filled_at: None,
            pnl: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Calculate expected PnL if both legs complete
    /// PnL = shares * (1 - leg1_price - leg2_price) - fees
    pub fn expected_pnl(&self, leg2_price: Decimal, fee_rate: Decimal) -> Option<Decimal> {
        match (self.leg1_entry_price, self.leg1_shares) {
            (Some(leg1_price), Some(shares)) => {
                let gross = Decimal::from(shares) * (Decimal::ONE - leg1_price - leg2_price);
                let fees = Decimal::from(shares) * (leg1_price + leg2_price) * fee_rate;
                Some(gross - fees)
            }
            _ => None,
        }
    }

    /// Check if Leg2 condition is met
    pub fn should_trigger_leg2(&self, opposite_ask: Decimal, sum_target: Decimal) -> bool {
        match self.leg1_entry_price {
            Some(leg1_price) => leg1_price + opposite_ask <= sum_target,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_order_fill_pct() {
        let mut order = Order {
            id: None,
            cycle_id: None,
            leg: 1,
            client_order_id: "test".to_string(),
            exchange_order_id: None,
            token_id: "token".to_string(),
            market_side: Side::Up,
            order_side: OrderSide::Buy,
            shares: 100,
            limit_price: dec!(0.45),
            avg_fill_price: Some(dec!(0.44)),
            filled_shares: 50,
            status: OrderStatus::PartiallyFilled,
            submitted_at: Some(Utc::now()),
            filled_at: None,
            error: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert_eq!(order.fill_pct(), dec!(50));

        order.filled_shares = 100;
        order.status = OrderStatus::Filled;
        assert_eq!(order.fill_pct(), dec!(100));
    }

    #[test]
    fn test_cycle_expected_pnl() {
        let mut cycle = Cycle::new(1, "LEG1_FILLED");
        cycle.leg1_entry_price = Some(dec!(0.45));
        cycle.leg1_shares = Some(100);

        // leg2_price = 0.50, sum = 0.95
        // gross = 100 * (1 - 0.45 - 0.50) = 100 * 0.05 = 5
        // fees = 100 * 0.95 * 0.005 = 0.475
        // net = 5 - 0.475 = 4.525
        let pnl = cycle.expected_pnl(dec!(0.50), dec!(0.005)).unwrap();
        assert!(pnl > dec!(4) && pnl < dec!(5));
    }

    #[test]
    fn test_cycle_leg2_trigger() {
        let mut cycle = Cycle::new(1, "LEG1_FILLED");
        cycle.leg1_entry_price = Some(dec!(0.45));

        // sum_target = 0.96, opposite_ask = 0.50
        // 0.45 + 0.50 = 0.95 <= 0.96 -> true
        assert!(cycle.should_trigger_leg2(dec!(0.50), dec!(0.96)));

        // opposite_ask = 0.55
        // 0.45 + 0.55 = 1.00 > 0.96 -> false
        assert!(!cycle.should_trigger_leg2(dec!(0.55), dec!(0.96)));
    }
}
