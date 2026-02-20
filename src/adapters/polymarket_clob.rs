//! Polymarket CLOB API client using official SDK
//!
//! This module provides a client that uses the official polymarket-client-sdk
//! for both CLOB (trading) and Gamma (market discovery) operations.

use crate::domain::{OrderRequest, OrderSide, OrderStatus, TimeInForce};
use crate::error::{PloyError, Result};
use crate::signing::Wallet;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use chrono::{DateTime, Utc};
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
use polymarket_client_sdk::gamma::types::request::{
    EventByIdRequest, MarketsRequest, SearchRequest, SeriesByIdRequest,
};
use polymarket_client_sdk::gamma::types::response::Event as SdkEvent;
use polymarket_client_sdk::gamma::Client as GammaClient;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, instrument};
use zeroize::Zeroize;

/// Chain ID for Polygon Mainnet
pub const POLYGON_CHAIN_ID: u64 = 137;

/// Gamma API base URL
pub const GAMMA_API_URL: &str = "https://gamma-api.polymarket.com";
const CLOB_TERMINAL_CURSOR: &str = "LTE="; // base64("-1"), used by CLOB pagination

type AuthClobClient = ClobClient<Authenticated<Normal>>;

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
    /// Legacy wallet for backward compatibility
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

fn parse_json_array_strings(input: &str) -> std::result::Result<Vec<String>, serde_json::Error> {
    let s = input.trim();
    if s.is_empty() || s == "null" {
        return Ok(Vec::new());
    }

    // Common case: JSON array of strings.
    if let Ok(v) = serde_json::from_str::<Vec<String>>(s) {
        return Ok(v);
    }

    // Fallback: JSON array of numbers/values.
    let vals = serde_json::from_str::<Vec<serde_json::Value>>(s)?;
    Ok(vals
        .into_iter()
        .map(|v| match v {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        })
        .collect())
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

#[derive(Debug, Deserialize, Serialize)]
pub struct ApiKeyResponse {
    #[serde(rename = "apiKey")]
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
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

// ==================== Gamma API Types ====================

#[derive(Debug, Deserialize, Serialize)]
pub struct GammaSeriesResponse {
    pub id: String,
    pub ticker: Option<String>,
    pub slug: Option<String>,
    pub title: Option<String>,
    pub recurrence: Option<String>,
    #[serde(default)]
    pub events: Vec<GammaEventInfo>,
    pub volume: Option<f64>,
    pub liquidity: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GammaEventInfo {
    pub id: String,
    pub slug: Option<String>,
    pub title: Option<String>,
    #[serde(rename = "endDate")]
    pub end_date: Option<String>,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub markets: Vec<GammaMarketInfo>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GammaMarketInfo {
    #[serde(rename = "conditionId")]
    pub condition_id: Option<String>,
    pub question: Option<String>,
    #[serde(default)]
    pub tokens: Option<Vec<GammaTokenInfo>>,
    #[serde(rename = "groupItemTitle")]
    pub group_item_title: Option<String>,
    #[serde(rename = "clobTokenIds")]
    pub clob_token_ids: Option<String>,
    #[serde(rename = "outcomePrices")]
    pub outcome_prices: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GammaTokenInfo {
    pub token_id: String,
    pub outcome: String,
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

    fn allow_legacy_direct_submit() -> bool {
        Self::env_bool(&[
            "PLOY_ALLOW_LEGACY_DIRECT_SUBMIT",
            "PLOY_ALLOW_DIRECT_SUBMIT",
        ])
    }

    fn gateway_execution_context_active() -> bool {
        GATEWAY_EXECUTION_CONTEXT.try_with(|v| *v).unwrap_or(false)
    }

    fn validate_gateway_execution_context(dry_run: bool) -> Result<()> {
        if dry_run || Self::allow_legacy_direct_submit() {
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

    async fn fetch_orders_paginated(
        &self,
        auth_client: &AuthClobClient,
        req: &OrdersRequest,
        limit: Option<usize>,
    ) -> Result<Vec<polymarket_client_sdk::clob::types::response::OpenOrderResponse>> {
        let mut cursor: Option<String> = None;
        let mut out = Vec::new();

        loop {
            let page = auth_client
                .orders(req, cursor.clone())
                .await
                .map_err(|e| PloyError::Internal(format!("Failed to get orders: {}", e)))?;

            for order in page.data {
                out.push(order);
                if let Some(max) = limit {
                    if out.len() >= max {
                        return Ok(out);
                    }
                }
            }

            if page.next_cursor == CLOB_TERMINAL_CURSOR {
                break;
            }
            cursor = Some(page.next_cursor);
        }

        Ok(out)
    }

    async fn fetch_trades_paginated(
        &self,
        auth_client: &AuthClobClient,
        req: &TradesRequest,
        limit: Option<usize>,
    ) -> Result<Vec<polymarket_client_sdk::clob::types::response::TradeResponse>> {
        let mut cursor: Option<String> = None;
        let mut out = Vec::new();

        loop {
            let page = auth_client
                .trades(req, cursor.clone())
                .await
                .map_err(|e| PloyError::Internal(format!("Failed to get trades: {}", e)))?;

            for trade in page.data {
                out.push(trade);
                if let Some(max) = limit {
                    if out.len() >= max {
                        return Ok(out);
                    }
                }
            }

            if page.next_cursor == CLOB_TERMINAL_CURSOR {
                break;
            }
            cursor = Some(page.next_cursor);
        }

        Ok(out)
    }

    async fn authenticate_fresh(&self, signer: &PrivateKeySigner) -> Result<AuthClobClient> {
        // Serialize auth handshakes. The upstream SDK requires unique ownership when
        // transitioning unauthenticated -> authenticated, so we create a fresh client per call.
        let _guard = self.order_mutex.lock().await;

        let fresh_client = ClobClient::new(&self.base_url, ClobConfig::default())
            .map_err(|e| PloyError::Internal(format!("Failed to create CLOB client: {}", e)))?;

        let auth_client = if let Some(funder) = self.funder {
            debug!("Using proxy wallet authentication, funder: {:?}", funder);
            fresh_client
                .authentication_builder(signer)
                .funder(funder)
                .signature_type(SdkSignatureType::Proxy)
                .authenticate()
                .await
                .map_err(|e| PloyError::Auth(format!("Proxy authentication failed: {}", e)))?
        } else {
            debug!("Using EOA wallet authentication");
            fresh_client
                .authentication_builder(signer)
                .authenticate()
                .await
                .map_err(|e| PloyError::Auth(format!("Authentication failed: {}", e)))?
        };

        Ok(auth_client)
    }

    /// Create a new CLOB client (dry run mode)
    pub fn new(base_url: &str, dry_run: bool) -> Result<Self> {
        let config = ClobConfig::default();
        let clob_client = ClobClient::new(base_url, config)
            .map_err(|e| PloyError::Internal(format!("Failed to create CLOB client: {}", e)))?;

        let gamma_client = GammaClient::new(GAMMA_API_URL)
            .map_err(|e| PloyError::Internal(format!("Failed to create Gamma client: {}", e)))?;

        info!(
            "Created Polymarket SDK client (read-only, dry_run={})",
            dry_run
        );

        Ok(Self {
            clob_client,
            gamma_client,
            signer: None,
            wallet: None,
            funder: None,
            base_url: base_url.trim_end_matches('/').to_string(),
            dry_run,
            neg_risk: false,
            order_mutex: Arc::new(Mutex::new(())),
        })
    }

    /// Create an authenticated CLOB client with wallet
    /// For proxy wallets (Magic/email), use new_authenticated_proxy instead
    pub async fn new_authenticated(base_url: &str, wallet: Wallet, neg_risk: bool) -> Result<Self> {
        let config = ClobConfig::default();
        let clob_client = ClobClient::new(base_url, config)
            .map_err(|e| PloyError::Internal(format!("Failed to create CLOB client: {}", e)))?;

        let gamma_client = GammaClient::new(GAMMA_API_URL)
            .map_err(|e| PloyError::Internal(format!("Failed to create Gamma client: {}", e)))?;

        // Read private key from environment variable for SDK signer
        // Scope the raw key string so it is zeroized and dropped immediately after parsing
        let signer: PrivateKeySigner = {
            let mut private_key_hex = std::env::var("POLYMARKET_PRIVATE_KEY")
                .or_else(|_| std::env::var("PRIVATE_KEY"))
                .map_err(|_| {
                    PloyError::Wallet(
                        "POLYMARKET_PRIVATE_KEY or PRIVATE_KEY environment variable not set"
                            .to_string(),
                    )
                })?;

            let result = private_key_hex
                .trim_start_matches("0x")
                .parse::<PrivateKeySigner>()
                .map_err(|e| PloyError::Wallet(format!("Invalid private key: {}", e)));

            private_key_hex.zeroize();
            result?
        }
        .with_chain_id(Some(POLYGON_CHAIN_ID));

        info!(
            "Created authenticated Polymarket SDK client, address: {:?}",
            signer.address()
        );

        Ok(Self {
            clob_client,
            gamma_client,
            signer: Some(signer),
            wallet: Some(Arc::new(wallet)),
            funder: None,
            base_url: base_url.trim_end_matches('/').to_string(),
            dry_run: false,
            neg_risk,
            order_mutex: Arc::new(Mutex::new(())),
        })
    }

    /// Create an authenticated CLOB client with proxy wallet (Magic/email wallet)
    /// funder_address is the proxy wallet address that holds the funds
    pub async fn new_authenticated_proxy(
        base_url: &str,
        wallet: Wallet,
        funder_address: &str,
        neg_risk: bool,
    ) -> Result<Self> {
        let config = ClobConfig::default();
        let clob_client = ClobClient::new(base_url, config)
            .map_err(|e| PloyError::Internal(format!("Failed to create CLOB client: {}", e)))?;

        let gamma_client = GammaClient::new(GAMMA_API_URL)
            .map_err(|e| PloyError::Internal(format!("Failed to create Gamma client: {}", e)))?;

        // Read private key from environment variable for SDK signer
        // Scope the raw key string so it is zeroized and dropped immediately after parsing
        let signer: PrivateKeySigner = {
            let mut private_key_hex = std::env::var("POLYMARKET_PRIVATE_KEY")
                .or_else(|_| std::env::var("PRIVATE_KEY"))
                .map_err(|_| {
                    PloyError::Wallet(
                        "POLYMARKET_PRIVATE_KEY or PRIVATE_KEY environment variable not set"
                            .to_string(),
                    )
                })?;

            let result = private_key_hex
                .trim_start_matches("0x")
                .parse::<PrivateKeySigner>()
                .map_err(|e| PloyError::Wallet(format!("Invalid private key: {}", e)));

            private_key_hex.zeroize();
            result?
        }
        .with_chain_id(Some(POLYGON_CHAIN_ID));

        // Parse funder address
        let funder: alloy::primitives::Address = funder_address
            .parse()
            .map_err(|e| PloyError::Wallet(format!("Invalid funder address: {}", e)))?;

        info!(
            "Created authenticated Polymarket SDK client (proxy mode), signer: {:?}, funder: {:?}",
            signer.address(),
            funder
        );

        Ok(Self {
            clob_client,
            gamma_client,
            signer: Some(signer),
            wallet: Some(Arc::new(wallet)),
            funder: Some(funder),
            base_url: base_url.trim_end_matches('/').to_string(),
            dry_run: false,
            neg_risk,
            order_mutex: Arc::new(Mutex::new(())),
        })
    }

    /// Set the funder address for proxy wallets
    pub fn set_funder(&mut self, funder_address: &str) -> Result<()> {
        let funder: alloy::primitives::Address = funder_address
            .parse()
            .map_err(|e| PloyError::Wallet(format!("Invalid funder address: {}", e)))?;
        self.funder = Some(funder);
        info!("Set funder address: {:?}", funder);
        Ok(())
    }

    /// Check if in dry run mode
    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Check if HMAC authentication is configured (for backward compatibility)
    pub fn has_hmac_auth(&self) -> bool {
        self.signer.is_some()
    }

    /// Get base URL
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    // ==================== Gamma API Methods ====================

    /// Get the raw Gamma market (SDK type) by CLOB token id.
    ///
    /// This is useful for official settlement/outcome checks without relying on
    /// undocumented endpoints.
    #[instrument(skip(self))]
    pub async fn get_gamma_market_by_token_id(
        &self,
        token_id: &str,
    ) -> Result<polymarket_client_sdk::gamma::types::response::Market> {
        let req = MarketsRequest::builder()
            .clob_token_ids(vec![token_id.to_string()])
            .limit(1)
            .build();

        let markets = self
            .gamma_client
            .markets(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get market: {}", e)))?;

        markets.into_iter().next().ok_or_else(|| {
            PloyError::MarketDataUnavailable(format!("Market not found for token_id={}", token_id))
        })
    }

    /// Get market by condition ID
    #[instrument(skip(self))]
    pub async fn get_market(&self, condition_id: &str) -> Result<MarketResponse> {
        // Gamma's `market_by_id` is keyed by Gamma market id, not `condition_id`.
        // Use `markets?condition_ids=...` to fetch by condition id.
        let req = MarketsRequest::builder()
            .condition_ids(vec![condition_id.to_string()])
            .limit(1)
            .build();

        let markets = self
            .gamma_client
            .markets(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get market: {}", e)))?;

        let market = markets.into_iter().next().ok_or_else(|| {
            PloyError::MarketDataUnavailable(format!(
                "Market not found for condition_id={}",
                condition_id
            ))
        })?;

        let token_ids: Vec<String> = market
            .clob_token_ids
            .as_deref()
            .and_then(|s| parse_json_array_strings(s).ok())
            .unwrap_or_default();
        let outcomes: Vec<String> = market
            .outcomes
            .as_deref()
            .and_then(|s| parse_json_array_strings(s).ok())
            .unwrap_or_default();
        let prices: Vec<String> = market
            .outcome_prices
            .as_deref()
            .and_then(|s| parse_json_array_strings(s).ok())
            .unwrap_or_default();

        let mut tokens = Vec::new();
        for (i, token_id) in token_ids.iter().enumerate() {
            let outcome = outcomes.get(i).cloned().unwrap_or_default();
            let price = prices.get(i).cloned();
            tokens.push(TokenInfo {
                token_id: token_id.clone(),
                outcome,
                price,
                extra: HashMap::new(),
            });
        }

        Ok(MarketResponse {
            condition_id: market
                .condition_id
                .clone()
                .unwrap_or_else(|| condition_id.to_string()),
            question_id: market.question_id.clone(),
            tokens,
            minimum_order_size: market
                .order_min_size
                .as_ref()
                .map(|d| serde_json::json!(d.to_string())),
            minimum_tick_size: market
                .order_price_min_tick_size
                .as_ref()
                .map(|d| serde_json::json!(d.to_string())),
            active: market.active.unwrap_or(true),
            closed: market.closed.unwrap_or(false),
            end_date_iso: market
                .end_date_iso
                .clone()
                .or_else(|| market.end_date.map(|d| d.to_rfc3339())),
            neg_risk: None,
            extra: HashMap::new(),
        })
    }

    /// Get order book for a token
    #[instrument(skip(self))]
    pub async fn get_order_book(&self, token_id: &str) -> Result<OrderBookResponse> {
        let req = OrderBookSummaryRequest::builder()
            .token_id(token_id)
            .build();

        let resp = self
            .clob_client
            .order_book(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get order book: {}", e)))?;

        Ok(OrderBookResponse {
            market: Some(resp.market),
            asset_id: resp.asset_id,
            bids: resp
                .bids
                .into_iter()
                .map(|l| OrderBookLevel {
                    price: l.price.to_string(),
                    size: l.size.to_string(),
                })
                .collect(),
            asks: resp
                .asks
                .into_iter()
                .map(|l| OrderBookLevel {
                    price: l.price.to_string(),
                    size: l.size.to_string(),
                })
                .collect(),
            timestamp: Some(resp.timestamp.to_rfc3339()),
            hash: resp.hash,
        })
    }

    /// Get best bid/ask prices
    #[instrument(skip(self))]
    pub async fn get_best_prices(
        &self,
        token_id: &str,
    ) -> Result<(Option<Decimal>, Option<Decimal>)> {
        let order_book = self.get_order_book(token_id).await?;

        let best_bid = order_book
            .bids
            .first()
            .and_then(|l| l.price.parse::<Decimal>().ok());
        let best_ask = order_book
            .asks
            .first()
            .and_then(|l| l.price.parse::<Decimal>().ok());

        Ok((best_bid, best_ask))
    }

    /// Search for markets
    #[instrument(skip(self))]
    pub async fn search_markets(&self, query: &str) -> Result<Vec<MarketSummary>> {
        let req = SearchRequest::builder().q(query).build();

        let results = self
            .gamma_client
            .search(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to search markets: {}", e)))?;

        // Extract markets from events in search results
        let mut summaries = Vec::new();
        for event in results.events.unwrap_or_default() {
            if let Some(markets) = event.markets {
                for m in markets {
                    summaries.push(MarketSummary {
                        condition_id: m.condition_id.unwrap_or_default(),
                        question: m.question,
                        slug: m.slug,
                        active: m.active.unwrap_or(true),
                        clob_token_ids: m.clob_token_ids,
                        outcome_prices: m.outcome_prices,
                    });
                }
            }
        }

        Ok(summaries)
    }

    /// Get series by ID
    #[instrument(skip(self))]
    pub async fn get_series(&self, series_id: &str) -> Result<GammaSeriesResponse> {
        let req = SeriesByIdRequest::builder().id(series_id).build();

        let series = self
            .gamma_client
            .series_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get series: {}", e)))?;

        Ok(GammaSeriesResponse {
            id: series.id,
            ticker: series.ticker,
            slug: series.slug,
            title: series.title,
            recurrence: series.recurrence,
            events: vec![], // Events need to be fetched separately
            volume: series.volume.map(|d| d.to_string().parse().unwrap_or(0.0)),
            liquidity: series
                .liquidity
                .map(|d| d.to_string().parse().unwrap_or(0.0)),
        })
    }

    /// Get current (active, not closed) event from a series
    #[instrument(skip(self))]
    pub async fn get_current_event(&self, series_id: &str) -> Result<Option<GammaEventInfo>> {
        // The Gamma `/events` endpoint has historically been inconsistent about including
        // series membership. Prefer the direct `/series/{id}` endpoint and pick the
        // soonest-ending active event in the future.
        let events = self.get_all_active_events(series_id).await?;
        let now = Utc::now();

        let mut best: Option<(DateTime<Utc>, GammaEventInfo)> = None;
        for e in events {
            let Some(end_str) = &e.end_date else {
                continue;
            };
            let Ok(end) = DateTime::parse_from_rfc3339(end_str).map(|dt| dt.with_timezone(&Utc))
            else {
                continue;
            };
            if end <= now {
                continue;
            }

            match best.as_ref() {
                None => best = Some((end, e)),
                Some((best_end, _)) if end < *best_end => best = Some((end, e)),
                _ => {}
            }
        }

        Ok(best.map(|(_, e)| e))
    }

    /// Get event details by ID
    #[instrument(skip(self))]
    pub async fn get_event_details(&self, event_id: &str) -> Result<GammaEventInfo> {
        let req = EventByIdRequest::builder().id(event_id).build();

        let event = self
            .gamma_client
            .event_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get event: {}", e)))?;

        Ok(self.convert_sdk_event(&event))
    }

    /// Get current market tokens from a series
    #[instrument(skip(self))]
    pub async fn get_current_market_tokens(
        &self,
        series_id: &str,
    ) -> Result<Option<(String, MarketResponse)>> {
        let Some(event) = self.get_current_event(series_id).await? else {
            return Ok(None);
        };

        // `/series/{id}` events are lightweight; fetch full event details to access markets.
        let details = self.get_event_details(&event.id).await?;
        let market = match details.markets.first() {
            Some(m) => m,
            None => return Ok(None),
        };

        let Some(condition_id) = &market.condition_id else {
            return Ok(None);
        };

        let market_resp = self.get_market(condition_id).await?;
        Ok(Some((details.id, market_resp)))
    }

    /// Get all active events from a series
    #[instrument(skip(self))]
    pub async fn get_all_active_events(&self, series_id: &str) -> Result<Vec<GammaEventInfo>> {
        let req = SeriesByIdRequest::builder().id(series_id).build();
        let series = self
            .gamma_client
            .series_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to fetch series: {}", e)))?;

        // Filter for active (not closed) events
        let active_events: Vec<GammaEventInfo> = series
            .events
            .unwrap_or_default()
            .into_iter()
            .filter(|e| !e.closed.unwrap_or(false))
            .map(|e| self.convert_sdk_event(&e))
            .collect();

        debug!(
            "Found {} active events in series {}",
            active_events.len(),
            series_id
        );
        Ok(active_events)
    }

    /// Get active sports events matching a keyword
    #[instrument(skip(self))]
    pub async fn get_active_sports_events(&self, keyword: &str) -> Result<Vec<GammaEventInfo>> {
        let req = SearchRequest::builder().q(keyword).build();

        let results = self
            .gamma_client
            .search(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to search: {}", e)))?;

        // Convert events from search results
        Ok(results
            .events
            .unwrap_or_default()
            .into_iter()
            .filter(|e| !e.closed.unwrap_or(false))
            .map(|e| self.convert_sdk_event(&e))
            .collect())
    }

    /// Get all tokens from all active events in a series
    /// Returns (event, up_token_id, down_token_id) for each event
    #[instrument(skip(self))]
    pub async fn get_series_all_tokens(
        &self,
        series_id: &str,
    ) -> Result<Vec<(GammaEventInfo, String, String)>> {
        let events = self.get_all_active_events(series_id).await?;
        let mut result = Vec::new();

        for event in events {
            // Find the first market with clob token IDs
            for market in &event.markets {
                if let Some(clob_ids) = &market.clob_token_ids {
                    // Parse JSON array of token IDs
                    if let Ok(ids) = serde_json::from_str::<Vec<String>>(clob_ids) {
                        if ids.len() >= 2 {
                            // First token is "Yes", second is "No"
                            result.push((event.clone(), ids[0].clone(), ids[1].clone()));
                            break; // Only take first market per event
                        }
                    }
                }
            }
        }

        Ok(result)
    }

    // ==================== Trading Methods ====================

    /// Submit an order
    #[instrument(skip(self))]
    pub async fn submit_order(&self, request: &OrderRequest) -> Result<OrderResponse> {
        Self::validate_gateway_execution_context(self.dry_run)?;
        if !self.dry_run {
            Self::validate_gateway_order_request(request)?;
        }

        if self.dry_run {
            info!(
                "DRY RUN: Would submit {} order for {} shares of {} @ {}",
                request.order_side, request.shares, request.token_id, request.limit_price
            );

            let sdk_order_type = match request.time_in_force {
                TimeInForce::GTC => SdkOrderType::GTC,
                TimeInForce::FOK => SdkOrderType::FOK,
                // Polymarket SDK uses FAK (Fill and Kill) for IOC semantics.
                TimeInForce::IOC => SdkOrderType::FAK,
            };

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
                associate_trades: None,
                created_at: Some(Utc::now().to_rfc3339()),
                expiration: None,
                order_type: Some(sdk_order_type.to_string()),
            });
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.authenticate_fresh(signer).await?;

        // Build the order
        let sdk_side = match request.order_side {
            OrderSide::Buy => SdkSide::Buy,
            OrderSide::Sell => SdkSide::Sell,
        };

        let sdk_order_type = match request.time_in_force {
            TimeInForce::GTC => SdkOrderType::GTC,
            TimeInForce::FOK => SdkOrderType::FOK,
            // Polymarket SDK uses FAK (Fill and Kill) for IOC semantics.
            TimeInForce::IOC => SdkOrderType::FAK,
        };

        let order = auth_client
            .limit_order()
            .token_id(&request.token_id)
            .price(request.limit_price)
            .size(Decimal::from(request.shares))
            .side(sdk_side)
            .order_type(sdk_order_type)
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
        // Mutex guard dropped here, releasing lock for next order

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
            associate_trades: None,
            created_at: Some(Utc::now().to_rfc3339()),
            expiration: None,
            order_type: Some(sdk_order_type.to_string()),
        })
    }

    /// Get order by ID
    #[instrument(skip(self))]
    pub async fn get_order(&self, order_id: &str) -> Result<OrderResponse> {
        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.authenticate_fresh(signer).await?;

        let order = auth_client
            .order(order_id)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get order: {}", e)))?;

        Ok(OrderResponse {
            id: order.id,
            status: format!("{:?}", order.status),
            owner: Some(order.owner.to_string()),
            market: Some(order.market),
            asset_id: Some(order.asset_id),
            side: Some(format!("{:?}", order.side)),
            original_size: Some(order.original_size.to_string()),
            size_matched: Some(order.size_matched.to_string()),
            price: Some(order.price.to_string()),
            associate_trades: None,
            created_at: Some(order.created_at.to_rfc3339()),
            expiration: Some(order.expiration.to_rfc3339()),
            order_type: Some(format!("{:?}", order.order_type)),
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

        let auth_client = self.authenticate_fresh(signer).await?;

        auth_client
            .cancel_order(order_id)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to cancel order: {}", e)))?;

        Ok(true)
    }

    /// Cancel all orders for a token
    #[instrument(skip(self))]
    pub async fn cancel_all_orders(&self, token_id: &str) -> Result<CancelOrderResponse> {
        if self.dry_run {
            info!("DRY RUN: Would cancel all orders for token {}", token_id);
            return Ok(CancelOrderResponse {
                canceled: Some(vec![]),
                not_canceled: None,
            });
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.authenticate_fresh(signer).await?;

        let req = CancelMarketOrderRequest::builder()
            .asset_id(token_id)
            .build();
        let resp = auth_client
            .cancel_market_orders(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to cancel token orders: {}", e)))?;

        let not_canceled = if resp.not_canceled.is_empty() {
            None
        } else {
            Some(
                resp.not_canceled
                    .into_iter()
                    .map(|(order_id, reason)| NotCanceledOrder { order_id, reason })
                    .collect(),
            )
        };

        Ok(CancelOrderResponse {
            canceled: Some(resp.canceled),
            not_canceled,
        })
    }

    // ==================== Account Methods ====================

    /// Get account balance
    #[instrument(skip(self))]
    pub async fn get_balance(&self) -> Result<BalanceResponse> {
        if self.dry_run {
            return Ok(BalanceResponse {
                balance: "100.00".to_string(),
                allowance: None,
            });
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.authenticate_fresh(signer).await?;

        let req = BalanceAllowanceRequest::builder()
            .asset_type(AssetType::Collateral)
            .build();

        let resp = auth_client
            .balance_allowance(req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get balance: {}", e)))?;

        Ok(BalanceResponse {
            balance: resp.balance.to_string(),
            allowance: None,
        })
    }

    /// Get USDC balance
    #[instrument(skip(self))]
    pub async fn get_usdc_balance(&self) -> Result<Decimal> {
        let balance = self.get_balance().await?;
        balance
            .balance
            .parse::<Decimal>()
            .map_err(|e| PloyError::Internal(format!("Failed to parse balance: {}", e)))
    }

    /// Get open orders
    #[instrument(skip(self))]
    pub async fn get_open_orders(&self) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.authenticate_fresh(signer).await?;

        let req = OrdersRequest::builder().build();

        let orders = self
            .fetch_orders_paginated(&auth_client, &req, None)
            .await?;

        // Filter for open orders (LIVE status)
        Ok(orders
            .into_iter()
            .filter(|o| {
                let status = format!("{:?}", o.status);
                status.contains("Live") || status.contains("Open")
            })
            .map(|o| OrderResponse {
                id: o.id.clone(),
                status: format!("{:?}", o.status),
                owner: Some(o.owner.to_string()),
                market: Some(o.market.clone()),
                asset_id: Some(o.asset_id.clone()),
                side: Some(format!("{:?}", o.side)),
                original_size: Some(o.original_size.to_string()),
                size_matched: Some(o.size_matched.to_string()),
                price: Some(o.price.to_string()),
                associate_trades: None,
                created_at: Some(o.created_at.to_rfc3339()),
                expiration: Some(o.expiration.to_rfc3339()),
                order_type: Some(format!("{:?}", o.order_type)),
            })
            .collect())
    }

    /// Get orders for a specific token
    #[instrument(skip(self))]
    pub async fn get_orders_for_token(&self, token_id: &str) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.authenticate_fresh(signer).await?;

        let req = OrdersRequest::builder().asset_id(token_id).build();

        let orders = self
            .fetch_orders_paginated(&auth_client, &req, None)
            .await?;

        Ok(orders
            .into_iter()
            .map(|o| OrderResponse {
                id: o.id.clone(),
                status: format!("{:?}", o.status),
                owner: Some(o.owner.to_string()),
                market: Some(o.market.clone()),
                asset_id: Some(o.asset_id.clone()),
                side: Some(format!("{:?}", o.side)),
                original_size: Some(o.original_size.to_string()),
                size_matched: Some(o.size_matched.to_string()),
                price: Some(o.price.to_string()),
                associate_trades: None,
                created_at: Some(o.created_at.to_rfc3339()),
                expiration: Some(o.expiration.to_rfc3339()),
                order_type: Some(format!("{:?}", o.order_type)),
            })
            .collect())
    }

    /// Get order history
    #[instrument(skip(self))]
    pub async fn get_order_history(&self, limit: Option<u32>) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.authenticate_fresh(signer).await?;

        let req = OrdersRequest::builder().build();
        let orders_data = self
            .fetch_orders_paginated(&auth_client, &req, limit.map(|v| v as usize))
            .await?;

        Ok(orders_data
            .into_iter()
            .map(|o| OrderResponse {
                id: o.id.clone(),
                status: format!("{:?}", o.status),
                owner: Some(o.owner.to_string()),
                market: Some(o.market.clone()),
                asset_id: Some(o.asset_id.clone()),
                side: Some(format!("{:?}", o.side)),
                original_size: Some(o.original_size.to_string()),
                size_matched: Some(o.size_matched.to_string()),
                price: Some(o.price.to_string()),
                associate_trades: None,
                created_at: Some(o.created_at.to_rfc3339()),
                expiration: Some(o.expiration.to_rfc3339()),
                order_type: Some(format!("{:?}", o.order_type)),
            })
            .collect())
    }

    /// Get positions (via Polymarket Data API)
    #[instrument(skip(self))]
    pub async fn get_positions(&self) -> Result<Vec<PositionResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let user = if let Some(funder) = self.funder {
            format!("{:#x}", funder)
        } else if let Some(w) = self.wallet.as_ref() {
            format!("{:#x}", w.address())
        } else if let Some(signer) = self.signer.as_ref() {
            format!("{:#x}", signer.address())
        } else {
            return Err(PloyError::Auth("Not authenticated".to_string()));
        };

        let data_client = DataClient::default();
        let user_addr: polymarket_client_sdk::types::Address = user
            .parse()
            .map_err(|e| PloyError::Internal(format!("Invalid user address {}: {}", user, e)))?;

        let mut positions = Vec::new();
        let mut offset: i32 = 0;
        let page_size: i32 = 500;

        loop {
            let req_builder = PositionsRequest::builder().user(user_addr);
            let req_builder = req_builder
                .limit(page_size)
                .map_err(|e| PloyError::Internal(format!("Invalid positions limit: {}", e)))?;
            let req_builder = req_builder
                .offset(offset)
                .map_err(|e| PloyError::Internal(format!("Invalid positions offset: {}", e)))?;
            let req = req_builder.build();

            let batch = data_client
                .positions(&req)
                .await
                .map_err(|e| PloyError::Internal(format!("Failed to fetch positions: {}", e)))?;

            if batch.is_empty() {
                break;
            }

            let batch_len = batch.len() as i32;
            positions.extend(batch.into_iter().map(|p| {
                let mut extra = HashMap::new();
                extra.insert("title".to_string(), serde_json::json!(p.title));
                extra.insert("slug".to_string(), serde_json::json!(p.slug));
                extra.insert("eventSlug".to_string(), serde_json::json!(p.event_slug));
                extra.insert(
                    "oppositeOutcome".to_string(),
                    serde_json::json!(p.opposite_outcome),
                );
                extra.insert(
                    "oppositeAsset".to_string(),
                    serde_json::json!(p.opposite_asset),
                );
                extra.insert("endDate".to_string(), serde_json::json!(p.end_date));
                extra.insert("mergeable".to_string(), serde_json::json!(p.mergeable));

                PositionResponse {
                    asset_id: p.asset.clone(),
                    token_id: Some(p.asset),
                    condition_id: Some(p.condition_id),
                    outcome: Some(p.outcome),
                    outcome_index: Some(p.outcome_index.to_string()),
                    size: p.size.to_string(),
                    avg_price: Some(p.avg_price.to_string()),
                    realized_pnl: Some(p.realized_pnl.to_string()),
                    unrealized_pnl: Some(p.cash_pnl.to_string()),
                    cur_price: Some(p.cur_price.to_string()),
                    redeemable: Some(p.redeemable),
                    negative_risk: Some(p.negative_risk),
                    extra,
                }
            }));

            if batch_len < page_size || offset >= 10_000 {
                break;
            }
            offset += batch_len;
        }

        Ok(positions)
    }

    /// Get trades
    #[instrument(skip(self))]
    pub async fn get_trades(&self, limit: Option<u32>) -> Result<Vec<TradeResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.authenticate_fresh(signer).await?;

        let req = TradesRequest::builder().build();

        let trades = self
            .fetch_trades_paginated(&auth_client, &req, limit.map(|v| v as usize))
            .await?;

        Ok(trades
            .into_iter()
            .map(|t| TradeResponse {
                id: Some(t.id.clone()),
                order_id: Some(t.taker_order_id.clone()),
                asset_id: t.asset_id.clone(),
                side: format!("{:?}", t.side),
                price: t.price.to_string(),
                size: t.size.to_string(),
                fee: Some(t.fee_rate_bps.to_string()),
                timestamp: Some(t.match_time.to_rfc3339()),
                extra: HashMap::new(),
            })
            .collect())
    }

    /// Get comprehensive account summary
    #[instrument(skip(self))]
    pub async fn get_account_summary(&self) -> Result<AccountSummary> {
        let usdc_balance = self.get_usdc_balance().await.unwrap_or(Decimal::ZERO);
        let open_orders = self.get_open_orders().await.unwrap_or_default();
        let positions = self.get_positions().await.unwrap_or_default();

        let open_order_value = open_orders
            .iter()
            .filter_map(|o| {
                let price = o.price.as_ref()?.parse::<Decimal>().ok()?;
                let size = o.original_size.as_ref()?.parse::<Decimal>().ok()?;
                Some(price * size)
            })
            .sum();

        let position_value = positions.iter().filter_map(|p| p.market_value()).sum();

        let total_equity = usdc_balance + position_value;

        Ok(AccountSummary {
            usdc_balance,
            open_order_count: open_orders.len(),
            open_order_value,
            position_count: positions.len(),
            position_value,
            total_equity,
            open_orders,
            positions,
        })
    }

    // ==================== Helper Methods ====================

    /// Convert SDK Event to our GammaEventInfo
    fn convert_sdk_event(&self, event: &SdkEvent) -> GammaEventInfo {
        GammaEventInfo {
            id: event.id.clone(),
            slug: event.slug.clone(),
            title: event.title.clone(),
            end_date: event.end_date.map(|d| d.to_rfc3339()),
            closed: event.closed.unwrap_or(false),
            markets: event
                .markets
                .as_ref()
                .map(|markets| {
                    markets
                        .iter()
                        .map(|m| GammaMarketInfo {
                            condition_id: m.condition_id.clone(),
                            question: m.question.clone(),
                            tokens: None,
                            group_item_title: m.group_item_title.clone(),
                            clob_token_ids: m.clob_token_ids.clone(),
                            outcome_prices: m.outcome_prices.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Parse order status from string
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
    async fn test_gateway_execution_context_rejects_legacy_direct_live_submit() {
        if PolymarketClient::allow_legacy_direct_submit() {
            return;
        }

        let result = PolymarketClient::validate_gateway_execution_context(false);
        assert!(result.is_err());

        let scoped = PolymarketClient::with_gateway_execution_context(async {
            PolymarketClient::validate_gateway_execution_context(false)
        })
        .await;
        assert!(scoped.is_ok());
    }
}
