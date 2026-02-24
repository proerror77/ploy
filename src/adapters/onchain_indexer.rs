//! On-chain OrderFilled event indexer for Polymarket CTF Exchange
//!
//! Decodes `OrderFilled` events from the Polygon blockchain to enable:
//! - Whale address tracking (large maker/taker flows)
//! - Smart money signal detection
//! - Real-time trade flow analysis
//!
//! Based on data schemas from Jon Becker's prediction-market-analysis.

use alloy::sol;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Contract addresses & constants
// ============================================================================

/// Polymarket CTF Exchange on Polygon
pub const CTF_EXCHANGE: &str = "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E";

/// Polymarket NegRisk CTF Exchange on Polygon
pub const NEGRISK_CTF_EXCHANGE: &str = "0xC5d563A36AE78145C45a50134d48A1215220f80a";

/// OrderFilled event topic0
pub const ORDER_FILLED_TOPIC: &str =
    "0xd0a08e8c493f9c94f29311604c9de1b4e8c8d4c06bd0c789af57f2d65bfec0f6";

/// Block where Polymarket CTF Exchange was deployed
pub const POLYMARKET_START_BLOCK: u64 = 33_605_403;

/// USDC has 6 decimals
const USDC_DECIMALS: u32 = 6;

// ============================================================================
// Solidity event definition (alloy sol! macro)
// ============================================================================

sol! {
    /// OrderFilled event emitted by CTF Exchange contracts
    #[derive(Debug)]
    event OrderFilled(
        bytes32 indexed orderHash,
        address indexed maker,
        address indexed taker,
        uint256 makerAssetId,
        uint256 takerAssetId,
        uint256 makerAmountFilled,
        uint256 takerAmountFilled,
        uint256 fee
    );
}

// ============================================================================
// Decoded trade types
// ============================================================================

/// A decoded on-chain trade from an OrderFilled event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnChainTrade {
    pub block_number: u64,
    pub transaction_hash: String,
    pub log_index: u32,
    pub order_hash: String,
    pub maker: String,
    pub taker: String,
    pub maker_asset_id: String,
    pub taker_asset_id: String,
    /// Amount maker gave (raw, 6 decimals for USDC)
    pub maker_amount: u64,
    /// Amount taker gave (raw, 6 decimals)
    pub taker_amount: u64,
    /// Fee (raw, 6 decimals)
    pub fee: u64,
    /// Which contract emitted this event
    pub contract: ExchangeContract,
    /// Decoded timestamp (if block timestamp available)
    pub timestamp: Option<DateTime<Utc>>,
}

impl OnChainTrade {
    /// True if maker is providing USDC (asset_id == 0) → buying outcome tokens
    pub fn is_buy(&self) -> bool {
        self.maker_asset_id == "0x0" || self.maker_asset_id == "0"
    }

    /// Calculate price in USDC per outcome token
    pub fn price(&self) -> f64 {
        if self.is_buy() {
            if self.taker_amount > 0 {
                self.maker_amount as f64 / self.taker_amount as f64
            } else {
                0.0
            }
        } else if self.maker_amount > 0 {
            self.taker_amount as f64 / self.maker_amount as f64
        } else {
            0.0
        }
    }

    /// Number of outcome tokens traded (in human-readable units)
    pub fn size_tokens(&self) -> f64 {
        let raw = if self.is_buy() {
            self.taker_amount
        } else {
            self.maker_amount
        };
        raw as f64 / 10f64.powi(USDC_DECIMALS as i32)
    }

    /// USDC volume of this trade
    pub fn volume_usdc(&self) -> f64 {
        let raw = if self.is_buy() {
            self.maker_amount
        } else {
            self.taker_amount
        };
        raw as f64 / 10f64.powi(USDC_DECIMALS as i32)
    }

    /// Trade side from taker's perspective
    pub fn taker_side(&self) -> &'static str {
        if self.is_buy() {
            "BUY"
        } else {
            "SELL"
        }
    }

    /// The outcome token's asset ID (the non-USDC side)
    pub fn token_asset_id(&self) -> &str {
        if self.is_buy() {
            &self.taker_asset_id
        } else {
            &self.maker_asset_id
        }
    }
}

/// Which exchange contract emitted the event
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExchangeContract {
    CTFExchange,
    NegRiskCTFExchange,
}

impl ExchangeContract {
    pub fn address(&self) -> &'static str {
        match self {
            Self::CTFExchange => CTF_EXCHANGE,
            Self::NegRiskCTFExchange => NEGRISK_CTF_EXCHANGE,
        }
    }
}

// ============================================================================
// Whale tracker
// ============================================================================

/// Tracks trading activity by address for whale detection
#[derive(Debug, Clone, Default)]
pub struct WhaleTracker {
    /// address → cumulative volume in USDC
    volumes: HashMap<String, f64>,
    /// address → trade count
    trade_counts: HashMap<String, u64>,
    /// address → net position (positive = net buyer)
    net_positions: HashMap<String, f64>,
    /// Minimum volume to be considered a whale (USD)
    whale_threshold: f64,
}

impl WhaleTracker {
    pub fn new(whale_threshold: f64) -> Self {
        Self {
            whale_threshold,
            ..Default::default()
        }
    }

