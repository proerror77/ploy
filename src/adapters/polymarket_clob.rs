//! Polymarket CLOB API client using official SDK
//!
//! This module provides a client that uses the official polymarket-client-sdk
//! for both CLOB (trading) and Gamma (market discovery) operations.

use crate::domain::{OrderRequest, OrderSide, OrderStatus};
use crate::error::{PloyError, Result};
use crate::signing::Wallet;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use chrono::Utc;
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk::clob::types::{
    request::{
        OrderBookSummaryRequest, BalanceAllowanceRequest, OrdersRequest, TradesRequest,
    },
    AssetType, Side as SdkSide, OrderType as SdkOrderType, SignatureType as SdkSignatureType,
};
use polymarket_client_sdk::gamma::{Client as GammaClient};
use polymarket_client_sdk::gamma::types::request::{
    EventsRequest, EventByIdRequest, MarketByIdRequest,
    SeriesByIdRequest, SearchRequest,
};
use polymarket_client_sdk::gamma::types::response::Event as SdkEvent;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, instrument, warn};

/// Chain ID for Polygon Mainnet
pub const POLYGON_CHAIN_ID: u64 = 137;

/// Gamma API base URL
pub const GAMMA_API_URL: &str = "https://gamma-api.polymarket.com";

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
#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
pub struct OrderBookResponse {
    pub market: Option<String>,
    pub asset_id: String,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub timestamp: Option<String>,
    pub hash: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OrderBookLevel {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Deserialize)]
