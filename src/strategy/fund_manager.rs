//! Fund management for position sizing and risk control
//!
//! Provides centralized fund management including:
//! - Balance checking before order submission
//! - Dynamic share calculation based on fixed amount or percentage
//! - Concurrent position tracking
//! - Minimum balance enforcement

use crate::adapters::PolymarketClient;
use crate::config::RiskConfig;
use crate::error::Result;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Fund manager for position sizing and balance management
pub struct FundManager {
    client: PolymarketClient,
    config: RiskConfig,
    /// Track active position event IDs
    active_positions: Arc<RwLock<HashSet<String>>>,
    /// Cached balance (refreshed periodically)
    cached_balance: Arc<RwLock<Option<Decimal>>>,
}

impl FundManager {
    /// Create a new fund manager
    pub fn new(client: PolymarketClient, config: RiskConfig) -> Self {
        Self {
            client,
            config,
            active_positions: Arc::new(RwLock::new(HashSet::new())),
            cached_balance: Arc::new(RwLock::new(None)),
        }
    }

    /// Check if we can open a new position
    /// Returns Ok(shares) if allowed, or an error explaining why not
    pub async fn can_open_position(
        &self,
        event_id: &str,
        price: Decimal,
    ) -> Result<PositionSizeResult> {
        // 1. Check max positions limit
        if self.config.max_positions > 0 {
            let positions = self.active_positions.read().await;
            if positions.len() >= self.config.max_positions as usize {
                return Ok(PositionSizeResult::Rejected(
                    format!("Max positions reached ({}/{})", positions.len(), self.config.max_positions)
                ));
            }

            // Check if already in this event
            if positions.contains(event_id) {
                return Ok(PositionSizeResult::Rejected(
                    format!("Already have position in event {}", &event_id[..8.min(event_id.len())])
                ));
            }
        }

        // 2. Get available balance
        let balance = self.get_balance().await?;

        // 3. Check minimum balance requirement
        if balance < self.config.min_balance_usd {
            return Ok(PositionSizeResult::Rejected(
                format!("Balance ${:.2} below minimum ${:.2}", balance, self.config.min_balance_usd)
            ));
        }

        // 4. Calculate position size
        let (amount_usd, shares) = self.calculate_position_size(balance, price)?;

        // 5. Check if we have enough after min balance
        let available = balance - self.config.min_balance_usd;
        if amount_usd > available {
            return Ok(PositionSizeResult::Rejected(
                format!("Order ${:.2} exceeds available ${:.2} (keeping ${:.2} reserve)",
                    amount_usd, available, self.config.min_balance_usd)
            ));
        }

        // 6. Ensure minimum order requirements
        // Polymarket: minimum 5 shares AND $1 order value
        if shares < 5 {
            return Ok(PositionSizeResult::Rejected(
                format!("Calculated shares ({}) below minimum 5", shares)
            ));
        }

        if amount_usd < dec!(1) {
            return Ok(PositionSizeResult::Rejected(
                format!("Order value ${:.2} below minimum $1", amount_usd)
            ));
        }

        info!(
            "✅ Position approved: {} shares @ {:.2}¢ = ${:.2} (balance: ${:.2})",
            shares,
            price * dec!(100),
            amount_usd,
            balance
        );

        Ok(PositionSizeResult::Approved { shares, amount_usd })
    }

    /// Calculate position size based on config
    fn calculate_position_size(&self, balance: Decimal, price: Decimal) -> Result<(Decimal, u64)> {
        let amount_usd = if let Some(fixed) = self.config.fixed_amount_usd {
            // Fixed USD amount per trade
            fixed
        } else if let Some(pct) = self.config.position_size_pct {
            // Percentage of available balance
            let available = balance - self.config.min_balance_usd;
            available * pct
        } else {
            // Default: use max_single_exposure_usd
            self.config.max_single_exposure_usd
        };

        // Calculate shares: amount / price
        // Round down to avoid over-spending
        let shares_dec = amount_usd / price;
        let shares = shares_dec.to_u64().unwrap_or(0);

        // Recalculate actual USD amount
        let actual_amount = Decimal::from(shares) * price;

        debug!(
            "Position size: ${:.2} / {:.2}¢ = {} shares (actual: ${:.2})",
            amount_usd,
            price * dec!(100),
            shares,
            actual_amount
        );

        Ok((actual_amount, shares))
    }

