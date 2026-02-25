//! Official Polymarket CLOB Client using polymarket-client-sdk
//!
//! This module provides a client that uses the official Polymarket SDK internally
//! while preserving the current adapter API surface.

use crate::domain::{OrderRequest, OrderSide};
use crate::error::{PloyError, Result};
use alloy::primitives::U256;
use alloy::signers::local::PrivateKeySigner;
use chrono::Utc;
use polymarket_client_sdk::clob::types::{
    request::{
        BalanceAllowanceRequest, LastTradePriceRequest, MidpointRequest, OrderBookSummaryRequest,
        PriceRequest,
    },
    AssetType, OrderType as SdkOrderType, Side as SdkSide,
};
use polymarket_client_sdk::clob::{Client, Config};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::{info, instrument};

/// Chain IDs
pub const POLYGON_CHAIN_ID: u64 = 137;
pub const AMOY_CHAIN_ID: u64 = 80002;

/// Order response mapped to the existing adapter types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResponse {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_matched: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_type: Option<String>,
}

/// Order book response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookResponse {
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
    pub hash: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: Decimal,
    pub size: Decimal,
}

/// Best prices for a token
#[derive(Debug, Clone)]
pub struct BestPrices {
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub spread: Option<Decimal>,
}

/// Polymarket client using official SDK
pub struct SdkPolymarketClient {
    /// The official SDK client (unauthenticated for read operations)
    read_client: Client,
    /// Signer for authenticated operations
    signer: Option<PrivateKeySigner>,
    /// Base URL
    base_url: String,
    /// Dry run mode
    dry_run: bool,
}

impl SdkPolymarketClient {
    /// Create a new unauthenticated client (read-only)
    pub fn new(base_url: &str, dry_run: bool) -> Result<Self> {
        let config = Config::default();
        let client = Client::new(base_url, config)
            .map_err(|e| PloyError::Internal(format!("Failed to create SDK client: {}", e)))?;

        info!("Created Polymarket SDK client (read-only)");

        Ok(Self {
            read_client: client,
            signer: None,
            base_url: base_url.to_string(),
            dry_run,
        })
    }

    /// Create an authenticated client
    pub async fn new_authenticated(base_url: &str, private_key: &str) -> Result<Self> {
        let config = Config::default();
        let client = Client::new(base_url, config)
            .map_err(|e| PloyError::Internal(format!("Failed to create SDK client: {}", e)))?;

        // Parse private key
        let signer: PrivateKeySigner = private_key
            .parse()
            .map_err(|e| PloyError::Wallet(format!("Invalid private key: {}", e)))?;

        info!(
            "Created authenticated Polymarket SDK client, address: {:?}",
            signer.address()
        );

        Ok(Self {
            read_client: client,
            signer: Some(signer),
            base_url: base_url.to_string(),
            dry_run: false,
        })
    }

    /// Check if authenticated
    pub fn is_authenticated(&self) -> bool {
        self.signer.is_some()
    }