pub struct CreateOrderResponse {
    pub success: Option<bool>,
    pub error_msg: Option<String>,
    #[serde(rename = "orderID")]
    pub order_id: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CancelOrderResponse {
    pub canceled: Option<Vec<String>>,
    pub not_canceled: Option<Vec<NotCanceledOrder>>,
}

#[derive(Debug, Deserialize)]
pub struct NotCanceledOrder {
    pub order_id: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct MarketsSearchResponse {
    pub data: Option<Vec<MarketSummary>>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MarketSummary {
    pub condition_id: String,
    pub question: Option<String>,
    pub slug: Option<String>,
    pub active: bool,
}

#[derive(Debug, Deserialize)]
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
    #[serde(default)]
    pub asset_id: String,
    #[serde(default, alias = "token_id")]
    pub token_id: Option<String>,
    #[serde(default)]
    pub condition_id: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub avg_price: Option<String>,
    #[serde(default)]
    pub realized_pnl: Option<String>,
    #[serde(default)]
    pub unrealized_pnl: Option<String>,
    #[serde(default)]
    pub cur_price: Option<String>,
    #[serde(default)]
    pub redeemable: Option<bool>,
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
pub struct GammaTokenInfo {
    pub token_id: String,
    pub outcome: String,
}

// ==================== Implementation ====================

impl PolymarketClient {
    /// Create a new CLOB client (dry run mode)
    pub fn new(base_url: &str, dry_run: bool) -> Result<Self> {
        let config = ClobConfig::default();
        let clob_client = ClobClient::new(base_url, config)
            .map_err(|e| PloyError::Internal(format!("Failed to create CLOB client: {}", e)))?;

        let gamma_client = GammaClient::new(GAMMA_API_URL)
            .map_err(|e| PloyError::Internal(format!("Failed to create Gamma client: {}", e)))?;

        info!("Created Polymarket SDK client (read-only, dry_run={})", dry_run);

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
    pub async fn new_authenticated(
        base_url: &str,
        wallet: Wallet,
        neg_risk: bool,
    ) -> Result<Self> {
        let config = ClobConfig::default();
        let clob_client = ClobClient::new(base_url, config)
            .map_err(|e| PloyError::Internal(format!("Failed to create CLOB client: {}", e)))?;

        let gamma_client = GammaClient::new(GAMMA_API_URL)
            .map_err(|e| PloyError::Internal(format!("Failed to create Gamma client: {}", e)))?;

        // Convert wallet private key to alloy signer with Polygon chain ID
        let private_key_hex = wallet.private_key_hex();
        let signer: PrivateKeySigner = private_key_hex
            .parse::<PrivateKeySigner>()
            .map_err(|e| PloyError::Wallet(format!("Invalid private key: {}", e)))?
            .with_chain_id(Some(POLYGON_CHAIN_ID));

        info!("Created authenticated Polymarket SDK client, address: {:?}", signer.address());

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

        // Convert wallet private key to alloy signer with Polygon chain ID
        let private_key_hex = wallet.private_key_hex();
        let signer: PrivateKeySigner = private_key_hex
            .parse::<PrivateKeySigner>()
            .map_err(|e| PloyError::Wallet(format!("Invalid private key: {}", e)))?
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

    /// Get market by condition ID
    #[instrument(skip(self))]
    pub async fn get_market(&self, condition_id: &str) -> Result<MarketResponse> {
        let req = MarketByIdRequest::builder()
            .id(condition_id)
            .build();

        let market = self.gamma_client
            .market_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get market: {}", e)))?;

        // Convert SDK Market to our MarketResponse
        Ok(MarketResponse {
            condition_id: market.condition_id.unwrap_or_else(|| condition_id.to_string()),
            question_id: None,
            tokens: vec![],  // Tokens need to be fetched separately from CLOB
            minimum_order_size: None,
            minimum_tick_size: None,
            active: market.active.unwrap_or(true),
            closed: false,
            end_date_iso: market.end_date.map(|d| d.to_rfc3339()),
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

        let resp = self.clob_client
            .order_book(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get order book: {}", e)))?;

        Ok(OrderBookResponse {
            market: Some(resp.market),
            asset_id: resp.asset_id,
            bids: resp.bids.into_iter().map(|l| OrderBookLevel {
                price: l.price.to_string(),
                size: l.size.to_string(),
            }).collect(),
            asks: resp.asks.into_iter().map(|l| OrderBookLevel {
                price: l.price.to_string(),
                size: l.size.to_string(),
            }).collect(),
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

        let best_bid = order_book.bids.first()
            .and_then(|l| l.price.parse::<Decimal>().ok());
        let best_ask = order_book.asks.first()
            .and_then(|l| l.price.parse::<Decimal>().ok());

        Ok((best_bid, best_ask))
    }

    /// Search for markets
    #[instrument(skip(self))]
    pub async fn search_markets(&self, query: &str) -> Result<Vec<MarketSummary>> {
        let req = SearchRequest::builder()
            .q(query)
            .build();

        let results = self.gamma_client
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
                    });
                }
            }
        }

        Ok(summaries)
    }

    /// Get series by ID
    #[instrument(skip(self))]
    pub async fn get_series(&self, series_id: &str) -> Result<GammaSeriesResponse> {
        let req = SeriesByIdRequest::builder()
            .id(series_id)
            .build();

        let series = self.gamma_client
            .series_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get series: {}", e)))?;

        Ok(GammaSeriesResponse {
            id: series.id,
            ticker: series.ticker,
            slug: series.slug,
            title: series.title,
            recurrence: series.recurrence,
            events: vec![],  // Events need to be fetched separately
            volume: series.volume.map(|d| d.to_string().parse().unwrap_or(0.0)),
            liquidity: series.liquidity.map(|d| d.to_string().parse().unwrap_or(0.0)),
        })
    }

    /// Get current (active, not closed) event from a series
    #[instrument(skip(self))]
    pub async fn get_current_event(&self, series_id: &str) -> Result<Option<GammaEventInfo>> {
        // Fetch active events and filter by series
        let req = EventsRequest::builder()
            .active(true)
            .closed(false)
            .build();

        let events = self.gamma_client
            .events(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get events: {}", e)))?;

        // Find the first event that belongs to this series
        let event = events.into_iter()
            .filter(|e| !e.closed.unwrap_or(false))
            .find(|e| {
                e.series.as_ref().map_or(false, |series| {
                    series.iter().any(|s| s.id == series_id)
                })
            });

        match event {
            Some(e) => Ok(Some(self.convert_sdk_event(&e))),
            None => Ok(None),
        }
    }

    /// Get event details by ID
    #[instrument(skip(self))]
    pub async fn get_event_details(&self, event_id: &str) -> Result<GammaEventInfo> {
        let req = EventByIdRequest::builder()
            .id(event_id)
            .build();

        let event = self.gamma_client
            .event_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get event: {}", e)))?;

        Ok(self.convert_sdk_event(&event))
    }

    /// Get current market tokens from a series
    #[instrument(skip(self))]
    pub async fn get_current_market_tokens(&self, series_id: &str) -> Result<Option<(String, MarketResponse)>> {
        let event = self.get_current_event(series_id).await?;

        match event {
            Some(e) if !e.markets.is_empty() => {
                let market = &e.markets[0];
                if let Some(condition_id) = &market.condition_id {
                    let market_resp = self.get_market(condition_id).await?;
                    return Ok(Some((e.id, market_resp)));
                }
            }
            _ => {}
        }

        Ok(None)
    }

    /// Get all active events from a series
    #[instrument(skip(self))]
    pub async fn get_all_active_events(&self, series_id: &str) -> Result<Vec<GammaEventInfo>> {
        // Use direct HTTP call to /series/{id} which returns events array
        let url = format!("{}/series/{}", GAMMA_API_URL, series_id);

        let client = reqwest::Client::new();
        let response = client.get(&url)
            .send()
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to fetch series: {}", e)))?;

        if !response.status().is_success() {
            return Err(PloyError::Internal(format!(
                "Gamma API error: {} for series {}",
                response.status(),
                series_id
            )));
        }

        let series: GammaSeriesResponse = response.json()
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to parse series response: {}", e)))?;

        // Filter for active (not closed) events
        let active_events: Vec<GammaEventInfo> = series.events
            .into_iter()
            .filter(|e| !e.closed)
            .collect();

        debug!("Found {} active events in series {}", active_events.len(), series_id);
        Ok(active_events)
    }

    /// Get active sports events matching a keyword
    #[instrument(skip(self))]
    pub async fn get_active_sports_events(&self, keyword: &str) -> Result<Vec<GammaEventInfo>> {
        let req = SearchRequest::builder()
            .q(keyword)
            .build();

        let results = self.gamma_client
            .search(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to search: {}", e)))?;

        // Convert events from search results
        Ok(results.events.unwrap_or_default()
            .into_iter()
            .filter(|e| !e.closed.unwrap_or(false))
            .map(|e| self.convert_sdk_event(&e))
            .collect())
    }

    /// Get all tokens from all active events in a series
    /// Returns (event, up_token_id, down_token_id) for each event
    #[instrument(skip(self))]
    pub async fn get_series_all_tokens(&self, series_id: &str)
        -> Result<Vec<(GammaEventInfo, String, String)>>
    {
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
                            result.push((
                                event.clone(),
                                ids[0].clone(),
                                ids[1].clone(),
                            ));
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
                associate_trades: None,
                created_at: Some(Utc::now().to_rfc3339()),
                expiration: None,
                order_type: Some("GTC".to_string()),
            });
        }

        let signer = self.signer.as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        // Acquire mutex to serialize order submissions and prevent auth race condition
        let _guard = self.order_mutex.lock().await;
        debug!("Acquired order mutex for submission");

        // Create a fresh ClobClient for each order to avoid SDK reference issues
        let fresh_client = ClobClient::new(&self.base_url, ClobConfig::default())
            .map_err(|e| PloyError::Internal(format!("Failed to create CLOB client: {}", e)))?;

        // Authenticate for this operation
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

        // Build the order
        let sdk_side = match request.order_side {
            OrderSide::Buy => SdkSide::Buy,
            OrderSide::Sell => SdkSide::Sell,
        };

        let order = auth_client
            .limit_order()
            .token_id(&request.token_id)
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
            order_type: Some("GTC".to_string()),
        })
    }

    /// Get order by ID
    #[instrument(skip(self))]
    pub async fn get_order(&self, order_id: &str) -> Result<OrderResponse> {
        let signer = self.signer.as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.clob_client
            .clone()
            .authentication_builder(signer)
            .authenticate()
            .await
            .map_err(|e| PloyError::Auth(format!("Authentication failed: {}", e)))?;

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

        let signer = self.signer.as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.clob_client
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

    /// Cancel all orders for a token
    #[instrument(skip(self))]
    pub async fn cancel_all_orders(&self, _token_id: &str) -> Result<CancelOrderResponse> {
        if self.dry_run {
            info!("DRY RUN: Would cancel all orders");
            return Ok(CancelOrderResponse {
                canceled: Some(vec![]),
                not_canceled: None,
            });
        }

        let signer = self.signer.as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.clob_client
            .clone()
            .authentication_builder(signer)
            .authenticate()
            .await
            .map_err(|e| PloyError::Auth(format!("Authentication failed: {}", e)))?;

        auth_client
            .cancel_all_orders()
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to cancel all orders: {}", e)))?;

        Ok(CancelOrderResponse {
            canceled: Some(vec![]),
            not_canceled: None,
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

        let signer = self.signer.as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.clob_client
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

        Ok(BalanceResponse {
            balance: resp.balance.to_string(),
            allowance: None,
        })
    }

    /// Get USDC balance
    #[instrument(skip(self))]
    pub async fn get_usdc_balance(&self) -> Result<Decimal> {
        let balance = self.get_balance().await?;
        balance.balance.parse::<Decimal>()
            .map_err(|e| PloyError::Internal(format!("Failed to parse balance: {}", e)))
    }

    /// Get open orders
    #[instrument(skip(self))]
    pub async fn get_open_orders(&self) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let signer = self.signer.as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.clob_client
            .clone()
            .authentication_builder(signer)
            .authenticate()
            .await
            .map_err(|e| PloyError::Auth(format!("Authentication failed: {}", e)))?;

        let req = OrdersRequest::builder().build();

        let orders = auth_client
            .orders(&req, None)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get open orders: {}", e)))?;

        // Filter for open orders (LIVE status)
        Ok(orders.data.into_iter()
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
            }).collect())
    }

    /// Get orders for a specific token
    #[instrument(skip(self))]
    pub async fn get_orders_for_token(&self, token_id: &str) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let signer = self.signer.as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.clob_client
            .clone()
            .authentication_builder(signer)
            .authenticate()
            .await
            .map_err(|e| PloyError::Auth(format!("Authentication failed: {}", e)))?;

        let req = OrdersRequest::builder()
            .asset_id(token_id)
            .build();

        let orders = auth_client
            .orders(&req, None)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get orders: {}", e)))?;

        Ok(orders.data.into_iter().map(|o| OrderResponse {
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
        }).collect())
    }

