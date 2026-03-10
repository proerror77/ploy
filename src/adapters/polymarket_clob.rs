//! Polymarket CLOB API client using official SDK
//!
//! This module provides a client that uses the official polymarket-client-sdk
//! for both CLOB (trading) and Gamma (market discovery) operations.

use crate::domain::{OrderRequest, OrderSide, OrderStatus, TimeInForce};
use crate::error::{PloyError, Result};
use crate::exchange::{ExchangeClient, ExchangeKind};
use crate::signing::Wallet;
use alloy::primitives::{B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use async_trait::async_trait;
use chrono::Utc;
use polymarket_client_sdk::auth::{state::Authenticated, Normal};
use polymarket_client_sdk::clob::types::{
    request::{
        BalanceAllowanceRequest, CancelMarketOrderRequest, OrderBookSummaryRequest, OrdersRequest,
        TradesRequest,
    },
    AssetType, OrderType as SdkOrderType, Side as SdkSide, SignatureType as SdkSignatureType,
};
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::data::Client as DataClient;
use polymarket_client_sdk::gamma::types::request::MarketsRequest;
use polymarket_client_sdk::gamma::Client as GammaClient;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, instrument, warn};
use zeroize::Zeroize;

/// Chain ID for Polygon Mainnet
pub const POLYGON_CHAIN_ID: u64 = 137;

/// Gamma API base URL
pub const GAMMA_API_URL: &str = "https://gamma-api.polymarket.com";
const CLOB_TERMINAL_CURSOR: &str = "LTE="; // base64("-1"), used by CLOB pagination

type AuthClobClient = ClobClient<Authenticated<Normal>>;

mod gamma;
mod read_api;

pub use gamma::{GammaEventInfo, GammaMarketInfo, GammaSeriesResponse, GammaTokenInfo};

tokio::task_local! {
    static GATEWAY_EXECUTION_CONTEXT: bool;
}

/// Polymarket CLOB API client using official SDK
pub struct PolymarketClient {
    /// SDK CLOB client for trading operations
    clob_client: ClobClient,
    /// SDK Gamma client for market discovery
    gamma_client: GammaClient,
    /// Private key signer for authenticated operations
    signer: Option<PrivateKeySigner>,
    /// Optional wallet handle for authenticated signing flows
    wallet: Option<Arc<Wallet>>,
    /// Funder address (proxy wallet that holds funds)
    funder: Option<alloy::primitives::Address>,
    /// Base URL
    base_url: String,
    /// Dry run mode
    dry_run: bool,
    /// Whether to use negative risk exchange
    neg_risk: bool,
    /// Mutex to serialize order submissions (prevents auth race condition)
    order_mutex: Arc<Mutex<()>>,
    /// Cached authenticated CLOB client (API key) to avoid spamming `/auth/api-key`.
    auth_client: Arc<Mutex<Option<AuthClobClient>>>,
}

impl Clone for PolymarketClient {
    fn clone(&self) -> Self {
        Self {
            clob_client: self.clob_client.clone(),
            gamma_client: self.gamma_client.clone(),
            signer: self.signer.clone(),
            wallet: self.wallet.clone(),
            funder: self.funder,
            base_url: self.base_url.clone(),
            dry_run: self.dry_run,
            neg_risk: self.neg_risk,
            order_mutex: self.order_mutex.clone(), // Share mutex across clones
            auth_client: self.auth_client.clone(),
        }
    }
}

// ==================== API Response Types ====================

/// Market response from CLOB API
#[derive(Debug, Deserialize, Serialize)]
pub struct MarketResponse {
    pub condition_id: String,
    #[serde(default)]
    pub question_id: Option<String>,
    #[serde(default)]
    pub tokens: Vec<TokenInfo>,
    #[serde(default)]
    pub minimum_order_size: Option<serde_json::Value>,
    #[serde(default)]
    pub minimum_tick_size: Option<serde_json::Value>,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub end_date_iso: Option<String>,
    #[serde(default)]
    pub neg_risk: Option<bool>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TokenInfo {
    pub token_id: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default, deserialize_with = "deserialize_price")]
    pub price: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