    /// Check if dry run mode
    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Get midpoint price for a token
    #[instrument(skip(self))]
    pub async fn get_midpoint(&self, token_id: &str) -> Result<Decimal> {
        let token_id_u256 = U256::from_str(token_id)
            .map_err(|e| PloyError::Validation(format!("invalid token_id: {e}")))?;
        let req = MidpointRequest::builder().token_id(token_id_u256).build();

        let resp = self
            .read_client
            .midpoint(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get midpoint: {}", e)))?;

        Ok(resp.mid)
    }

    /// Get order book for a token
    #[instrument(skip(self))]
    pub async fn get_order_book(&self, token_id: &str) -> Result<OrderBookResponse> {
        let token_id_u256 = U256::from_str(token_id)
            .map_err(|e| PloyError::Validation(format!("invalid token_id: {e}")))?;
        let req = OrderBookSummaryRequest::builder()
            .token_id(token_id_u256)
            .build();

        let resp = self
            .read_client
            .order_book(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get order book: {}", e)))?;

        Ok(OrderBookResponse {
            bids: resp
                .bids
                .into_iter()
                .map(|l| PriceLevel {
                    price: l.price,
                    size: l.size,
                })
                .collect(),
            asks: resp
                .asks
                .into_iter()
                .map(|l| PriceLevel {
                    price: l.price,
                    size: l.size,
                })
                .collect(),
            hash: resp.hash.unwrap_or_default(),
            timestamp: resp.timestamp.timestamp_millis(),
        })
    }

    /// Get best bid/ask prices by examining the order book
    #[instrument(skip(self))]
    pub async fn get_best_prices(&self, token_id: &str) -> Result<BestPrices> {
        let order_book = self.get_order_book(token_id).await?;

        let best_bid = order_book.bids.first().map(|l| l.price);
        let best_ask = order_book.asks.first().map(|l| l.price);
        let spread = match (best_bid, best_ask) {
            (Some(bid), Some(ask)) => Some(ask - bid),
            _ => None,
        };

        Ok(BestPrices {
            best_bid,
            best_ask,
            spread,
        })
    }

    /// Get price for a specific side
    #[instrument(skip(self))]
    pub async fn get_price(&self, token_id: &str, side: &str) -> Result<Decimal> {
        let sdk_side = match side.to_uppercase().as_str() {
            "BUY" => SdkSide::Buy,
            "SELL" => SdkSide::Sell,
            _ => return Err(PloyError::Validation(format!("Invalid side: {}", side))),
        };

        let token_id_u256 = U256::from_str(token_id)
            .map_err(|e| PloyError::Validation(format!("invalid token_id: {e}")))?;
        let req = PriceRequest::builder()
            .token_id(token_id_u256)
            .side(sdk_side)
            .build();

        let resp = self
            .read_client
            .price(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get price: {}", e)))?;

        Ok(resp.price)
    }

    /// Get last trade price
    #[instrument(skip(self))]
    pub async fn get_last_trade_price(&self, token_id: &str) -> Result<Decimal> {
        let token_id_u256 = U256::from_str(token_id)
            .map_err(|e| PloyError::Validation(format!("invalid token_id: {e}")))?;
        let req = LastTradePriceRequest::builder()
            .token_id(token_id_u256)
            .build();

        let resp =
            self.read_client.last_trade_price(&req).await.map_err(|e| {
                PloyError::Internal(format!("Failed to get last trade price: {}", e))
            })?;

        Ok(resp.price)
    }

    /// Submit an order (requires authentication)
    #[instrument(skip(self))]
    pub async fn submit_order(&self, request: &OrderRequest) -> Result<OrderResponse> {
        if self.dry_run {
            info!(
                "DRY RUN: Would submit {} order for {} shares of {} @ {}",
                request.order_side, request.shares, request.token_id, request.limit_price
            );

            return Ok(OrderResponse {
                id: request.client_order_id.clone(),
                status: "OPEN".to_string(),
                owner: None,
                market: None,
                asset_id: Some(request.token_id.clone()),
                side: Some(format!("{:?}", request.order_side)),
                original_size: Some(request.shares.to_string()),
                size_matched: Some("0".to_string()),
                price: Some(request.limit_price.to_string()),
                created_at: Some(Utc::now().to_rfc3339()),
                order_type: Some("GTC".to_string()),
            });
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        // Authenticate for this operation
        let auth_client = self
            .read_client
            .clone()
            .authentication_builder(signer)
            .authenticate()
            .await
            .map_err(|e| PloyError::Auth(format!("Authentication failed: {}", e)))?;

        // Build the order
        let sdk_side = match request.order_side {
            OrderSide::Buy => SdkSide::Buy,
            OrderSide::Sell => SdkSide::Sell,
        };

        let token_id_u256 = U256::from_str(&request.token_id)
            .map_err(|e| PloyError::Validation(format!("invalid token_id: {e}")))?;

        let order = auth_client
            .limit_order()
            .token_id(token_id_u256)
            .price(request.limit_price)
            .size(Decimal::from(request.shares))
            .side(sdk_side)
            .order_type(SdkOrderType::GTC)
            .build()
            .await
            .map_err(|e| PloyError::OrderSubmission(format!("Failed to build order: {}", e)))?;

        // Sign and submit
        let signed = auth_client
            .sign(signer, order)
            .await
            .map_err(|e| PloyError::OrderSubmission(format!("Failed to sign order: {}", e)))?;

        let resp = auth_client
            .post_order(signed)
            .await
            .map_err(|e| PloyError::OrderSubmission(format!("Failed to post order: {}", e)))?;

        info!("Order submitted successfully: {:?}", resp);

        Ok(OrderResponse {
            id: resp.order_id,
            status: format!("{:?}", resp.status),
            owner: None,
            market: None,
            asset_id: Some(request.token_id.clone()),
            side: Some(format!("{:?}", request.order_side)),
            original_size: Some(request.shares.to_string()),
            size_matched: Some("0".to_string()),
            price: Some(request.limit_price.to_string()),
            created_at: Some(Utc::now().to_rfc3339()),
            order_type: Some("GTC".to_string()),
        })
    }

    /// Cancel an order
    #[instrument(skip(self))]
    pub async fn cancel_order(&self, order_id: &str) -> Result<bool> {
        if self.dry_run {
            info!("DRY RUN: Would cancel order {}", order_id);
            return Ok(true);
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self
            .read_client
            .clone()
            .authentication_builder(signer)
            .authenticate()
            .await
            .map_err(|e| PloyError::Auth(format!("Authentication failed: {}", e)))?;

        auth_client
            .cancel_order(order_id)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to cancel order: {}", e)))?;

        Ok(true)
    }

    /// Cancel all orders
    #[instrument(skip(self))]
    pub async fn cancel_all_orders(&self) -> Result<()> {
        if self.dry_run {
            info!("DRY RUN: Would cancel all orders");
            return Ok(());
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self
            .read_client
            .clone()
            .authentication_builder(signer)
            .authenticate()
            .await
            .map_err(|e| PloyError::Auth(format!("Authentication failed: {}", e)))?;

        auth_client
            .cancel_all_orders()
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to cancel all orders: {}", e)))?;

        Ok(())
    }

    /// Get USDC balance
    #[instrument(skip(self))]
    pub async fn get_usdc_balance(&self) -> Result<Decimal> {
        if self.dry_run {
            return Ok(Decimal::new(10000, 2)); // $100.00 fake balance
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self
            .read_client
            .clone()
            .authentication_builder(signer)
            .authenticate()
            .await
            .map_err(|e| PloyError::Auth(format!("Authentication failed: {}", e)))?;

        let req = BalanceAllowanceRequest::builder()
            .asset_type(AssetType::Collateral)
            .build();

        let resp = auth_client
            .balance_allowance(req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get balance: {}", e)))?;

        Ok(resp.balance)
    }

    /// Health check
    pub async fn health_check(&self) -> Result<bool> {
        self.read_client
            .ok()
            .await
            .map_err(|e| PloyError::Internal(format!("Health check failed: {}", e)))?;
        Ok(true)
    }

    /// Check if geoblocked
    pub async fn check_geoblock(&self) -> Result<bool> {
        let resp = self
            .read_client
            .check_geoblock()
            .await
            .map_err(|e| PloyError::Internal(format!("Geoblock check failed: {}", e)))?;
        Ok(resp.blocked)
    }
}

impl Clone for SdkPolymarketClient {
    fn clone(&self) -> Self {
        Self {
            read_client: self.read_client.clone(),
            signer: self.signer.clone(),
            base_url: self.base_url.clone(),
            dry_run: self.dry_run,
        }
    }
}

/// Re-export SDK for direct access when needed
pub mod sdk {
    pub use polymarket_client_sdk::clob::types::*;
    pub use polymarket_client_sdk::clob::{Client, Config};
    pub use polymarket_client_sdk::{AMOY, POLYGON};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_client() {
        let client = SdkPolymarketClient::new("https://clob.polymarket.com", true).unwrap();
        assert!(client.is_dry_run());
        assert!(!client.is_authenticated());
    }

    #[tokio::test]
    async fn test_health_check() {
        let client = SdkPolymarketClient::new("https://clob.polymarket.com", false).unwrap();
        // This might fail due to network, but tests the API
        let _ = client.health_check().await;
    }
}