    /// Get order history
    #[instrument(skip(self))]
    pub async fn get_order_history(&self, limit: Option<u32>) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let signer = self.signer.as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.clob_client
            .clone()
            .authentication_builder(signer)
            .authenticate()
            .await
            .map_err(|e| PloyError::Auth(format!("Authentication failed: {}", e)))?;

        let req = OrdersRequest::builder().build();

        let orders = auth_client
            .orders(&req, None)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get order history: {}", e)))?;

        // Apply limit if specified (SDK doesn't support limit parameter)
        let orders_data: Vec<_> = if let Some(l) = limit {
            orders.data.into_iter().take(l as usize).collect()
        } else {
            orders.data
        };

        Ok(orders_data.into_iter().map(|o| OrderResponse {
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
        }).collect())
    }

    /// Get positions (placeholder - SDK may not support this directly)
    #[instrument(skip(self))]
    pub async fn get_positions(&self) -> Result<Vec<PositionResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        // The SDK doesn't have a direct positions endpoint
        // This would need to be derived from trades or use a custom HTTP call
        warn!("get_positions not fully implemented with SDK");
        Ok(vec![])
    }

    /// Get trades
    #[instrument(skip(self))]
    pub async fn get_trades(&self, limit: Option<u32>) -> Result<Vec<TradeResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let signer = self.signer.as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let auth_client = self.clob_client
            .clone()
            .authentication_builder(signer)
            .authenticate()
            .await
            .map_err(|e| PloyError::Auth(format!("Authentication failed: {}", e)))?;

        let req = TradesRequest::builder().build();

        let trades = auth_client
            .trades(&req, None)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get trades: {}", e)))?;

        // Apply limit if specified (SDK doesn't support limit parameter)
        let trades_iter: Box<dyn Iterator<Item = _>> = match limit {
            Some(l) => Box::new(trades.data.into_iter().take(l as usize)),
            None => Box::new(trades.data.into_iter()),
        };

        Ok(trades_iter.map(|t| TradeResponse {
            id: Some(t.id.clone()),
            order_id: Some(t.taker_order_id.clone()),
            asset_id: t.asset_id.clone(),
            side: format!("{:?}", t.side),
            price: t.price.to_string(),
            size: t.size.to_string(),
            fee: Some(t.fee_rate_bps.to_string()),
            timestamp: Some(t.match_time.to_rfc3339()),
            extra: HashMap::new(),
        }).collect())
    }

    /// Get comprehensive account summary
    #[instrument(skip(self))]
    pub async fn get_account_summary(&self) -> Result<AccountSummary> {
        let usdc_balance = self.get_usdc_balance().await.unwrap_or(Decimal::ZERO);
        let open_orders = self.get_open_orders().await.unwrap_or_default();
        let positions = self.get_positions().await.unwrap_or_default();

        let open_order_value = open_orders.iter()
            .filter_map(|o| {
                let price = o.price.as_ref()?.parse::<Decimal>().ok()?;
                let size = o.original_size.as_ref()?.parse::<Decimal>().ok()?;
                Some(price * size)
            })
            .sum();

        let position_value = positions.iter()
            .filter_map(|p| p.market_value())
            .sum();

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
            markets: event.markets.as_ref()
                .map(|markets| markets.iter().map(|m| GammaMarketInfo {
                    condition_id: m.condition_id.clone(),
                    question: m.question.clone(),
                    tokens: None,
                    group_item_title: m.group_item_title.clone(),
                    clob_token_ids: m.clob_token_ids.clone(),
                    outcome_prices: m.outcome_prices.clone(),
                }).collect())
                .unwrap_or_default(),
        }
    }

    /// Parse order status from string
    pub fn parse_order_status(status: &str) -> OrderStatus {
        match status.to_uppercase().as_str() {
            "LIVE" | "OPEN" => OrderStatus::Submitted,
            "MATCHED" | "FILLED" => OrderStatus::Filled,
            "CANCELED" | "CANCELLED" => OrderStatus::Cancelled,
            "DELAYED" | "PENDING" => OrderStatus::Pending,
            _ => OrderStatus::Pending,
        }
    }

    /// Calculate fill amount and average price from an order
    /// Returns (filled_shares, avg_price)
    pub fn calculate_fill(order: &OrderResponse) -> (Decimal, Decimal) {
        let size_matched = order.size_matched.as_ref()
            .and_then(|s| s.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO);
        let price = order.price.as_ref()
            .and_then(|p| p.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO);
        (size_matched, price)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_client() {
        let client = PolymarketClient::new("https://clob.polymarket.com", true).unwrap();
        assert!(client.is_dry_run());
        assert!(!client.has_hmac_auth());
    }

    #[test]
    fn test_parse_order_status() {
        assert!(matches!(PolymarketClient::parse_order_status("LIVE"), OrderStatus::Submitted));
        assert!(matches!(PolymarketClient::parse_order_status("MATCHED"), OrderStatus::Filled));
        assert!(matches!(PolymarketClient::parse_order_status("CANCELED"), OrderStatus::Cancelled));
    }
}
