//! Store trait for StrategyEngine dependency injection.
//!
//! Decouples the engine from PostgresStore, enabling unit tests with MockStore.

use async_trait::async_trait;
use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::domain::{Order, OrderStatus, Round, Side, StrategyState};
use crate::error::Result;

/// Minimal persistence interface used by [`super::StrategyEngine`].
///
/// Only the methods the engine actually calls are included here (YAGNI).
/// `PostgresStore` implements this via a blanket impl below.
#[async_trait]
pub trait EngineStore: Send + Sync {
    // --- Rounds ---
    async fn upsert_round(&self, round: &Round) -> Result<i32>;

    // --- Cycles ---
    async fn create_cycle(&self, round_id: i32, state: StrategyState) -> Result<i32>;
    async fn update_cycle_state(&self, cycle_id: i32, state: StrategyState) -> Result<()>;
    async fn update_cycle_leg1(
        &self,
        cycle_id: i32,
        side: Side,
        entry_price: Decimal,
        shares: u64,
    ) -> Result<()>;
    async fn update_cycle_leg2(
        &self,
        cycle_id: i32,
        entry_price: Decimal,
        shares: u64,
        pnl: Decimal,
    ) -> Result<()>;
    async fn abort_cycle(&self, cycle_id: i32, reason: &str) -> Result<()>;

    // --- Orders ---
    async fn insert_order(&self, order: &Order) -> Result<i32>;
    async fn update_order_status(
        &self,
        client_order_id: &str,
        status: OrderStatus,
        exchange_order_id: Option<&str>,
    ) -> Result<()>;
    async fn update_order_fill(
        &self,
        client_order_id: &str,
        filled_shares: u64,
        avg_fill_price: Decimal,
        status: OrderStatus,
    ) -> Result<()>;

    // --- Strategy state ---
    async fn update_strategy_state(
        &self,
        state: StrategyState,
        round_id: Option<i32>,
        cycle_id: Option<i32>,
    ) -> Result<()>;

    // --- Daily stats ---
    async fn increment_cycle_count(&self, date: NaiveDate) -> Result<()>;
    async fn record_cycle_completion(&self, date: NaiveDate, pnl: Decimal) -> Result<()>;
    async fn record_cycle_abort(&self, date: NaiveDate) -> Result<()>;
    async fn record_cycle_abort_neutral(&self, date: NaiveDate) -> Result<()>;
    async fn halt_trading(&self, date: NaiveDate, reason: &str) -> Result<()>;
}

#[async_trait]
impl EngineStore for crate::adapters::PostgresStore {
    async fn upsert_round(&self, round: &Round) -> Result<i32> {
        self.upsert_round(round).await
    }
    async fn create_cycle(&self, round_id: i32, state: StrategyState) -> Result<i32> {
        self.create_cycle(round_id, state).await
    }
    async fn update_cycle_state(&self, cycle_id: i32, state: StrategyState) -> Result<()> {
        self.update_cycle_state(cycle_id, state).await
    }
    async fn update_cycle_leg1(
        &self,
        cycle_id: i32,
        side: Side,
        entry_price: Decimal,
        shares: u64,
    ) -> Result<()> {
        self.update_cycle_leg1(cycle_id, side, entry_price, shares)
            .await
    }
    async fn update_cycle_leg2(
        &self,
        cycle_id: i32,
        entry_price: Decimal,
        shares: u64,
        pnl: Decimal,
    ) -> Result<()> {
        self.update_cycle_leg2(cycle_id, entry_price, shares, pnl)
            .await
    }
    async fn abort_cycle(&self, cycle_id: i32, reason: &str) -> Result<()> {
        self.abort_cycle(cycle_id, reason).await
    }
    async fn insert_order(&self, order: &Order) -> Result<i32> {
        self.insert_order(order).await
    }
    async fn update_order_status(
        &self,
        client_order_id: &str,
        status: OrderStatus,
        exchange_order_id: Option<&str>,
    ) -> Result<()> {
        self.update_order_status(client_order_id, status, exchange_order_id)
            .await
    }
    async fn update_order_fill(
        &self,
        client_order_id: &str,
        filled_shares: u64,
        avg_fill_price: Decimal,
        status: OrderStatus,
    ) -> Result<()> {
        self.update_order_fill(client_order_id, filled_shares, avg_fill_price, status)
            .await
    }
    async fn update_strategy_state(
        &self,
        state: StrategyState,
        round_id: Option<i32>,
        cycle_id: Option<i32>,
    ) -> Result<()> {
        self.update_strategy_state(state, round_id, cycle_id).await
    }
    async fn increment_cycle_count(&self, date: NaiveDate) -> Result<()> {
        self.increment_cycle_count(date).await
    }
    async fn record_cycle_completion(&self, date: NaiveDate, pnl: Decimal) -> Result<()> {
        self.record_cycle_completion(date, pnl).await
    }
    async fn record_cycle_abort(&self, date: NaiveDate) -> Result<()> {
        self.record_cycle_abort(date).await
    }
    async fn record_cycle_abort_neutral(&self, date: NaiveDate) -> Result<()> {
        self.record_cycle_abort_neutral(date).await
    }
    async fn halt_trading(&self, date: NaiveDate, reason: &str) -> Result<()> {
        self.halt_trading(date, reason).await
    }
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::sync::atomic::{AtomicI32, Ordering};

