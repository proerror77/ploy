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
    /// Track positions per symbol (symbol -> count)
    positions_per_symbol: Arc<RwLock<std::collections::HashMap<String, u32>>>,
    /// Track deployed funds per symbol (symbol -> USD amount)
    symbol_exposure: Arc<RwLock<std::collections::HashMap<String, Decimal>>>,
    /// Total number of symbols for equal allocation
    total_symbols: u32,
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
            positions_per_symbol: Arc::new(RwLock::new(std::collections::HashMap::new())),
            symbol_exposure: Arc::new(RwLock::new(std::collections::HashMap::new())),
            total_symbols: 4, // Default: BTC, ETH, SOL, XRP
            cached_balance: Arc::new(RwLock::new(None)),
        }
    }

    /// Create a new fund manager with specified symbol count
    pub fn new_with_symbols(client: PolymarketClient, config: RiskConfig, total_symbols: u32) -> Self {
        Self {
            client,
            config,
            active_positions: Arc::new(RwLock::new(HashSet::new())),
            positions_per_symbol: Arc::new(RwLock::new(std::collections::HashMap::new())),
            symbol_exposure: Arc::new(RwLock::new(std::collections::HashMap::new())),
            total_symbols: total_symbols.max(1), // At least 1
            cached_balance: Arc::new(RwLock::new(None)),
        }
    }

    /// Set total symbols for dynamic allocation
    pub fn set_total_symbols(&mut self, count: u32) {
        self.total_symbols = count.max(1);
    }

    /// Get per-symbol allocation amount
    pub async fn get_per_symbol_allocation(&self) -> Result<Decimal> {
        let balance = self.get_balance().await?;
        let available = balance - self.config.min_balance_usd;
        if available <= Decimal::ZERO {
            return Ok(Decimal::ZERO);
        }
        Ok(available / Decimal::from(self.total_symbols))
    }

    /// Get current exposure for a symbol
    pub async fn get_symbol_exposure(&self, symbol: &str) -> Decimal {
        let exposure = self.symbol_exposure.read().await;
        exposure.get(symbol).copied().unwrap_or(Decimal::ZERO)
    }

    /// Get remaining allocation for a symbol
    pub async fn get_remaining_allocation(&self, symbol: &str) -> Result<Decimal> {
        let per_symbol = self.get_per_symbol_allocation().await?;
        let current = self.get_symbol_exposure(symbol).await;
        Ok((per_symbol - current).max(Decimal::ZERO))
    }

    /// Check if we can open a new position
    /// Returns Ok(shares) if allowed, or an error explaining why not
    pub async fn can_open_position(
        &self,
        event_id: &str,
        symbol: &str,
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

        // 2. Check per-symbol position limit
        if self.config.max_positions_per_symbol > 0 {
            let symbol_positions = self.positions_per_symbol.read().await;
            let current_count = symbol_positions.get(symbol).copied().unwrap_or(0);
            if current_count >= self.config.max_positions_per_symbol {
                return Ok(PositionSizeResult::Rejected(
                    format!("{} already has {} positions (max: {})",
                        symbol, current_count, self.config.max_positions_per_symbol)
                ));
            }
        }

        // 3. Get available balance
        let balance = self.get_balance().await?;

        // 4. Check minimum balance requirement
        if balance < self.config.min_balance_usd {
            return Ok(PositionSizeResult::Rejected(
                format!("Balance ${:.2} below minimum ${:.2}", balance, self.config.min_balance_usd)
            ));
        }

        // 5. Check per-symbol allocation (dynamic fund distribution)
        let per_symbol_allocation = self.get_per_symbol_allocation().await?;
        let current_exposure = self.get_symbol_exposure(symbol).await;
        let remaining_allocation = (per_symbol_allocation - current_exposure).max(Decimal::ZERO);

        if remaining_allocation < dec!(1) {
            return Ok(PositionSizeResult::Rejected(
                format!("{} allocation exhausted: ${:.2} used of ${:.2} ({}% of funds)",
                    symbol, current_exposure, per_symbol_allocation,
                    (Decimal::from(100) / Decimal::from(self.total_symbols)).round())
            ));
        }

        // 6. Calculate position size (respecting per-symbol allocation)
        let (amount_usd, shares) = self.calculate_position_size_with_limit(
            balance, price, remaining_allocation
        )?;

        // 7. Check if we have enough after min balance
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
        self.calculate_position_size_with_limit(balance, price, Decimal::MAX)
    }

    /// Calculate position size with per-symbol allocation limit
    /// Uses dynamic allocation: available_funds / total_symbols
    fn calculate_position_size_with_limit(
        &self,
        balance: Decimal,
        price: Decimal,
        max_allocation: Decimal,
    ) -> Result<(Decimal, u64)> {
        // Start with base amount from config
        let base_amount = if let Some(fixed) = self.config.fixed_amount_usd {
            // Fixed USD amount per trade
            fixed
        } else if let Some(pct) = self.config.position_size_pct {
            // Percentage of available balance
            let available = balance - self.config.min_balance_usd;
            available * pct
        } else {
            // Default: use dynamic per-symbol allocation
            // This is the key change: use remaining allocation for this symbol
            max_allocation.min(self.config.max_single_exposure_usd)
        };

        // Apply per-symbol limit
        let amount_usd = base_amount.min(max_allocation);

        // Calculate shares: amount / price
        // Round down to avoid over-spending
        let shares_dec = amount_usd / price;
        let shares = shares_dec.to_u64().unwrap_or(0);

        // Recalculate actual USD amount
        let actual_amount = Decimal::from(shares) * price;

        debug!(
            "Position size: ${:.2} (limit: ${:.2}) / {:.2}¢ = {} shares (actual: ${:.2})",
            amount_usd,
            max_allocation,
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
    pub async fn record_position_opened(&self, event_id: &str, symbol: &str) {
        self.record_position_opened_with_amount(event_id, symbol, Decimal::ZERO).await;
    }

    /// Record that a position was opened with exposure amount
    pub async fn record_position_opened_with_amount(&self, event_id: &str, symbol: &str, amount_usd: Decimal) {
        // Track by event
        let mut positions = self.active_positions.write().await;
        positions.insert(event_id.to_string());
        let total = positions.len();
        drop(positions);

        // Track by symbol count
        let mut symbol_positions = self.positions_per_symbol.write().await;
        let count = symbol_positions.entry(symbol.to_string()).or_insert(0);
        *count += 1;
        let symbol_count = *count;
        drop(symbol_positions);

        // Track exposure amount
        let mut exposure = self.symbol_exposure.write().await;
        let current = exposure.entry(symbol.to_string()).or_insert(Decimal::ZERO);
        *current += amount_usd;
        let total_exposure = *current;
        drop(exposure);

        // Get allocation for logging
        let allocation = self.get_per_symbol_allocation().await.unwrap_or(Decimal::ZERO);

        info!(
            "📊 Position opened: {} {} | exposure: ${:.2}/${:.2} ({}%) | positions: {}/{} | total: {}",
            symbol,
            &event_id[..8.min(event_id.len())],
            total_exposure,
            allocation,
            if allocation > Decimal::ZERO { (total_exposure / allocation * dec!(100)).round() } else { Decimal::ZERO },
            symbol_count,
            self.config.max_positions_per_symbol,
            total
        );

        // Invalidate balance cache
        let mut cached = self.cached_balance.write().await;
        *cached = None;
    }

    /// Record that a position was closed
    pub async fn record_position_closed(&self, event_id: &str, symbol: &str) {
        self.record_position_closed_with_amount(event_id, symbol, Decimal::ZERO).await;
    }

    /// Record that a position was closed with exposure amount released
    pub async fn record_position_closed_with_amount(&self, event_id: &str, symbol: &str, amount_usd: Decimal) {
        // Remove from event tracking
        let mut positions = self.active_positions.write().await;
        positions.remove(event_id);
        let total = positions.len();
        drop(positions);

        // Decrement symbol count
        let mut symbol_positions = self.positions_per_symbol.write().await;
        if let Some(count) = symbol_positions.get_mut(symbol) {
            if *count > 0 {
                *count -= 1;
            }
            if *count == 0 {
                symbol_positions.remove(symbol);
            }
        }
        drop(symbol_positions);

        // Release exposure amount
        let mut exposure = self.symbol_exposure.write().await;
        if let Some(current) = exposure.get_mut(symbol) {
            *current = (*current - amount_usd).max(Decimal::ZERO);
            if *current == Decimal::ZERO {
                exposure.remove(symbol);
            }
        }
        let remaining = exposure.get(symbol).copied().unwrap_or(Decimal::ZERO);
        drop(exposure);

        info!(
            "📊 Position closed: {} {} | remaining exposure: ${:.2} | total: {}",
            symbol,
            &event_id[..8.min(event_id.len())],
            remaining,
            total
        );

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
        let per_symbol_allocation = self.get_per_symbol_allocation().await.unwrap_or(Decimal::ZERO);

        // Get symbol exposures
        let exposures = self.symbol_exposure.read().await.clone();

        FundStatus {
            balance,
            available,
            position_count,
            max_positions,
            min_balance: self.config.min_balance_usd,
            fixed_amount: self.config.fixed_amount_usd,
            position_pct: self.config.position_size_pct,
            total_symbols: self.total_symbols,
            per_symbol_allocation,
            symbol_exposures: exposures,
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
    /// Total symbols for allocation
    pub total_symbols: u32,
    /// Per-symbol allocation amount
    pub per_symbol_allocation: Decimal,
    /// Current exposure per symbol
    pub symbol_exposures: std::collections::HashMap<String, Decimal>,
}

impl std::fmt::Display for FundStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "Balance: ${:.2} | Available: ${:.2} | Positions: {}/{} | Reserve: ${:.2}",
            self.balance,
            self.available,
            self.position_count,
            self.max_positions,
            self.min_balance
        )?;
        writeln!(
            f,
            "Allocation: ${:.2} per symbol ({} symbols = {}% each)",
            self.per_symbol_allocation,
            self.total_symbols,
            100 / self.total_symbols
        )?;
        if !self.symbol_exposures.is_empty() {
            write!(f, "Exposure: ")?;
            for (symbol, exposure) in &self.symbol_exposures {
                let pct = if self.per_symbol_allocation > Decimal::ZERO {
                    (*exposure / self.per_symbol_allocation * dec!(100)).round()
                } else {
                    Decimal::ZERO
                };
                write!(f, "{}=${:.2}({}%) ", symbol, exposure, pct)?;
            }
        }
        Ok(())
    }
}

impl FundStatus {
    /// Simple one-line display (original format)
    pub fn one_line(&self) -> String {
        format!(
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
