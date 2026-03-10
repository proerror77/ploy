//! Strategy Adapters
//!
//! Adapters that wrap existing strategy implementations to implement the Strategy trait.
//! This enables using existing engines (MomentumEngine, SplitArbEngine) with the new
//! StrategyManager infrastructure.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{OnceCell, RwLock};
use tracing::{debug, info, warn};

use super::momentum::{Direction, ExitConfig, MomentumConfig};
use super::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyEvent, StrategyEventType, StrategyOrderIntent, StrategyStateInfo,
};
use crate::domain::{OrderType, Side, TimeInForce};
use crate::error::Result;
use crate::domain::Domain;
use crate::strategy::crypto::{all_updown_series_ids, symbol_and_window_for_series};
mod momentum_adapter;
mod split_arb_adapter;
pub use momentum_adapter::MomentumStrategyAdapter;
pub use split_arb_adapter::SplitArbStrategyAdapter;

fn crypto_submit_intent(
    client_order_id: String,
    market_slug: String,
    token_id: String,
    side: Side,
    is_buy: bool,
    shares: u64,
    limit_price: Decimal,
    priority: u8,
) -> StrategyAction {
    StrategyAction::SubmitIntent {
        intent: StrategyOrderIntent {
            client_order_id,
            domain: Domain::Crypto,
            market_slug,
            token_id,
            side,
            is_buy,
            shares,
            limit_price,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            priority,
            metadata: HashMap::new(),
        },
    }
}
