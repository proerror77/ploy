//! Position management for all strategies
//!
//! Centralized position tracking across all strategies.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::domain::Side;

/// A single position
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    /// Unique position ID
    pub id: String,
    /// Token ID
    pub token_id: String,
    /// Market side (Up/Down)
    pub side: Side,
    /// Number of shares
    pub shares: u64,
    /// Average entry price
    pub entry_price: Decimal,
    /// Current market price
    pub current_price: Option<Decimal>,
    /// Unrealized P&L
    pub unrealized_pnl: Decimal,
    /// Realized P&L (from partial closes)
    pub realized_pnl: Decimal,
    /// When position was opened
    pub opened_at: DateTime<Utc>,
    /// When position was last updated
    pub updated_at: DateTime<Utc>,
    /// Strategy that owns this position
    pub strategy_id: String,
    /// Associated event/round ID
    pub event_id: Option<String>,
    /// Custom metadata
    pub metadata: HashMap<String, String>,
}

impl Position {
    /// Create a new position
    pub fn new(
        token_id: String,
        side: Side,
        shares: u64,
        entry_price: Decimal,
        strategy_id: String,
    ) -> Self {
        let now = Utc::now();
        let id = format!("{}-{}-{}", strategy_id, token_id, now.timestamp_millis());

        Self {
            id,
            token_id,
            side,
            shares,
            entry_price,
            current_price: Some(entry_price),
            unrealized_pnl: Decimal::ZERO,
            realized_pnl: Decimal::ZERO,
            opened_at: now,
            updated_at: now,
            strategy_id,
            event_id: None,
            metadata: HashMap::new(),
        }
    }

    /// Update current price and recalculate P&L
    pub fn update_price(&mut self, price: Decimal) {
        self.current_price = Some(price);
        self.unrealized_pnl = (price - self.entry_price) * Decimal::from(self.shares);
        self.updated_at = Utc::now();
    }

    /// Calculate P&L percentage
    pub fn pnl_pct(&self) -> Decimal {
        if self.entry_price.is_zero() {
            return Decimal::ZERO;
        }
        self.current_price
            .map(|p| (p - self.entry_price) / self.entry_price)
            .unwrap_or(Decimal::ZERO)
    }

    /// Get total value at current price
    pub fn current_value(&self) -> Decimal {
        self.current_price.unwrap_or(self.entry_price) * Decimal::from(self.shares)
    }

    /// Get total value at entry price
    pub fn entry_value(&self) -> Decimal {
        self.entry_price * Decimal::from(self.shares)
    }

    /// Reduce position size (partial close)
    pub fn reduce(&mut self, shares_to_close: u64, close_price: Decimal) -> Decimal {
        let shares_to_close = shares_to_close.min(self.shares);
        if shares_to_close == 0 {
            return Decimal::ZERO;
        }

        let pnl = (close_price - self.entry_price) * Decimal::from(shares_to_close);
        self.realized_pnl += pnl;
        self.shares -= shares_to_close;
        self.updated_at = Utc::now();

        // Recalculate unrealized P&L for remaining shares
        if self.shares > 0 {
            self.unrealized_pnl = self
                .current_price
                .map(|p| (p - self.entry_price) * Decimal::from(self.shares))
                .unwrap_or(Decimal::ZERO);
        } else {
            self.unrealized_pnl = Decimal::ZERO;
        }

        pnl
    }

    /// Add to position (increase size)
    pub fn add(&mut self, shares_to_add: u64, add_price: Decimal) {
        // Calculate new average entry price
        let total_value = self.entry_value() + add_price * Decimal::from(shares_to_add);
        let new_shares = self.shares + shares_to_add;

        self.entry_price = total_value / Decimal::from(new_shares);
        self.shares = new_shares;
        self.updated_at = Utc::now();

        // Recalculate unrealized P&L
        if let Some(current) = self.current_price {
            self.unrealized_pnl = (current - self.entry_price) * Decimal::from(self.shares);
        }
    }

    /// Check if position is closed
    pub fn is_closed(&self) -> bool {
        self.shares == 0
    }

    /// Set metadata
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Set event ID
    pub fn with_event(mut self, event_id: impl Into<String>) -> Self {
        self.event_id = Some(event_id.into());
        self
    }
}

/// Position update event
#[derive(Debug, Clone)]
pub enum PositionUpdate {
    /// New position opened
    Opened(Position),
    /// Position price updated
    PriceUpdated {
        position_id: String,
        new_price: Decimal,
        unrealized_pnl: Decimal,
    },
    /// Position partially closed
    PartialClose {
        position_id: String,
        shares_closed: u64,
        close_price: Decimal,
        realized_pnl: Decimal,
    },
    /// Position fully closed
    Closed {
        position_id: String,
        close_price: Decimal,
        total_pnl: Decimal,
    },
    /// Position increased
    Increased {
        position_id: String,
        shares_added: u64,
        add_price: Decimal,
    },
}

