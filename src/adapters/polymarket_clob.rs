use crate::domain::{OrderRequest, OrderSide, OrderStatus, Side};
use crate::error::{PloyError, Result};
use crate::signing::{
    build_clob_auth_signature, build_signed_order, ApiCredentials, HmacAuth, OrderData, Wallet,
};
use chrono::Utc;
use ethers::types::Address;
use reqwest::{Client, StatusCode};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, info, instrument, warn};

/// Convert address to EIP-55 checksummed string format
fn to_checksum_address(addr: Address) -> String {
    ethers::utils::to_checksum(&addr, None)
}

/// Chain ID for Polygon Mainnet
pub const POLYGON_CHAIN_ID: u64 = 137;

/// Polymarket CLOB API client
pub struct PolymarketClient {
    client: Client,
    base_url: String,
    dry_run: bool,
    /// Wallet for signing (None in dry run mode without real trading)
    wallet: Option<Arc<Wallet>>,
    /// HMAC authentication (None until credentials are derived)
    hmac_auth: Option<HmacAuth>,
    /// Nonce counter for orders
    nonce: AtomicU64,
    /// Whether to use negative risk exchange
    neg_risk: bool,
}

impl Clone for PolymarketClient {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            dry_run: self.dry_run,
            wallet: self.wallet.clone(),
            hmac_auth: self.hmac_auth.clone(),
            nonce: AtomicU64::new(self.nonce.load(Ordering::SeqCst)),
            neg_risk: self.neg_risk,
        }
    }
}

// ==================== API Response Types ====================