    /// Get current balance (with caching)
    pub async fn get_balance(&self) -> Result<Decimal> {
        // Check cache first (valid for 10 seconds)
        {
            let cached = self.cached_balance.read().await;
            if let Some(bal) = *cached {
                return Ok(bal);
            }
        }

        // Fetch fresh balance
        let balance = self.client.get_usdc_balance().await?;

        // Update cache
        {
            let mut cached = self.cached_balance.write().await;
            *cached = Some(balance);
        }

        // Clear cache after 10 seconds
        let cache = self.cached_balance.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            let mut cached = cache.write().await;
            *cached = None;
        });

        Ok(balance)
    }

    /// Refresh balance cache
    pub async fn refresh_balance(&self) -> Result<Decimal> {
        let balance = self.client.get_usdc_balance().await?;
        let mut cached = self.cached_balance.write().await;
        *cached = Some(balance);
        Ok(balance)
    }

    /// Record that a position was opened
    pub async fn record_position_opened(&self, event_id: &str) {
        let mut positions = self.active_positions.write().await;
        positions.insert(event_id.to_string());
        info!("📊 Position opened: {} (total: {})", &event_id[..8.min(event_id.len())], positions.len());

        // Invalidate balance cache
        let mut cached = self.cached_balance.write().await;
        *cached = None;
    }

    /// Record that a position was closed
    pub async fn record_position_closed(&self, event_id: &str) {
        let mut positions = self.active_positions.write().await;
        positions.remove(event_id);
        info!("📊 Position closed: {} (total: {})", &event_id[..8.min(event_id.len())], positions.len());

        // Invalidate balance cache
        let mut cached = self.cached_balance.write().await;
        *cached = None;
    }

    /// Get current position count
    pub async fn position_count(&self) -> usize {
        self.active_positions.read().await.len()
    }

    /// Check if we have a position in an event
    pub async fn has_position(&self, event_id: &str) -> bool {
        self.active_positions.read().await.contains(event_id)
    }

    /// Get fund status summary
    pub async fn get_status(&self) -> FundStatus {
        let balance = self.get_balance().await.unwrap_or(Decimal::ZERO);
        let position_count = self.position_count().await;
        let max_positions = self.config.max_positions;
        let available = (balance - self.config.min_balance_usd).max(Decimal::ZERO);

        FundStatus {
            balance,
            available,
            position_count,
            max_positions,
            min_balance: self.config.min_balance_usd,
            fixed_amount: self.config.fixed_amount_usd,
            position_pct: self.config.position_size_pct,
        }
    }
}

/// Result of position size calculation
#[derive(Debug, Clone)]
pub enum PositionSizeResult {
    /// Position approved with calculated shares
    Approved { shares: u64, amount_usd: Decimal },
    /// Position rejected with reason
    Rejected(String),
}

impl PositionSizeResult {
    pub fn is_approved(&self) -> bool {
        matches!(self, PositionSizeResult::Approved { .. })
    }

    pub fn shares(&self) -> Option<u64> {
        match self {
            PositionSizeResult::Approved { shares, .. } => Some(*shares),
            _ => None,
        }
    }
}

/// Fund status summary
#[derive(Debug, Clone)]
pub struct FundStatus {
    pub balance: Decimal,
    pub available: Decimal,
    pub position_count: usize,
    pub max_positions: u32,
    pub min_balance: Decimal,
    pub fixed_amount: Option<Decimal>,
    pub position_pct: Option<Decimal>,
}

impl std::fmt::Display for FundStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Balance: ${:.2} | Available: ${:.2} | Positions: {}/{} | Reserve: ${:.2}",
            self.balance,
            self.available,
            self.position_count,
            self.max_positions,
            self.min_balance
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_size_result() {
        let approved = PositionSizeResult::Approved {
            shares: 10,
            amount_usd: dec!(3.50),
        };
        assert!(approved.is_approved());
        assert_eq!(approved.shares(), Some(10));

        let rejected = PositionSizeResult::Rejected("test".to_string());
        assert!(!rejected.is_approved());
        assert_eq!(rejected.shares(), None);
    }
}