/// Centralized position manager
pub struct PositionManager {
    /// All active positions by ID
    positions: Arc<RwLock<HashMap<String, Position>>>,
    /// Index by token ID
    by_token: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Index by strategy
    by_strategy: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Closed positions (for P&L tracking)
    closed: Arc<RwLock<Vec<Position>>>,
}

impl Default for PositionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PositionManager {
    /// Create a new position manager
    pub fn new() -> Self {
        Self {
            positions: Arc::new(RwLock::new(HashMap::new())),
            by_token: Arc::new(RwLock::new(HashMap::new())),
            by_strategy: Arc::new(RwLock::new(HashMap::new())),
            closed: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Open a new position
    pub async fn open(&self, position: Position) -> PositionUpdate {
        let id = position.id.clone();
        let token_id = position.token_id.clone();
        let strategy_id = position.strategy_id.clone();

        info!(
            "Opening position {}: {} {} shares of {} @ {}",
            id, position.side, position.shares, token_id, position.entry_price
        );

        // Add to main map
        self.positions.write().await.insert(id.clone(), position.clone());

        // Update token index
        self.by_token
            .write()
            .await
            .entry(token_id)
            .or_default()
            .push(id.clone());

        // Update strategy index
        self.by_strategy
            .write()
            .await
            .entry(strategy_id)
            .or_default()
            .push(id);

        PositionUpdate::Opened(position)
    }

    /// Update price for a position
    pub async fn update_price(&self, position_id: &str, price: Decimal) -> Option<PositionUpdate> {
        let mut positions = self.positions.write().await;
        if let Some(pos) = positions.get_mut(position_id) {
            pos.update_price(price);
            Some(PositionUpdate::PriceUpdated {
                position_id: position_id.to_string(),
                new_price: price,
                unrealized_pnl: pos.unrealized_pnl,
            })
        } else {
            None
        }
    }

    /// Update prices for all positions of a token
    pub async fn update_token_price(&self, token_id: &str, price: Decimal) -> Vec<PositionUpdate> {
        let position_ids: Vec<String> = {
            let by_token = self.by_token.read().await;
            by_token.get(token_id).cloned().unwrap_or_default()
        };

        let mut updates = Vec::new();
        for id in position_ids {
            if let Some(update) = self.update_price(&id, price).await {
                updates.push(update);
            }
        }
        updates
    }

    /// Close a position
    pub async fn close(&self, position_id: &str, close_price: Decimal) -> Option<PositionUpdate> {
        let mut positions = self.positions.write().await;

        if let Some(mut pos) = positions.remove(position_id) {
            // Calculate final P&L
            let final_pnl = pos.reduce(pos.shares, close_price);
            let total_pnl = pos.realized_pnl;

            info!(
                "Closing position {}: {} shares @ {} (P&L: {})",
                position_id, pos.shares, close_price, total_pnl
            );

            // Remove from indices
            {
                let mut by_token = self.by_token.write().await;
                if let Some(ids) = by_token.get_mut(&pos.token_id) {
                    ids.retain(|id| id != position_id);
                }
            }
            {
                let mut by_strategy = self.by_strategy.write().await;
                if let Some(ids) = by_strategy.get_mut(&pos.strategy_id) {
                    ids.retain(|id| id != position_id);
                }
            }

            // Archive closed position
            self.closed.write().await.push(pos);

            Some(PositionUpdate::Closed {
                position_id: position_id.to_string(),
                close_price,
                total_pnl,
            })
        } else {
            warn!("Attempted to close non-existent position: {}", position_id);
            None
        }
    }

    /// Partial close
    pub async fn partial_close(
        &self,
        position_id: &str,
        shares_to_close: u64,
        close_price: Decimal,
    ) -> Option<PositionUpdate> {
        let mut positions = self.positions.write().await;

        if let Some(pos) = positions.get_mut(position_id) {
            let realized_pnl = pos.reduce(shares_to_close, close_price);

            debug!(
                "Partial close {}: {} shares @ {} (P&L: {})",
                position_id, shares_to_close, close_price, realized_pnl
            );

            // If fully closed, remove from maps
            if pos.is_closed() {
                let pos = positions.remove(position_id).unwrap();

                // Remove from indices
                {
                    let mut by_token = self.by_token.write().await;
                    if let Some(ids) = by_token.get_mut(&pos.token_id) {
                        ids.retain(|id| id != position_id);
                    }
                }
                {
                    let mut by_strategy = self.by_strategy.write().await;
                    if let Some(ids) = by_strategy.get_mut(&pos.strategy_id) {
                        ids.retain(|id| id != position_id);
                    }
                }

                self.closed.write().await.push(pos);

                return Some(PositionUpdate::Closed {
                    position_id: position_id.to_string(),
                    close_price,
                    total_pnl: realized_pnl,
                });
            }

            Some(PositionUpdate::PartialClose {
                position_id: position_id.to_string(),
                shares_closed: shares_to_close,
                close_price,
                realized_pnl,
            })
        } else {
            None
        }
    }

    /// Get a position by ID
    pub async fn get(&self, position_id: &str) -> Option<Position> {
        self.positions.read().await.get(position_id).cloned()
    }

    /// Get all positions for a token
    pub async fn get_by_token(&self, token_id: &str) -> Vec<Position> {
        let position_ids: Vec<String> = {
            let by_token = self.by_token.read().await;
            by_token.get(token_id).cloned().unwrap_or_default()
        };

        let positions = self.positions.read().await;
        position_ids
            .iter()
            .filter_map(|id| positions.get(id).cloned())
            .collect()
    }

    /// Get all positions for a strategy
    pub async fn get_by_strategy(&self, strategy_id: &str) -> Vec<Position> {
        let position_ids: Vec<String> = {
            let by_strategy = self.by_strategy.read().await;
            by_strategy.get(strategy_id).cloned().unwrap_or_default()
        };

        let positions = self.positions.read().await;
        position_ids
            .iter()
            .filter_map(|id| positions.get(id).cloned())
            .collect()
    }

    /// Get all active positions
    pub async fn get_all(&self) -> Vec<Position> {
        self.positions.read().await.values().cloned().collect()
    }

    /// Get total exposure across all positions
    pub async fn total_exposure(&self) -> Decimal {
        self.positions
            .read()
            .await
            .values()
            .map(|p| p.current_value())
            .sum()
    }

    /// Get total unrealized P&L
    pub async fn total_unrealized_pnl(&self) -> Decimal {
        self.positions
            .read()
            .await
            .values()
            .map(|p| p.unrealized_pnl)
            .sum()
    }

    /// Get exposure by strategy
    pub async fn exposure_by_strategy(&self, strategy_id: &str) -> Decimal {
        self.get_by_strategy(strategy_id)
            .await
            .iter()
            .map(|p| p.current_value())
            .sum()
    }

    /// Count positions
    pub async fn count(&self) -> usize {
        self.positions.read().await.len()
    }

    /// Count positions for a strategy
    pub async fn count_by_strategy(&self, strategy_id: &str) -> usize {
        self.by_strategy
            .read()
            .await
            .get(strategy_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// Clear all positions (for testing/reset)
    pub async fn clear(&self) {
        self.positions.write().await.clear();
        self.by_token.write().await.clear();
        self.by_strategy.write().await.clear();
    }

    /// Get closed positions for P&L reporting
    pub async fn get_closed(&self) -> Vec<Position> {
        self.closed.read().await.clone()
    }

    /// Get today's realized P&L from closed positions
    pub async fn today_realized_pnl(&self) -> Decimal {
        let today = Utc::now().date_naive();
        self.closed
            .read()
            .await
            .iter()
            .filter(|p| p.updated_at.date_naive() == today)
            .map(|p| p.realized_pnl)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn test_position_lifecycle() {
        let manager = PositionManager::new();

        // Open position
        let pos = Position::new(
            "token1".to_string(),
            Side::Up,
            100,
            dec!(0.50),
            "test-strategy".to_string(),
        );
        let pos_id = pos.id.clone();
        manager.open(pos).await;

        // Check it exists
        assert_eq!(manager.count().await, 1);

        // Update price
        manager.update_price(&pos_id, dec!(0.60)).await;
        let updated = manager.get(&pos_id).await.unwrap();
        assert_eq!(updated.current_price, Some(dec!(0.60)));
        assert_eq!(updated.unrealized_pnl, dec!(10)); // (0.60 - 0.50) * 100

        // Close position
        manager.close(&pos_id, dec!(0.55)).await;
        assert_eq!(manager.count().await, 0);

        // Check closed history
        let closed = manager.get_closed().await;
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].realized_pnl, dec!(5)); // (0.55 - 0.50) * 100
    }

    #[tokio::test]
    async fn test_partial_close() {
        let manager = PositionManager::new();

        let pos = Position::new(
            "token1".to_string(),
            Side::Up,
            100,
            dec!(0.50),
            "test-strategy".to_string(),
        );
        let pos_id = pos.id.clone();
        manager.open(pos).await;

        // Partial close 50 shares
        manager.partial_close(&pos_id, 50, dec!(0.60)).await;

        let remaining = manager.get(&pos_id).await.unwrap();
        assert_eq!(remaining.shares, 50);
        assert_eq!(remaining.realized_pnl, dec!(5)); // (0.60 - 0.50) * 50
    }
}