    /// In-memory mock store for engine unit tests.
    ///
    /// All write operations succeed silently and return sequential IDs.
    pub struct MockStore {
        next_id: AtomicI32,
    }

    impl MockStore {
        pub fn new() -> Self {
            Self {
                next_id: AtomicI32::new(1),
            }
        }

        fn next_id(&self) -> i32 {
            self.next_id.fetch_add(1, Ordering::SeqCst)
        }
    }

    impl Default for MockStore {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl EngineStore for MockStore {
        async fn upsert_round(&self, _round: &Round) -> Result<i32> {
            Ok(self.next_id())
        }
        async fn create_cycle(&self, _round_id: i32, _state: StrategyState) -> Result<i32> {
            Ok(self.next_id())
        }
        async fn update_cycle_state(&self, _cycle_id: i32, _state: StrategyState) -> Result<()> {
            Ok(())
        }
        async fn update_cycle_leg1(
            &self,
            _cycle_id: i32,
            _side: Side,
            _entry_price: Decimal,
            _shares: u64,
        ) -> Result<()> {
            Ok(())
        }
        async fn update_cycle_leg2(
            &self,
            _cycle_id: i32,
            _entry_price: Decimal,
            _shares: u64,
            _pnl: Decimal,
        ) -> Result<()> {
            Ok(())
        }
        async fn abort_cycle(&self, _cycle_id: i32, _reason: &str) -> Result<()> {
            Ok(())
        }
        async fn insert_order(&self, _order: &Order) -> Result<i32> {
            Ok(self.next_id())
        }
        async fn update_order_status(
            &self,
            _client_order_id: &str,
            _status: OrderStatus,
            _exchange_order_id: Option<&str>,
        ) -> Result<()> {
            Ok(())
        }
        async fn update_order_fill(
            &self,
            _client_order_id: &str,
            _filled_shares: u64,
            _avg_fill_price: Decimal,
            _status: OrderStatus,
        ) -> Result<()> {
            Ok(())
        }
        async fn update_strategy_state(
            &self,
            _state: StrategyState,
            _round_id: Option<i32>,
            _cycle_id: Option<i32>,
        ) -> Result<()> {
            Ok(())
        }
        async fn increment_cycle_count(&self, _date: NaiveDate) -> Result<()> {
            Ok(())
        }
        async fn record_cycle_completion(&self, _date: NaiveDate, _pnl: Decimal) -> Result<()> {
            Ok(())
        }
        async fn record_cycle_abort(&self, _date: NaiveDate) -> Result<()> {
            Ok(())
        }
        async fn record_cycle_abort_neutral(&self, _date: NaiveDate) -> Result<()> {
            Ok(())
        }
        async fn halt_trading(&self, _date: NaiveDate, _reason: &str) -> Result<()> {
            Ok(())
        }
    }
}