    /// Record a trade for both maker and taker
    pub fn record_trade(&mut self, trade: &OnChainTrade) {
        let vol = trade.volume_usdc();
        let size = trade.size_tokens();

        // Maker
        *self.volumes.entry(trade.maker.clone()).or_default() += vol;
        *self.trade_counts.entry(trade.maker.clone()).or_default() += 1;

        // Taker
        *self.volumes.entry(trade.taker.clone()).or_default() += vol;
        *self.trade_counts.entry(trade.taker.clone()).or_default() += 1;

        // Net position tracking (taker buys → positive, sells → negative)
        if trade.is_buy() {
            *self.net_positions.entry(trade.taker.clone()).or_default() += size;
            *self.net_positions.entry(trade.maker.clone()).or_default() -= size;
        } else {
            *self.net_positions.entry(trade.taker.clone()).or_default() -= size;
            *self.net_positions.entry(trade.maker.clone()).or_default() += size;
        }
    }

    /// Get addresses that exceed the whale threshold, sorted by volume
    pub fn whales(&self) -> Vec<WhaleInfo> {
        let mut whales: Vec<WhaleInfo> = self
            .volumes
            .iter()
            .filter(|(_, vol)| **vol >= self.whale_threshold)
            .map(|(addr, vol)| WhaleInfo {
                address: addr.clone(),
                total_volume: *vol,
                trade_count: self.trade_counts.get(addr).copied().unwrap_or(0),
                net_position: self.net_positions.get(addr).copied().unwrap_or(0.0),
            })
            .collect();

        whales.sort_by(|a, b| {
            b.total_volume
                .partial_cmp(&a.total_volume)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        whales
    }

    /// Check if a specific address is a known whale
    pub fn is_whale(&self, address: &str) -> bool {
        self.volumes
            .get(address)
            .map(|v| *v >= self.whale_threshold)
            .unwrap_or(false)
    }

    /// Get the net direction of whale activity (positive = net buying)
    pub fn whale_net_flow(&self) -> f64 {
        self.whales().iter().map(|w| w.net_position).sum()
    }
}

/// Summary of a whale address
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhaleInfo {
    pub address: String,
    pub total_volume: f64,
    pub trade_count: u64,
    /// Positive = net buyer, negative = net seller
    pub net_position: f64,
}

impl WhaleInfo {
    /// Is this whale a net buyer?
    pub fn is_net_buyer(&self) -> bool {
        self.net_position > 0.0
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_trade(maker_amount: u64, taker_amount: u64, maker_asset_zero: bool) -> OnChainTrade {
        OnChainTrade {
            block_number: 50_000_000,
            transaction_hash: "0xabc".into(),
            log_index: 0,
            order_hash: "0x123".into(),
            maker: "0xmaker".into(),
            taker: "0xtaker".into(),
            maker_asset_id: if maker_asset_zero {
                "0".into()
            } else {
                "12345".into()
            },
            taker_asset_id: if maker_asset_zero {
                "12345".into()
            } else {
                "0".into()
            },
            maker_amount,
            taker_amount,
            fee: 1000,
            contract: ExchangeContract::CTFExchange,
            timestamp: None,
        }
    }

    #[test]
    fn test_trade_price_buy() {
        // Maker gives 650_000 USDC (0.65), taker gives 1_000_000 tokens
        let trade = mock_trade(650_000, 1_000_000, true);
        assert!(trade.is_buy());
        assert!((trade.price() - 0.65).abs() < 0.001);
        assert!((trade.size_tokens() - 1.0).abs() < 0.001);
        assert!((trade.volume_usdc() - 0.65).abs() < 0.001);
        assert_eq!(trade.taker_side(), "BUY");
    }

    #[test]
    fn test_trade_price_sell() {
        // Maker gives 1_000_000 tokens, taker gives 700_000 USDC
        let trade = mock_trade(1_000_000, 700_000, false);
        assert!(!trade.is_buy());
        assert!((trade.price() - 0.70).abs() < 0.001);
        assert_eq!(trade.taker_side(), "SELL");
    }

    #[test]
    fn test_whale_tracker() {
        let mut tracker = WhaleTracker::new(1.0); // $1 threshold for testing

        // Two trades: taker buys $0.65 and $0.80
        let t1 = mock_trade(650_000, 1_000_000, true);
        let t2 = mock_trade(800_000, 1_000_000, true);
        tracker.record_trade(&t1);
        tracker.record_trade(&t2);

        // Taker volume = 0.65 + 0.80 = 1.45 → whale
        assert!(tracker.is_whale("0xtaker"));
        // Maker volume = 0.65 + 0.80 = 1.45 → whale
        assert!(tracker.is_whale("0xmaker"));

        let whales = tracker.whales();
        assert_eq!(whales.len(), 2);

        // Taker is net buyer
        let taker_whale = whales.iter().find(|w| w.address == "0xtaker").unwrap();
        assert!(taker_whale.is_net_buyer());
    }

    #[test]
    fn test_exchange_contract_addresses() {
        assert_eq!(
            ExchangeContract::CTFExchange.address(),
            "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E"
        );
        assert_eq!(
            ExchangeContract::NegRiskCTFExchange.address(),
            "0xC5d563A36AE78145C45a50134d48A1215220f80a"
        );
    }
}