fn deserialize_price<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        Some(serde_json::Value::Number(n)) => Ok(Some(n.to_string())),
        Some(other) => Ok(Some(other.to_string())),
    }
}

fn deserialize_stringified<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(String::new()),
        Some(serde_json::Value::String(s)) => Ok(s),
        Some(serde_json::Value::Number(n)) => Ok(n.to_string()),
        Some(serde_json::Value::Bool(b)) => Ok(b.to_string()),
        Some(other) => Ok(other.to_string()),
    }
}

fn deserialize_option_stringified<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        Some(serde_json::Value::Number(n)) => Ok(Some(n.to_string())),
        Some(serde_json::Value::Bool(b)) => Ok(Some(b.to_string())),
        Some(other) => Ok(Some(other.to_string())),
    }
}

fn deserialize_option_boolish<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(b)) => Ok(Some(b)),
        Some(serde_json::Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                Ok(Some(i != 0))
            } else if let Some(u) = n.as_u64() {
                Ok(Some(u != 0))
            } else if let Some(f) = n.as_f64() {
                Ok(Some(f != 0.0))
            } else {
                Ok(None)
            }
        }
        Some(serde_json::Value::String(s)) => {
            let normalized = s.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "true" | "1" | "yes" | "y" | "on" => Ok(Some(true)),
                "false" | "0" | "no" | "n" | "off" => Ok(Some(false)),
                _ => Ok(None),
            }
        }
        Some(_) => Ok(None),
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OrderBookResponse {
    pub market: Option<String>,
    pub asset_id: String,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub timestamp: Option<String>,
    pub hash: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OrderBookLevel {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderResponse {
    pub id: String,
    pub status: String,
    pub owner: Option<String>,
    pub market: Option<String>,
    pub asset_id: Option<String>,
    pub side: Option<String>,
    pub original_size: Option<String>,
    pub size_matched: Option<String>,
    pub price: Option<String>,
    pub associate_trades: Option<Vec<TradeInfo>>,
    pub created_at: Option<String>,
    pub expiration: Option<String>,
    #[serde(rename = "type")]
    pub order_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TradeInfo {
    pub id: String,
    pub taker_order_id: String,
    pub market: String,
    pub asset_id: String,
    pub side: String,
    pub size: String,
    pub fee_rate_bps: String,
    pub price: String,
    pub status: String,
    pub match_time: String,
    pub outcome: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateOrderResponse {
    pub success: Option<bool>,
    pub error_msg: Option<String>,
    #[serde(rename = "orderID")]
    pub order_id: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CancelOrderResponse {
    pub canceled: Option<Vec<String>>,
    pub not_canceled: Option<Vec<NotCanceledOrder>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NotCanceledOrder {
    pub order_id: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MarketsSearchResponse {
    pub data: Option<Vec<MarketSummary>>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MarketSummary {
    pub condition_id: String,
    pub question: Option<String>,
    pub slug: Option<String>,
    pub active: bool,
    /// JSON-encoded CLOB token IDs (e.g., '["token_yes","token_no"]')
    #[serde(default)]
    pub clob_token_ids: Option<String>,
    /// JSON-encoded outcome prices (e.g., '["0.65","0.35"]')
    #[serde(default)]
    pub outcome_prices: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct ApiKeyResponse {
    #[serde(rename = "apiKey")]
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}

impl std::fmt::Debug for ApiKeyResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyResponse")
            .field(
                "api_key",
                &format_args!("{}...", &self.api_key[..8.min(self.api_key.len())]),
            )
            .field("secret", &"[REDACTED]")
            .field("passphrase", &"[REDACTED]")
            .finish()
    }
}

// ==================== Account & Position Types ====================

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BalanceResponse {
    pub balance: String,
    #[serde(default)]
    pub allowance: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PositionResponse {
    #[serde(
        default,
        alias = "assetId",
        deserialize_with = "deserialize_stringified"
    )]
    pub asset_id: String,
    #[serde(
        default,
        alias = "token_id",
        alias = "tokenId",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub token_id: Option<String>,
    #[serde(
        default,
        alias = "conditionId",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub condition_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_option_stringified")]
    pub outcome: Option<String>,
    #[serde(
        default,
        alias = "outcomeIndex",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub outcome_index: Option<String>,
    #[serde(default, deserialize_with = "deserialize_stringified")]
    pub size: String,
    #[serde(
        default,
        alias = "avgPrice",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub avg_price: Option<String>,
    #[serde(
        default,
        alias = "realizedPnl",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub realized_pnl: Option<String>,
    #[serde(
        default,
        alias = "unrealizedPnl",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub unrealized_pnl: Option<String>,
    #[serde(
        default,
        alias = "curPrice",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub cur_price: Option<String>,
    #[serde(
        default,
        alias = "isRedeemable",
        deserialize_with = "deserialize_option_boolish"
    )]
    pub redeemable: Option<bool>,
    #[serde(
        default,
        alias = "negativeRisk",
        deserialize_with = "deserialize_option_boolish"
    )]
    pub negative_risk: Option<bool>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl PositionResponse {
    pub fn value(&self) -> Option<Decimal> {
        let size = self.size.parse::<Decimal>().ok()?;
        let price = self.avg_price.as_ref()?.parse::<Decimal>().ok()?;
        Some(size * price)
    }

    pub fn market_value(&self) -> Option<Decimal> {
        let size = self.size.parse::<Decimal>().ok()?;
        let price = self.cur_price.as_ref()?.parse::<Decimal>().ok()?;
        Some(size * price)
    }

    /// Calculate payout if this position wins (size * $1)
    pub fn payout_if_win(&self) -> Option<Decimal> {
        let size = self.size.parse::<Decimal>().ok()?;
        Some(size) // Each winning share pays $1
    }

    /// Check if this position is redeemable
    pub fn is_redeemable(&self) -> bool {
        self.redeemable.unwrap_or(false)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TradeResponse {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub asset_id: String,
    #[serde(default)]
    pub side: String,
    #[serde(default)]
    pub price: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub fee: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountSummary {
    pub usdc_balance: Decimal,
    pub open_order_count: usize,
    pub open_order_value: Decimal,
    pub position_count: usize,
    pub position_value: Decimal,
    pub total_equity: Decimal,
    pub open_orders: Vec<OrderResponse>,
    pub positions: Vec<PositionResponse>,
}

impl AccountSummary {
    pub fn has_sufficient_balance(&self, required: Decimal) -> bool {
        self.usdc_balance >= required
    }

    pub fn available_balance(&self) -> Decimal {
        (self.usdc_balance - self.open_order_value).max(Decimal::ZERO)
    }

    pub fn log_summary(&self) {
        info!(
            "Account Summary: USDC={:.2}, Orders={} (${:.2}), Positions={} (${:.2}), Equity=${:.2}",
            self.usdc_balance,
            self.open_order_count,
            self.open_order_value,
            self.position_count,
            self.position_value,
            self.total_equity
        );
    }
}

// ==================== Implementation ====================

impl PolymarketClient {
    pub async fn with_gateway_execution_context<F, T>(future: F) -> T
    where
        F: Future<Output = T>,
    {
        GATEWAY_EXECUTION_CONTEXT.scope(true, future).await
    }

    fn parse_boolish(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "y" | "on"
        )
    }

    fn env_bool(keys: &[&str]) -> bool {
        keys.iter()
            .find_map(|k| std::env::var(k).ok())
            .map(|v| Self::parse_boolish(&v))
            .unwrap_or(false)
    }

    fn env_string(keys: &[&str]) -> Option<String> {
        keys.iter()
            .find_map(|k| std::env::var(k).ok())
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
    }

    fn gateway_only_mode_enabled() -> bool {
        let explicit_gate = Self::env_bool(&[
            "PLOY_GATEWAY_ONLY",
            "PLOY_ENFORCE_GATEWAY_ONLY",
            "PLOY_ENFORCE_COORDINATOR_GATEWAY_ONLY",
        ]);

        let openclaw_mode =
            Self::env_string(&["PLOY_AGENT_FRAMEWORK__MODE", "PLOY_AGENT_FRAMEWORK_MODE"])
                .is_some_and(|mode| mode == "openclaw");
        let openclaw_hard_disable = Self::env_bool(&[
            "PLOY_AGENT_FRAMEWORK__HARD_DISABLE_INTERNAL_AGENTS",
            "PLOY_AGENT_FRAMEWORK_HARD_DISABLE_INTERNAL_AGENTS",
            "PLOY_OPENCLAW_ONLY",
        ]);

        explicit_gate || (openclaw_mode && openclaw_hard_disable)
    }

    fn gateway_execution_context_active() -> bool {
        GATEWAY_EXECUTION_CONTEXT.try_with(|v| *v).unwrap_or(false)
    }

    fn validate_gateway_execution_context(dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }

        if Self::gateway_execution_context_active() {
            return Ok(());
        }

        Err(PloyError::Validation(
            "direct order submission is disabled; route writes through coordinator/execution gateway"
                .to_string(),
        ))
    }

    fn validate_gateway_order_request_inner(request: &OrderRequest, enforce: bool) -> Result<()> {
        if !enforce {
            return Ok(());
        }

        let has_idempotency_key = request
            .idempotency_key
            .as_deref()
            .map(str::trim)
            .is_some_and(|v| !v.is_empty());
        if !has_idempotency_key {
            return Err(PloyError::Validation(
                "gateway-only mode: idempotency_key is required (route writes through coordinator/gateway)"
                    .to_string(),
            ));
        }

        if !request.client_order_id.starts_with("intent:") {
            return Err(PloyError::Validation(
                "gateway-only mode: client_order_id must start with 'intent:'".to_string(),
            ));
        }

        Ok(())
    }

    fn validate_gateway_order_request(request: &OrderRequest) -> Result<()> {
        Self::validate_gateway_order_request_inner(request, Self::gateway_only_mode_enabled())
    }

    pub fn parse_order_status(status: &str) -> OrderStatus {
        match status.to_uppercase().as_str() {
            "LIVE" | "OPEN" => OrderStatus::Submitted,
            "MATCHED" => OrderStatus::PartiallyFilled,
            "FILLED" => OrderStatus::Filled,
            "CANCELED" | "CANCELLED" => OrderStatus::Cancelled,
            "REJECTED" => OrderStatus::Rejected,
            "EXPIRED" => OrderStatus::Expired,
            "DELAYED" | "PENDING" => OrderStatus::Pending,
            _ => OrderStatus::Pending,
        }
    }

    /// Infer order status using both the status string and fill amounts.
    ///
    /// Polymarket can report intermediate states like `MATCHED`; use size_matched/original_size
    /// when available to distinguish partial vs full fills.
    pub fn infer_order_status(order: &OrderResponse) -> OrderStatus {
        let status = order.status.to_uppercase();

        // Terminal status overrides (may still have partial fills).
        match status.as_str() {
            "CANCELED" | "CANCELLED" => return OrderStatus::Cancelled,
            "REJECTED" => return OrderStatus::Rejected,
            "EXPIRED" => return OrderStatus::Expired,
            _ => {}
        }

        let size_matched = order
            .size_matched
            .as_ref()
            .and_then(|s| s.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO);
        let original_size = order
            .original_size
            .as_ref()
            .and_then(|s| s.parse::<Decimal>().ok());

        if size_matched > Decimal::ZERO {
            if let Some(orig) = original_size {
                if size_matched >= orig {
                    return OrderStatus::Filled;
                }
                return OrderStatus::PartiallyFilled;
            }

            // If we can't read original size, fall back to the status string.
            if status == "FILLED" || status == "MATCHED" {
                return OrderStatus::Filled;
            }
            return OrderStatus::PartiallyFilled;
        }

        // No fills observed yet.
        match status.as_str() {
            "LIVE" | "OPEN" => OrderStatus::Submitted,
            "DELAYED" | "PENDING" => OrderStatus::Pending,
            // Some APIs return MATCHED before size_matched is populated.
            "MATCHED" => OrderStatus::Submitted,
            "FILLED" => OrderStatus::Filled,
            _ => OrderStatus::Pending,
        }
    }

    /// Calculate fill amount and average price from an order
    /// Returns (filled_shares, avg_price)
    pub fn calculate_fill(order: &OrderResponse) -> (Decimal, Decimal) {
        let size_matched = order
            .size_matched
            .as_ref()
            .and_then(|s| s.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO);

        // Prefer a weighted average from associated trades when available.
        let avg_price = if let Some(trades) = order.associate_trades.as_ref() {
            let mut total_size = Decimal::ZERO;
            let mut total_notional = Decimal::ZERO;

            for t in trades {
                let Some(size) = t.size.parse::<Decimal>().ok() else {
                    continue;
                };
                let Some(price) = t.price.parse::<Decimal>().ok() else {
                    continue;
                };
                if size <= Decimal::ZERO || price <= Decimal::ZERO {
                    continue;
                }
                total_size += size;
                total_notional += size * price;
            }

            if total_size > Decimal::ZERO {
                total_notional / total_size
            } else {
                order
                    .price
                    .as_ref()
                    .and_then(|p| p.parse::<Decimal>().ok())
                    .unwrap_or(Decimal::ZERO)
            }
        } else {
            order
                .price
                .as_ref()
                .and_then(|p| p.parse::<Decimal>().ok())
                .unwrap_or(Decimal::ZERO)
        };

        (size_matched, avg_price)
    }
}

#[async_trait]
impl ExchangeClient for PolymarketClient {
    fn kind(&self) -> ExchangeKind {
        ExchangeKind::Polymarket
    }

    fn is_dry_run(&self) -> bool {
        PolymarketClient::is_dry_run(self)
    }

    async fn submit_order_gateway(&self, request: &OrderRequest) -> Result<OrderResponse> {
        PolymarketClient::with_gateway_execution_context(self.submit_order(request)).await
    }

    async fn get_order(&self, order_id: &str) -> Result<OrderResponse> {
        PolymarketClient::get_order(self, order_id).await
    }

    async fn cancel_order(&self, order_id: &str) -> Result<bool> {
        PolymarketClient::cancel_order(self, order_id).await
    }

    async fn get_best_prices(&self, token_id: &str) -> Result<(Option<Decimal>, Option<Decimal>)> {
        PolymarketClient::get_best_prices(self, token_id).await
    }

    fn infer_order_status(&self, order: &OrderResponse) -> OrderStatus {
        PolymarketClient::infer_order_status(order)
    }

    fn calculate_fill(&self, order: &OrderResponse) -> (u64, Option<Decimal>) {
        let (filled, avg) = PolymarketClient::calculate_fill(order);
        (filled.to_u64().unwrap_or(0), Some(avg))
    }

    async fn get_market(&self, market_id: &str) -> Result<MarketResponse> {
        PolymarketClient::get_market(self, market_id).await
    }

    async fn search_markets(&self, query: &str) -> Result<Vec<MarketSummary>> {
        PolymarketClient::search_markets(self, query).await
    }

    async fn get_balance(&self) -> Result<BalanceResponse> {
        PolymarketClient::get_balance(self).await
    }

    async fn get_positions(&self) -> Result<Vec<PositionResponse>> {
        PolymarketClient::get_positions(self).await
    }

    async fn get_order_history(&self, limit: Option<u32>) -> Result<Vec<OrderResponse>> {
        PolymarketClient::get_order_history(self, limit).await
    }

    async fn get_trades(&self, limit: Option<u32>) -> Result<Vec<TradeResponse>> {
        PolymarketClient::get_trades(self, limit).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn test_create_client() {
        let client = PolymarketClient::new("https://clob.polymarket.com", true).unwrap();
        assert!(client.is_dry_run());
        assert!(!client.has_hmac_auth());
    }

    #[test]
    fn test_parse_order_status() {
        assert!(matches!(
            PolymarketClient::parse_order_status("LIVE"),
            OrderStatus::Submitted
        ));
        assert!(matches!(
            PolymarketClient::parse_order_status("MATCHED"),
            OrderStatus::PartiallyFilled
        ));
        assert!(matches!(
            PolymarketClient::parse_order_status("CANCELED"),
            OrderStatus::Cancelled
        ));
    }

    #[test]
    fn test_infer_order_status_uses_sizes() {
        let base = OrderResponse {
            id: "1".to_string(),
            status: "MATCHED".to_string(),
            owner: None,
            market: None,
            asset_id: None,
            side: None,
            original_size: Some("10".to_string()),
            size_matched: Some("5".to_string()),
            price: Some("0.50".to_string()),
            associate_trades: None,
            created_at: None,
            expiration: None,
            order_type: None,
        };

        assert_eq!(
            PolymarketClient::infer_order_status(&base),
            OrderStatus::PartiallyFilled
        );

        let mut full = base.clone();
        full.size_matched = Some("10".to_string());
        assert_eq!(
            PolymarketClient::infer_order_status(&full),
            OrderStatus::Filled
        );

        let mut cancelled = base.clone();
        cancelled.status = "CANCELED".to_string();
        assert_eq!(
            PolymarketClient::infer_order_status(&cancelled),
            OrderStatus::Cancelled
        );
    }

    #[test]
    fn test_calculate_fill_prefers_associated_trades() {
        let order = OrderResponse {
            id: "1".to_string(),
            status: "FILLED".to_string(),
            owner: None,
            market: None,
            asset_id: None,
            side: None,
            original_size: Some("5".to_string()),
            size_matched: Some("5".to_string()),
            price: Some("0.99".to_string()), // should be ignored if trades present
            associate_trades: Some(vec![
                TradeInfo {
                    id: "t1".to_string(),
                    taker_order_id: "o1".to_string(),
                    market: "m".to_string(),
                    asset_id: "a".to_string(),
                    side: "BUY".to_string(),
                    size: "2".to_string(),
                    fee_rate_bps: "0".to_string(),
                    price: "0.40".to_string(),
                    status: "MATCHED".to_string(),
                    match_time: "now".to_string(),
                    outcome: None,
                },
                TradeInfo {
                    id: "t2".to_string(),
                    taker_order_id: "o1".to_string(),
                    market: "m".to_string(),
                    asset_id: "a".to_string(),
                    side: "BUY".to_string(),
                    size: "3".to_string(),
                    fee_rate_bps: "0".to_string(),
                    price: "0.50".to_string(),
                    status: "MATCHED".to_string(),
                    match_time: "now".to_string(),
                    outcome: None,
                },
            ]),
            created_at: None,
            expiration: None,
            order_type: None,
        };

        let (filled, avg) = PolymarketClient::calculate_fill(&order);
        assert_eq!(filled, dec!(5));
        // (2*0.40 + 3*0.50)/5 = 0.46
        assert_eq!(avg, dec!(0.46));
    }

    #[test]
    fn test_position_response_deserializes_numeric_fields() {
        let raw = serde_json::json!({
            "asset_id": 12345,
            "token_id": 67890,
            "condition_id": "abc123",
            "outcome": "Yes",
            "size": 49.4701,
            "avg_price": 0.5,
            "realized_pnl": -1.23,
            "unrealized_pnl": 0.0,
            "cur_price": 1,
            "redeemable": 1
        });

        let pos: PositionResponse =
            serde_json::from_value(raw).expect("position should deserialize");
        assert_eq!(pos.asset_id, "12345");
        assert_eq!(pos.token_id.as_deref(), Some("67890"));
        assert_eq!(pos.size, "49.4701");
        assert_eq!(pos.avg_price.as_deref(), Some("0.5"));
        assert_eq!(pos.cur_price.as_deref(), Some("1"));
        assert_eq!(pos.redeemable, Some(true));
    }

    #[test]
    fn test_position_response_deserializes_redeemable_string() {
        let raw = serde_json::json!({
            "asset_id": "token",
            "size": "10",
            "redeemable": "true"
        });

        let pos: PositionResponse =
            serde_json::from_value(raw).expect("position should deserialize");
        assert_eq!(pos.redeemable, Some(true));
        assert!(pos.is_redeemable());
    }

    #[test]
    fn test_position_response_deserializes_camel_case_fields() {
        let raw = serde_json::json!({
            "assetId": "token-1",
            "tokenId": "tok-yes",
            "conditionId": "0xabc123",
            "outcome": "Yes",
            "outcomeIndex": 0,
            "size": 5,
            "avgPrice": 0.42,
            "realizedPnl": 1.5,
            "unrealizedPnl": -0.1,
            "curPrice": 1,
            "isRedeemable": "1",
            "negativeRisk": "true"
        });

        let pos: PositionResponse =
            serde_json::from_value(raw).expect("position should deserialize");
        assert_eq!(pos.asset_id, "token-1");
        assert_eq!(pos.token_id.as_deref(), Some("tok-yes"));
        assert_eq!(pos.condition_id.as_deref(), Some("0xabc123"));
        assert_eq!(pos.outcome_index.as_deref(), Some("0"));
        assert_eq!(pos.avg_price.as_deref(), Some("0.42"));
        assert_eq!(pos.cur_price.as_deref(), Some("1"));
        assert_eq!(pos.redeemable, Some(true));
        assert_eq!(pos.negative_risk, Some(true));
    }

    #[test]
    fn test_gateway_only_validation_rejects_missing_idempotency() {
        let request =
            OrderRequest::buy_limit("token".to_string(), crate::domain::Side::Up, 10, dec!(0.5));
        let result = PolymarketClient::validate_gateway_order_request_inner(&request, true);
        assert!(result.is_err());
    }

    #[test]
    fn test_gateway_only_validation_rejects_non_intent_client_order_id() {
        let mut request =
            OrderRequest::buy_limit("token".to_string(), crate::domain::Side::Up, 10, dec!(0.5));
        request.idempotency_key = Some("stable-key".to_string());
        let result = PolymarketClient::validate_gateway_order_request_inner(&request, true);
        assert!(result.is_err());
    }

    #[test]
    fn test_gateway_only_validation_accepts_gateway_stamped_order() {
        let mut request =
            OrderRequest::buy_limit("token".to_string(), crate::domain::Side::Up, 10, dec!(0.5));
        request.client_order_id = "intent:abc".to_string();
        request.idempotency_key = Some("stable-key".to_string());
        let result = PolymarketClient::validate_gateway_order_request_inner(&request, true);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_gateway_execution_context_scope_sets_flag() {
        assert!(!PolymarketClient::gateway_execution_context_active());
        let active = PolymarketClient::with_gateway_execution_context(async {
            PolymarketClient::gateway_execution_context_active()
        })
        .await;
        assert!(active);
        assert!(!PolymarketClient::gateway_execution_context_active());
    }

    #[tokio::test]
    async fn test_gateway_execution_context_rejects_direct_live_submit() {
        let result = PolymarketClient::validate_gateway_execution_context(false);
        assert!(result.is_err());

        let scoped = PolymarketClient::with_gateway_execution_context(async {
            PolymarketClient::validate_gateway_execution_context(false)
        })
        .await;
        assert!(scoped.is_ok());
    }
}
