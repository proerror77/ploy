//! Core traits for market strategies
//!
//! These traits define the interface that all market types must implement.

use crate::error::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Type of market being traded
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketType {
    /// Crypto UP/DOWN markets (BTC, ETH, SOL 15m/1h/4h)
    CryptoUpDown,
    /// Sports moneyline (team A vs team B)
    SportsMoneyline,
    /// Sports spread betting
    SportsSpread,
    /// Sports over/under totals
    SportsTotal,
    /// Political markets
    Political,
    /// Other binary markets
    Custom,
}

impl std::fmt::Display for MarketType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarketType::CryptoUpDown => write!(f, "Crypto UP/DOWN"),
            MarketType::SportsMoneyline => write!(f, "Sports Moneyline"),
            MarketType::SportsSpread => write!(f, "Sports Spread"),
            MarketType::SportsTotal => write!(f, "Sports Total"),
            MarketType::Political => write!(f, "Political"),
            MarketType::Custom => write!(f, "Custom"),
        }
    }
}

/// A binary market that can be traded with split arbitrage
#[derive(Debug, Clone)]
pub struct BinaryMarket {
    /// Unique event identifier
    pub event_id: String,
    
    /// Market condition ID (for Polymarket)
    pub condition_id: String,
    
    /// Token ID for "Yes" side
    pub yes_token_id: String,
    
    /// Token ID for "No" side
    pub no_token_id: String,
    
    /// Human-readable name for Yes outcome
    pub yes_label: String,
    
    /// Human-readable name for No outcome
    pub no_label: String,
    
    /// When the market resolves
    pub end_time: DateTime<Utc>,
    
    /// Type of market
    pub market_type: MarketType,
    
    /// Market-specific metadata (JSON)
    pub metadata: Option<String>,
}

impl BinaryMarket {
    /// Create a crypto UP/DOWN market
    pub fn crypto_up_down(
        event_id: String,
        condition_id: String,
        up_token_id: String,
        down_token_id: String,
        end_time: DateTime<Utc>,
    ) -> Self {
        Self {
            event_id,
            condition_id,
            yes_token_id: up_token_id,
            no_token_id: down_token_id,
            yes_label: "UP".to_string(),
            no_label: "DOWN".to_string(),
            end_time,
            market_type: MarketType::CryptoUpDown,
            metadata: None,
        }
    }
    
    /// Create a sports moneyline market
    pub fn sports_moneyline(
        event_id: String,
        condition_id: String,
        team_a_token: String,
        team_b_token: String,
        team_a_name: String,
        team_b_name: String,
        end_time: DateTime<Utc>,
    ) -> Self {
        Self {
            event_id,
            condition_id,
            yes_token_id: team_a_token,
            no_token_id: team_b_token,
            yes_label: team_a_name,
            no_label: team_b_name,
            end_time,
            market_type: MarketType::SportsMoneyline,
            metadata: None,
        }
    }
}

/// Trait for discovering tradable markets
#[async_trait]
pub trait MarketDiscovery: Send + Sync {
    /// Get the market type this discovery handles
    fn market_type(&self) -> MarketType;
    
    /// Discover all currently tradable markets
    async fn discover_markets(&self) -> Result<Vec<BinaryMarket>>;
    
    /// Get a specific market by ID
    async fn get_market(&self, event_id: &str) -> Result<Option<BinaryMarket>>;
}