/// Use serde_json::Value for flexible parsing of API responses
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
    /// Catch all other fields
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct TokenInfo {
    pub token_id: String,
    #[serde(default)]
    pub outcome: String,
    /// Price can be a number or string from API
    #[serde(default, deserialize_with = "deserialize_price")]
    pub price: Option<String>,
    /// Catch all other fields
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Helper to deserialize price that can be number or string
fn deserialize_price<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

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

/// Account balance response
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BalanceResponse {
    pub balance: String,
    #[serde(default)]
    pub allowance: Option<String>,
}

/// Position response from API
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PositionResponse {
    #[serde(default)]
    pub asset_id: String,
    #[serde(default, alias = "token_id")]
    pub token_id: Option<String>,
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
    /// Catch all other fields
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

impl PositionResponse {
    /// Get position value (size * avg_price)
    pub fn value(&self) -> Option<Decimal> {
        let size = self.size.parse::<Decimal>().ok()?;
        let price = self.avg_price.as_ref()?.parse::<Decimal>().ok()?;
        Some(size * price)
    }

    /// Get current market value (size * cur_price)
    pub fn market_value(&self) -> Option<Decimal> {
        let size = self.size.parse::<Decimal>().ok()?;
        let price = self.cur_price.as_ref()?.parse::<Decimal>().ok()?;
        Some(size * price)
    }
}

/// Trade/fill response from API
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
    /// Catch all other fields
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Comprehensive account summary
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
    /// Check if account has sufficient balance for a trade
    pub fn has_sufficient_balance(&self, required: Decimal) -> bool {
        self.usdc_balance >= required
    }

    /// Get available balance (USDC - open order value)
    pub fn available_balance(&self) -> Decimal {
        (self.usdc_balance - self.open_order_value).max(Decimal::ZERO)
    }

    /// Log account summary
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

/// Gamma API base URL
pub const GAMMA_API_URL: &str = "https://gamma-api.polymarket.com";

/// Response from Gamma API series endpoint
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

/// Event info from Gamma API
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

/// Market info from Gamma API event
#[derive(Debug, Clone, Deserialize)]
pub struct GammaMarketInfo {
    #[serde(rename = "conditionId")]
    pub condition_id: Option<String>,
    pub question: Option<String>,
    #[serde(default)]
    pub tokens: Option<Vec<GammaTokenInfo>>,
    /// Group item title (e.g., "↑ 104,000")
    #[serde(rename = "groupItemTitle")]
    pub group_item_title: Option<String>,
    /// CLOB token IDs as JSON array string (e.g., "[\"token1\", \"token2\"]")
    #[serde(rename = "clobTokenIds")]
    pub clob_token_ids: Option<String>,
    /// Outcome prices as JSON array string (e.g., "[\"0.50\", \"0.50\"]")
    #[serde(rename = "outcomePrices")]
    pub outcome_prices: Option<String>,
}

/// Token info from Gamma API
#[derive(Debug, Clone, Deserialize)]
pub struct GammaTokenInfo {
    pub token_id: String,
    pub outcome: String,
}

impl PolymarketClient {
    /// Create a new CLOB client (dry run mode)
    pub fn new(base_url: &str, dry_run: bool) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|e| PloyError::Internal(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            dry_run,
            wallet: None,
            hmac_auth: None,
            nonce: AtomicU64::new(0),
            neg_risk: false,
        })
    }

    /// Create an authenticated CLOB client with wallet
    pub async fn new_authenticated(
        base_url: &str,
        wallet: Wallet,
        neg_risk: bool,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|e| PloyError::Internal(format!("Failed to create HTTP client: {}", e)))?;

        let wallet = Arc::new(wallet);
        let base_url = base_url.trim_end_matches('/').to_string();

        let mut instance = Self {
            client,
            base_url,
            dry_run: false,
            wallet: Some(Arc::clone(&wallet)),
            hmac_auth: None,
            nonce: AtomicU64::new(0),
            neg_risk,
        };

        // Derive API credentials
        instance.derive_api_credentials().await?;

        Ok(instance)
    }

    /// Handle rate limit response with exponential backoff
    /// Returns the number of seconds to wait, or None if not rate limited
    fn parse_rate_limit_retry(&self, status: StatusCode, headers: &reqwest::header::HeaderMap) -> Option<u64> {
        if status == StatusCode::TOO_MANY_REQUESTS {
            // Try to parse Retry-After header
            let retry_after = headers
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(60); // Default to 60 seconds if no header
            Some(retry_after)
        } else {
            None
        }
    }

    /// Execute HTTP request with rate limit handling
    async fn execute_with_rate_limit(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let max_retries = 3;
        let mut attempt = 0;

        loop {
            // Clone the request for retry (RequestBuilder can't be reused)
            let req = request.try_clone().ok_or_else(|| {
                PloyError::Internal("Request cannot be cloned".to_string())
            })?;

            let resp = req.send().await?;
            let status = resp.status();

            if let Some(retry_after) = self.parse_rate_limit_retry(status, resp.headers()) {
                attempt += 1;
                if attempt >= max_retries {
                    return Err(PloyError::RateLimited(format!(
                        "Rate limited after {} attempts, retry after {}s",
                        attempt, retry_after
                    )));
                }

                warn!(
                    "Rate limited (429), waiting {}s before retry (attempt {}/{})",
                    retry_after, attempt, max_retries
                );
                tokio::time::sleep(std::time::Duration::from_secs(retry_after)).await;
                continue;
            }

            return Ok(resp);
        }
    }

    /// Derive API credentials using wallet signature
    async fn derive_api_credentials(&mut self) -> Result<()> {
        let wallet = self
            .wallet
            .as_ref()
            .ok_or_else(|| PloyError::Wallet("Wallet not configured".to_string()))?;

        let timestamp = Utc::now().timestamp();
        let nonce = 0u64;

        // Build EIP-712 signature for auth
        let (auth_message, signature) =
            build_clob_auth_signature(wallet, timestamp, nonce).await?;

        // Call derive API key endpoint
        let url = format!("{}/auth/derive-api-key", self.base_url);

        let body = serde_json::json!({
            "message": auth_message.message,
            "timestamp": auth_message.timestamp,
            "nonce": nonce.to_string(),
            "signature": signature
        });

        let resp = self
            .client
            .get(&url)
            .header("POLY_ADDRESS", to_checksum_address(wallet.address()))
            .header("POLY_SIGNATURE", &signature)
            .header("POLY_TIMESTAMP", timestamp.to_string())
            .header("POLY_NONCE", nonce.to_string())
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            warn!("Derive API key failed ({}): {}, trying create instead", status, text);

            // Try create instead of derive
            return self.create_api_credentials().await;
        }

        let api_key_resp: ApiKeyResponse = resp.json().await?;

        info!("API credentials derived successfully, API key: {}...", &api_key_resp.api_key[..8.min(api_key_resp.api_key.len())]);

        // Set up HMAC auth
        let checksum_addr = to_checksum_address(wallet.address());
        debug!("Setting up HMAC auth with address: {}", checksum_addr);
        debug!("API key prefix: {}...", &api_key_resp.api_key[..8.min(api_key_resp.api_key.len())]);

        let credentials = ApiCredentials::new(
            api_key_resp.api_key,
            api_key_resp.secret,
            api_key_resp.passphrase,
        );

        self.hmac_auth = Some(HmacAuth::new(
            credentials,
            checksum_addr,
        ));

        Ok(())
    }

    /// Create new API credentials
    async fn create_api_credentials(&mut self) -> Result<()> {
        let wallet = self
            .wallet
            .as_ref()
            .ok_or_else(|| PloyError::Wallet("Wallet not configured".to_string()))?;

        let timestamp = Utc::now().timestamp();
        let nonce = 0u64;

        let (_, signature) = build_clob_auth_signature(wallet, timestamp, nonce).await?;

        let url = format!("{}/auth/api-key", self.base_url);

        let resp = self
            .client
            .post(&url)
            .header("POLY_ADDRESS", to_checksum_address(wallet.address()))
            .header("POLY_SIGNATURE", &signature)
            .header("POLY_TIMESTAMP", timestamp.to_string())
            .header("POLY_NONCE", nonce.to_string())
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::Wallet(format!(
                "Failed to create API key: {}",
                text
            )));
        }

        let api_key_resp: ApiKeyResponse = resp.json().await?;

        info!("API credentials created successfully");

        let credentials = ApiCredentials::new(
            api_key_resp.api_key,
            api_key_resp.secret,
            api_key_resp.passphrase,
        );

        self.hmac_auth = Some(HmacAuth::new(
            credentials,
            to_checksum_address(wallet.address()),
        ));

        Ok(())
    }

    /// Get next nonce
    fn next_nonce(&self) -> u64 {
        self.nonce.fetch_add(1, Ordering::SeqCst)
    }

    /// Check if in dry run mode
    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Check if HMAC authentication is configured
    pub fn has_hmac_auth(&self) -> bool {
        self.hmac_auth.is_some()
    }

    /// Get base URL
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    // ==================== Market Data ====================

    #[instrument(skip(self))]
    pub async fn get_market(&self, condition_id: &str) -> Result<MarketResponse> {
        let url = format!("{}/markets/{}", self.base_url, condition_id);
        debug!("Fetching market: {}", url);

        let resp = self.client.get(&url).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::MarketDataUnavailable(format!(
                "HTTP {}: {}",
                status, text
            )));
        }

        let market: MarketResponse = resp.json().await?;
        Ok(market)
    }

    #[instrument(skip(self))]
    pub async fn get_order_book(&self, token_id: &str) -> Result<OrderBookResponse> {
        let url = format!("{}/book", self.base_url);

        let resp = self
            .client
            .get(&url)
            .query(&[("token_id", token_id)])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::MarketDataUnavailable(format!(
                "HTTP {}: {}",
                status, text
            )));
        }

        let book: OrderBookResponse = resp.json().await?;
        Ok(book)
    }

    pub async fn get_best_prices(
        &self,
        token_id: &str,
    ) -> Result<(Option<Decimal>, Option<Decimal>)> {
        let book = self.get_order_book(token_id).await?;

        let best_bid = book
            .bids
            .first()
            .and_then(|p| p.price.parse::<Decimal>().ok());
        let best_ask = book
            .asks
            .first()
            .and_then(|p| p.price.parse::<Decimal>().ok());

        Ok((best_bid, best_ask))
    }

    #[instrument(skip(self))]
    pub async fn search_markets(&self, query: &str) -> Result<Vec<MarketSummary>> {
        let url = format!("{}/markets", self.base_url);

        let resp = self
            .client
            .get(&url)
            .query(&[("slug", query), ("active", "true")])
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::MarketDataUnavailable(text));
        }

        let text = resp.text().await?;

        if let Ok(markets) = serde_json::from_str::<Vec<MarketSummary>>(&text) {
            return Ok(markets);
        }

        if let Ok(resp) = serde_json::from_str::<MarketsSearchResponse>(&text) {
            return Ok(resp.data.unwrap_or_default());
        }

        Err(PloyError::InvalidMarketData(format!(
            "Failed to parse markets response: {}",
            text
        )))
    }

    // ==================== Gamma API Methods ====================

    /// Get series info from Gamma API
    #[instrument(skip(self))]
    pub async fn get_series(&self, series_id: &str) -> Result<GammaSeriesResponse> {
        let url = format!("{}/series/{}", GAMMA_API_URL, series_id);
        debug!("Fetching series: {}", url);

        let resp = self.client.get(&url).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::MarketDataUnavailable(format!(
                "HTTP {}: {}",
                status, text
            )));
        }

        let series: GammaSeriesResponse = resp.json().await?;
        Ok(series)
    }

    /// Get current active event from a series
    /// Uses timestamp-based slug calculation for 15m/4h series
    #[instrument(skip(self))]
    pub async fn get_current_event(&self, series_id: &str) -> Result<Option<GammaEventInfo>> {
        use chrono::Utc;

        // Map series ID to (prefix, interval_seconds)
        let (prefix, interval) = match series_id {
            "10423" => ("sol-updown-15m", 15 * 60),    // 15 minutes
            "10332" => ("eth-updown-4h", 4 * 60 * 60), // 4 hours
            _ => return Ok(None), // Unknown series
        };

        let now = Utc::now().timestamp();

        // Try to find an active event by testing recent and future timestamps
        // Events are created at interval boundaries
        let events_url = format!("{}/events", GAMMA_API_URL);

        // Try past, current and future intervals (-2 to +2)
        for offset in -2..3i64 {
            let target_time = now - (now % interval as i64) + (offset * interval as i64);
            let slug = format!("{}-{}", prefix, target_time);

            debug!("Trying event slug: {}", slug);

            let resp = self.client
                .get(&events_url)
                .query(&[("slug", &slug)])
                .send()
                .await?;

            if resp.status().is_success() {
                let events: Vec<GammaEventInfo> = resp.json().await?;
                // Find an event that is not closed and has end time in the future
                let now_dt = Utc::now();
                if let Some(event) = events.into_iter().find(|e| {
                    !e.closed && e.end_date.as_ref().map_or(false, |end| {
                        chrono::DateTime::parse_from_rfc3339(end)
                            .map_or(false, |dt| dt > now_dt)
                    })
                }) {
                    info!("Found active event: {} ({})", slug, event.id);
                    return Ok(Some(event));
                }
            }
        }

        Ok(None)
    }

    /// Get event details with tokens from Gamma API
    #[instrument(skip(self))]
    pub async fn get_event_details(&self, event_id: &str) -> Result<GammaEventInfo> {
        let url = format!("{}/events/{}", GAMMA_API_URL, event_id);
        debug!("Fetching event: {}", url);

        let resp = self.client.get(&url).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::MarketDataUnavailable(format!(
                "HTTP {}: {}",
                status, text
            )));
        }

        let event: GammaEventInfo = resp.json().await?;
        Ok(event)
    }

    /// Get current market tokens for a series (combines series + event + CLOB lookups)
    #[instrument(skip(self))]
    pub async fn get_current_market_tokens(&self, series_id: &str) -> Result<Option<(String, MarketResponse)>> {
        // Get current event from series
        let event = match self.get_current_event(series_id).await? {
            Some(e) => e,
            None => return Ok(None),
        };

        // Get full event details
        let event_details = self.get_event_details(&event.id).await?;

        // Get the first market's condition ID
        let condition_id = event_details.markets.first()
            .and_then(|m| m.condition_id.clone())
            .ok_or_else(|| PloyError::InvalidMarketData("No condition ID found".to_string()))?;

        // Get market details from CLOB
        let market = self.get_market(&condition_id).await?;

        Ok(Some((event_details.title.unwrap_or_default(), market)))
    }

    // ==================== Order Management ====================

    #[instrument(skip(self, request))]
    pub async fn submit_order(&self, request: &OrderRequest) -> Result<OrderResponse> {
        if self.dry_run {
            info!(
                "DRY RUN: Would submit {} order for {} shares of {} ({}) @ {}",
                request.order_side,
                request.shares,
                request.market_side,
                request.token_id,
                request.limit_price
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
                order_type: Some("LIMIT".to_string()),
            });
        }

        // Get wallet and HMAC auth
        let wallet = self
            .wallet
            .as_ref()
            .ok_or_else(|| PloyError::Wallet("Wallet not configured".to_string()))?;

        let hmac_auth = self
            .hmac_auth
            .as_ref()
            .ok_or_else(|| PloyError::Wallet("API credentials not configured".to_string()))?;

        // Build order data
        let nonce = self.next_nonce();
        let order_data = match request.order_side {
            crate::domain::OrderSide::Buy => OrderData::new_buy(
                wallet.address(),
                wallet.address(),
                &request.token_id,
                request.limit_price,
                request.shares,
                nonce,
            )?,
            crate::domain::OrderSide::Sell => OrderData::new_sell(
                wallet.address(),
                wallet.address(),
                &request.token_id,
                request.limit_price,
                request.shares,
                nonce,
            )?,
        };

        // Sign the order
        let signed_order = build_signed_order(wallet, order_data, self.neg_risk).await?;

        // Build request body
        let body = signed_order.to_json()?;

        // Build auth headers
        let headers = hmac_auth.build_headers("POST", "/order", Some(&body))?;

        // Submit order
        let url = format!("{}/order", self.base_url);

        let resp = self
            .client
            .post(&url)
            .headers(headers)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::OrderSubmission(format!(
                "HTTP {}: {}",
                status, text
            )));
        }

        let create_resp: CreateOrderResponse = resp.json().await?;

        if let Some(error) = create_resp.error_msg {
            return Err(PloyError::OrderRejected(error));
        }

        let order_id = create_resp.order_id.unwrap_or_else(|| request.client_order_id.clone());

        info!("Order submitted: {}", order_id);

        Ok(OrderResponse {
            id: order_id,
            status: create_resp.status.unwrap_or_else(|| "OPEN".to_string()),
            owner: Some(to_checksum_address(wallet.address())),
            market: None,
            asset_id: Some(request.token_id.clone()),
            side: Some(format!("{:?}", request.order_side)),
            original_size: Some(request.shares.to_string()),
            size_matched: Some("0".to_string()),
            price: Some(request.limit_price.to_string()),
            associate_trades: None,
            created_at: Some(Utc::now().to_rfc3339()),
            expiration: None,
            order_type: Some("LIMIT".to_string()),
        })
    }

    #[instrument(skip(self))]
    pub async fn get_order(&self, order_id: &str) -> Result<OrderResponse> {
        if self.dry_run {
            return Ok(OrderResponse {
                id: order_id.to_string(),
                status: "MATCHED".to_string(),
                owner: None,
                market: None,
                asset_id: None,
                side: None,
                original_size: None,
                size_matched: None,
                price: None,
                associate_trades: None,
                created_at: None,
                expiration: None,
                order_type: None,
            });
        }

        let url = format!("{}/order/{}", self.base_url, order_id);

        let mut req = self.client.get(&url);

        // Add auth headers if available
        if let Some(hmac_auth) = &self.hmac_auth {
            let headers = hmac_auth.build_headers("GET", &format!("/order/{}", order_id), None)?;
            req = req.headers(headers);
        }

        let resp = req.send().await?;

        match resp.status() {
            StatusCode::OK => {
                let order: OrderResponse = resp.json().await?;
                Ok(order)
            }
            StatusCode::NOT_FOUND => Err(PloyError::OrderSubmission(format!(
                "Order not found: {}",
                order_id
            ))),
            status => {
                let text = resp.text().await.unwrap_or_default();
                Err(PloyError::OrderSubmission(format!(
                    "HTTP {}: {}",
                    status, text
                )))
            }
        }
    }

    #[instrument(skip(self))]
    pub async fn cancel_order(&self, order_id: &str) -> Result<bool> {
        if self.dry_run {
            info!("DRY RUN: Would cancel order {}", order_id);
            return Ok(true);
        }

        let hmac_auth = self
            .hmac_auth
            .as_ref()
            .ok_or_else(|| PloyError::Wallet("API credentials not configured".to_string()))?;

        let body = serde_json::json!({ "orderID": order_id }).to_string();
        let headers = hmac_auth.build_headers("DELETE", "/order", Some(&body))?;

        let url = format!("{}/order", self.base_url);

        let resp = self
            .client
            .delete(&url)
            .headers(headers)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            warn!("Failed to cancel order {}: {}", order_id, text);
            return Ok(false);
        }

        info!("Order cancelled: {}", order_id);
        Ok(true)
    }

    #[instrument(skip(self))]
    pub async fn cancel_all_orders(&self, token_id: &str) -> Result<CancelOrderResponse> {
        if self.dry_run {
            info!("DRY RUN: Would cancel all orders for token {}", token_id);
            return Ok(CancelOrderResponse {
                canceled: Some(vec![]),
                not_canceled: Some(vec![]),
            });
        }

        let hmac_auth = self
            .hmac_auth
            .as_ref()
            .ok_or_else(|| PloyError::Wallet("API credentials not configured".to_string()))?;

        let body = serde_json::json!({ "asset_id": token_id }).to_string();
        let headers = hmac_auth.build_headers("DELETE", "/orders", Some(&body))?;

        let url = format!("{}/orders", self.base_url);

        let resp = self
            .client
            .delete(&url)
            .headers(headers)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await?;

        let cancel_resp: CancelOrderResponse = resp.json().await?;
        Ok(cancel_resp)
    }

    // ==================== Multi-Event Methods ====================

    /// Get all active (non-closed, future end_date) events from a series
    #[instrument(skip(self))]
    pub async fn get_all_active_events(&self, series_id: &str) -> Result<Vec<GammaEventInfo>> {
        let series = self.get_series(series_id).await?;
        let now = Utc::now();

        let active_events: Vec<GammaEventInfo> = series.events
            .into_iter()
            .filter(|e| {
                !e.closed && e.end_date.as_ref().map_or(false, |end| {
                    chrono::DateTime::parse_from_rfc3339(end)
                        .map_or(false, |dt| dt > now)
                })
            })
            .collect();

        info!("Found {} active events in series {}", active_events.len(), series_id);
        Ok(active_events)
    }

    /// Get active sports events by keyword search
    /// Searches for events containing the keyword in the title
    #[instrument(skip(self))]
    pub async fn get_active_sports_events(&self, keyword: &str) -> Result<Vec<GammaEventInfo>> {
        let url = format!("{}/events", GAMMA_API_URL);
        debug!("Fetching sports events with keyword: {}", keyword);

        let resp = self
            .client
            .get(&url)
            .query(&[
                ("active", "true"),
                ("closed", "false"),
                ("limit", "500"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::MarketDataUnavailable(format!(
                "HTTP {}: {}",
                status, text
            )));
        }

        let events: Vec<GammaEventInfo> = resp.json().await?;
        let now = Utc::now();
        let keyword_lower = keyword.to_lowercase();

        // Filter events by keyword and active status
        let filtered: Vec<GammaEventInfo> = events
            .into_iter()
            .filter(|e| {
                // Must contain keyword in title
                let title_match = e.title.as_ref()
                    .map_or(false, |t| t.to_lowercase().contains(&keyword_lower));

                // Must not be closed and end date must be in future
                let is_active = !e.closed && e.end_date.as_ref().map_or(false, |end| {
                    chrono::DateTime::parse_from_rfc3339(end)
                        .map_or(false, |dt| dt > now)
                });

                title_match && is_active
            })
            .collect();

        info!("Found {} active events matching '{}'", filtered.len(), keyword);
        Ok(filtered)
    }

    /// Get all token pairs from all active events in a series
    /// Returns: Vec<(event_info, up_token_id, down_token_id)>
    #[instrument(skip(self))]
    pub async fn get_series_all_tokens(&self, series_id: &str)
        -> Result<Vec<(GammaEventInfo, String, String)>> {
        let events = self.get_all_active_events(series_id).await?;
        let mut all_tokens = Vec::new();

        for event in events {
            // Get full event details to access markets
            let event_details = match self.get_event_details(&event.id).await {
                Ok(details) => details,
                Err(e) => {
                    warn!("Failed to get event details for {}: {}", event.id, e);
                    continue;
                }
            };

            if let Some(market) = event_details.markets.first() {
                if let Some(cond_id) = &market.condition_id {
                    // Get market from CLOB to get token IDs
                    match self.get_market(cond_id).await {
                        Ok(clob_market) => {
                            let up_token = clob_market.tokens.iter()
                                .find(|t| {
                                    let outcome = t.outcome.to_lowercase();
                                    outcome.contains("up") || outcome == "yes"
                                })
                                .map(|t| t.token_id.clone());

                            let down_token = clob_market.tokens.iter()
                                .find(|t| {
                                    let outcome = t.outcome.to_lowercase();
                                    outcome.contains("down") || outcome == "no"
                                })
                                .map(|t| t.token_id.clone());

                            if let (Some(up), Some(down)) = (up_token, down_token) {
                                debug!(
                                    "Event {} tokens: UP={}, DOWN={}",
                                    event.id,
                                    &up[..20.min(up.len())],
                                    &down[..20.min(down.len())]
                                );
                                all_tokens.push((event_details, up, down));
                            }
                        }
                        Err(e) => {
                            warn!("Failed to get market {}: {}", cond_id, e);
                            continue;
                        }
                    }
                }
            }
        }

        info!("Collected {} token pairs from series {}", all_tokens.len(), series_id);
        Ok(all_tokens)
    }

    // ==================== Account & Balance Methods ====================

    /// Get account balance (USDC available for trading)
    #[instrument(skip(self))]
    pub async fn get_balance(&self) -> Result<BalanceResponse> {
        if self.dry_run {
            info!("DRY RUN: Returning mock balance");
            return Ok(BalanceResponse {
                balance: "10000.00".to_string(),
                allowance: Some("10000.00".to_string()),
            });
        }

        let hmac_auth = self
            .hmac_auth
            .as_ref()
            .ok_or_else(|| PloyError::Wallet("API credentials not configured".to_string()))?;

        // Balance endpoint - use the exact format from py-clob-client
        let path = "/balance-allowance";
        let full_url = format!("{}{}?asset_type=USDC&signature_type=0", self.base_url, path);

        // Sign with path only (not including query params)
        let headers = hmac_auth.build_headers("GET", path, None)?;

        debug!("Balance request URL: {}", full_url);
        debug!("Balance request headers: {:?}", headers);

        let resp = self
            .client
            .get(&full_url)
            .headers(headers)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::Internal(format!(
                "Failed to get balance: {} - {}",
                status, text
            )));
        }

        let balance: BalanceResponse = resp.json().await?;
        debug!("Account balance: {}", balance.balance);
        Ok(balance)
    }

    /// Get USDC balance as Decimal
    pub async fn get_usdc_balance(&self) -> Result<Decimal> {
        let balance = self.get_balance().await?;
        balance
            .balance
            .parse::<Decimal>()
            .map_err(|e| PloyError::Internal(format!("Failed to parse balance: {}", e)))
    }

    // ==================== Open Orders Methods ====================

    /// Get all open orders for the account
    #[instrument(skip(self))]
    pub async fn get_open_orders(&self) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            info!("DRY RUN: Returning empty open orders");
            return Ok(vec![]);
        }

        let hmac_auth = self
            .hmac_auth
            .as_ref()
            .ok_or_else(|| PloyError::Wallet("API credentials not configured".to_string()))?;

        let headers = hmac_auth.build_headers("GET", "/data/orders", None)?;
        let url = format!("{}/data/orders", self.base_url);

        let resp = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::Internal(format!(
                "Failed to get open orders: {} - {}",
                status, text
            )));
        }

        let orders: Vec<OrderResponse> = resp.json().await.unwrap_or_default();
        info!("Found {} open orders", orders.len());
        Ok(orders)
    }

    /// Get open orders for a specific token
    #[instrument(skip(self))]
    pub async fn get_orders_for_token(&self, token_id: &str) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let hmac_auth = self
            .hmac_auth
            .as_ref()
            .ok_or_else(|| PloyError::Wallet("API credentials not configured".to_string()))?;

        let path = format!("/orders?asset_id={}", token_id);
        let headers = hmac_auth.build_headers("GET", &path, None)?;
        let url = format!("{}{}", self.base_url, path);

        let resp = self.client.get(&url).headers(headers).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::Internal(format!(
                "Failed to get orders for token: {} - {}",
                status, text
            )));
        }

        let orders: Vec<OrderResponse> = resp.json().await.unwrap_or_default();
        Ok(orders)
    }

    /// Get order history (filled and cancelled orders)
    #[instrument(skip(self))]
    pub async fn get_order_history(&self, limit: Option<u32>) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let hmac_auth = self
            .hmac_auth
            .as_ref()
            .ok_or_else(|| PloyError::Wallet("API credentials not configured".to_string()))?;

        let limit_param = limit.unwrap_or(100);
        let path = format!("/orders/history?limit={}", limit_param);
        let headers = hmac_auth.build_headers("GET", &path, None)?;
        let url = format!("{}{}", self.base_url, path);

        let resp = self.client.get(&url).headers(headers).send().await?;

        if !resp.status().is_success() {
            // Some APIs don't have history endpoint, return empty
            debug!("Order history endpoint returned error, may not be available");
            return Ok(vec![]);
        }

        let orders: Vec<OrderResponse> = resp.json().await.unwrap_or_default();
        Ok(orders)
    }

    // ==================== Positions Methods ====================

    /// Get all positions for the account
    #[instrument(skip(self))]
    pub async fn get_positions(&self) -> Result<Vec<PositionResponse>> {
        if self.dry_run {
            info!("DRY RUN: Returning empty positions");
            return Ok(vec![]);
        }

        let hmac_auth = self
            .hmac_auth
            .as_ref()
            .ok_or_else(|| PloyError::Wallet("API credentials not configured".to_string()))?;

        // Try the positions endpoint
        let headers = hmac_auth.build_headers("GET", "/positions", None)?;
        let url = format!("{}/positions", self.base_url);

        let resp = self.client.get(&url).headers(headers).send().await?;

        if !resp.status().is_success() {
            // Positions endpoint might not exist, return empty
            debug!("Positions endpoint returned error, may not be available");
            return Ok(vec![]);
        }

        let positions: Vec<PositionResponse> = resp.json().await.unwrap_or_default();
        info!("Found {} positions", positions.len());
        Ok(positions)
    }

    /// Get trades/fills history
    #[instrument(skip(self))]
    pub async fn get_trades(&self, limit: Option<u32>) -> Result<Vec<TradeResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let hmac_auth = self
            .hmac_auth
            .as_ref()
            .ok_or_else(|| PloyError::Wallet("API credentials not configured".to_string()))?;

        let limit_param = limit.unwrap_or(100);
        let path = format!("/data/trades?limit={}", limit_param);
        let headers = hmac_auth.build_headers("GET", &path, None)?;
        let url = format!("{}{}", self.base_url, path);

        let resp = self.client.get(&url).headers(headers).send().await?;

        if !resp.status().is_success() {
            debug!("Trades endpoint returned error, may not be available");
            return Ok(vec![]);
        }

        let trades: Vec<TradeResponse> = resp.json().await.unwrap_or_default();
        Ok(trades)
    }

    // ==================== Account Summary ====================

    /// Get comprehensive account summary
    pub async fn get_account_summary(&self) -> Result<AccountSummary> {
        let balance = self.get_balance().await.ok();
        let open_orders = self.get_open_orders().await.unwrap_or_default();
        let positions = self.get_positions().await.unwrap_or_default();

        let usdc_balance = balance
            .as_ref()
            .and_then(|b| b.balance.parse::<Decimal>().ok())
            .unwrap_or_default();

        let open_order_count = open_orders.len();
        let position_count = positions.len();

        // Calculate total exposure from open orders
        let open_order_value: Decimal = open_orders
            .iter()
            .filter_map(|o| {
                let size = o.original_size.as_ref()?.parse::<Decimal>().ok()?;
                let price = o.price.as_ref()?.parse::<Decimal>().ok()?;
                Some(size * price)
            })
            .sum();

        // Calculate total position value
        let position_value: Decimal = positions
            .iter()
            .filter_map(|p| {
                let size = p.size.parse::<Decimal>().ok()?;
                let price = p.avg_price.as_ref()?.parse::<Decimal>().ok()?;
                Some(size * price)
            })
            .sum();

        Ok(AccountSummary {
            usdc_balance,
            open_order_count,
            open_order_value,
            position_count,
            position_value,
            total_equity: usdc_balance + position_value,
            open_orders,
            positions,
        })
    }

    // ==================== Helper Methods ====================

    pub fn parse_order_status(status: &str) -> OrderStatus {
        match status.to_uppercase().as_str() {
            "OPEN" | "LIVE" => OrderStatus::Submitted,
            "MATCHED" | "FILLED" => OrderStatus::Filled,
            "PARTIALLY_MATCHED" | "PARTIALLY_FILLED" => OrderStatus::PartiallyFilled,
            "CANCELED" | "CANCELLED" => OrderStatus::Cancelled,
            "EXPIRED" => OrderStatus::Expired,
            _ => OrderStatus::Failed,
        }
    }

    pub fn calculate_fill(order: &OrderResponse) -> (u64, Option<Decimal>) {
        let filled = order
            .size_matched
            .as_ref()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        let price = order
            .price
            .as_ref()
            .and_then(|p| p.parse::<Decimal>().ok());

        (filled, price)
    }
}

/// Builder for creating order requests
pub struct OrderBuilder {
    token_id: String,
    market_side: Side,
    order_side: OrderSide,
    shares: u64,
    price: Option<Decimal>,
}

impl OrderBuilder {
    pub fn new(token_id: String, market_side: Side) -> Self {
        Self {
            token_id,
            market_side,
            order_side: OrderSide::Buy,
            shares: 0,
            price: None,
        }
    }

    pub fn buy(mut self, shares: u64) -> Self {
        self.order_side = OrderSide::Buy;
        self.shares = shares;
        self
    }

    pub fn sell(mut self, shares: u64) -> Self {
        self.order_side = OrderSide::Sell;
        self.shares = shares;
        self
    }

    pub fn at_price(mut self, price: Decimal) -> Self {
        self.price = Some(price);
        self
    }

    pub fn build(self) -> Result<OrderRequest> {
        let price = self
            .price
            .ok_or_else(|| PloyError::OrderSubmission("Price is required".to_string()))?;

        Ok(OrderRequest::buy_limit(
            self.token_id,
            self.market_side,
            self.shares,
            price,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_order_builder() {
        let request = OrderBuilder::new("token123".to_string(), Side::Up)
            .buy(100)
            .at_price(dec!(0.45))
            .build()
            .unwrap();

        assert_eq!(request.token_id, "token123");
        assert_eq!(request.market_side, Side::Up);
        assert_eq!(request.order_side, OrderSide::Buy);
        assert_eq!(request.shares, 100);
        assert_eq!(request.limit_price, dec!(0.45));
    }

    #[test]
    fn test_parse_order_status() {
        assert_eq!(
            PolymarketClient::parse_order_status("OPEN"),
            OrderStatus::Submitted
        );
        assert_eq!(
            PolymarketClient::parse_order_status("MATCHED"),
            OrderStatus::Filled
        );
        assert_eq!(
            PolymarketClient::parse_order_status("CANCELED"),
            OrderStatus::Cancelled
        );
    }
}
