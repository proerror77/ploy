//! Kalshi REST adapter (native Rust, no external SDK dependency).
//!
//! This client intentionally normalizes Kalshi payloads into the existing
//! Polymarket-shaped response structs so strategy/execution code can be reused.

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use reqwest::{Client, Method};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use tracing::warn;

use super::{
    polymarket_clob::{OrderBookLevel, OrderBookResponse, TokenInfo},
    BalanceResponse, MarketResponse, MarketSummary, OrderResponse, PositionResponse, TradeResponse,
};
use crate::domain::{OrderRequest, OrderSide, OrderStatus};
use crate::error::{PloyError, Result};
use crate::exchange::{ExchangeClient, ExchangeKind};

const DEFAULT_KALSHI_API_BASE: &str = "https://api.elections.kalshi.com/trade-api/v2";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutcomeSide {
    Yes,
    No,
}

impl OutcomeSide {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
        }
    }

    fn from_token_id(token_id: &str) -> (String, Self) {
        if let Some((ticker, side)) = token_id.rsplit_once(':') {
            return (
                ticker.trim().to_string(),
                if side.trim().eq_ignore_ascii_case("no") {
                    Self::No
                } else {
                    Self::Yes
                },
            );
        }

        if let Some(stripped) = token_id.strip_suffix("-YES") {
            return (stripped.to_string(), Self::Yes);
        }
        if let Some(stripped) = token_id.strip_suffix("-NO") {
            return (stripped.to_string(), Self::No);
        }
        if let Some(stripped) = token_id.strip_suffix("_YES") {
            return (stripped.to_string(), Self::Yes);
        }
        if let Some(stripped) = token_id.strip_suffix("_NO") {
            return (stripped.to_string(), Self::No);
        }

        (token_id.trim().to_string(), Self::Yes)
    }
}

#[derive(Clone)]
pub struct KalshiClient {
    http: Client,
    base_url: String,
    api_key: Option<String>,
    api_secret: Option<String>,
    dry_run: bool,
}

impl KalshiClient {
    pub fn new(
        base_url: Option<&str>,
        api_key: Option<String>,
        api_secret: Option<String>,
        dry_run: bool,
    ) -> Result<Self> {
        let base_url = base_url
            .unwrap_or(DEFAULT_KALSHI_API_BASE)
            .trim_end_matches('/')
            .to_string();

        let http = Client::builder()
            .user_agent("ploy-kalshi-adapter/0.1")
            .build()
            .map_err(|e| {
                PloyError::Internal(format!("failed to build Kalshi HTTP client: {}", e))
            })?;

        Ok(Self {
            http,
            base_url,
            api_key,
            api_secret,
            dry_run,
        })
    }

    pub fn from_env(base_url: Option<&str>, dry_run: bool) -> Result<Self> {
        let api_key = std::env::var("KALSHI_API_KEY")
            .ok()
            .or_else(|| std::env::var("KALSHI_ACCESS_KEY").ok());
        let api_secret = std::env::var("KALSHI_API_SECRET")
            .ok()
            .or_else(|| std::env::var("KALSHI_ACCESS_SECRET").ok());

        Self::new(base_url, api_key, api_secret, dry_run)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_headers(&self, method: &Method, path: &str, body: &str) -> Result<HeaderMap> {
        let key = self.api_key.as_ref().ok_or_else(|| {
            PloyError::Auth("KALSHI_API_KEY (or KALSHI_ACCESS_KEY) is required".to_string())
        })?;
        let secret = self.api_secret.as_ref().ok_or_else(|| {
            PloyError::Auth("KALSHI_API_SECRET (or KALSHI_ACCESS_SECRET) is required".to_string())
        })?;

        let timestamp = Utc::now().timestamp_millis().to_string();
        let sign_payload = format!(
            "{}{}{}{}",
            timestamp,
            method.as_str().to_uppercase(),
            path,
            body
        );

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .map_err(|e| PloyError::Auth(format!("invalid Kalshi secret: {}", e)))?;
        mac.update(sign_payload.as_bytes());
        let signature = BASE64_STANDARD.encode(mac.finalize().into_bytes());

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("kalshi-access-key"),
            HeaderValue::from_str(key)
                .map_err(|e| PloyError::Auth(format!("invalid Kalshi API key header: {}", e)))?,
        );
        headers.insert(
            HeaderName::from_static("kalshi-access-signature"),
            HeaderValue::from_str(&signature)
                .map_err(|e| PloyError::Auth(format!("invalid Kalshi signature header: {}", e)))?,
        );
        headers.insert(
            HeaderName::from_static("kalshi-access-timestamp"),
            HeaderValue::from_str(&timestamp)
                .map_err(|e| PloyError::Auth(format!("invalid Kalshi timestamp header: {}", e)))?,
        );

        Ok(headers)
    }

    async fn request_json(
        &self,
        method: Method,
        path: &str,
        query: Option<&[(&str, String)]>,
        body: Option<Value>,
        require_auth: bool,
    ) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let body_text = body
            .as_ref()
            .map(|b| b.to_string())
            .unwrap_or_else(String::new);

        let mut req = self.http.request(method.clone(), &url);

        if let Some(query) = query {
            req = req.query(query);
        }

        if require_auth {
            let headers = self.auth_headers(&method, path, &body_text)?;
            req = req.headers(headers);
        }

        if let Some(body) = body {
            req = req.header(CONTENT_TYPE, "application/json").json(&body);
        }

        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;

        if status.as_u16() == 429 {
            return Err(PloyError::RateLimited(format!(
                "Kalshi API rate limited for {} {}",
                method, path
            )));
        }

        if !status.is_success() {
            return Err(PloyError::Internal(format!(
                "Kalshi API {} {} failed: status={} body={}",
                method, path, status, text
            )));
        }

        if text.trim().is_empty() {
            return Ok(Value::Null);
        }

        serde_json::from_str(&text)
            .map_err(|e| PloyError::Internal(format!("invalid Kalshi JSON response: {}", e)))
    }

    fn pick_array<'a>(root: &'a Value, keys: &[&str]) -> Option<&'a [Value]> {
        keys.iter()
            .find_map(|key| root.get(*key).and_then(|v| v.as_array()).map(Vec::as_slice))
    }

    fn pick_obj<'a>(root: &'a Value, keys: &[&str]) -> Option<&'a Value> {
        keys.iter().find_map(|key| root.get(*key))
    }

    fn pick_str<'a>(root: &'a Value, keys: &[&str]) -> Option<&'a str> {
        Self::pick_obj(root, keys).and_then(|v| v.as_str())
    }

    fn pick_bool(root: &Value, keys: &[&str]) -> Option<bool> {
        Self::pick_obj(root, keys).and_then(|v| {
            if let Some(b) = v.as_bool() {
                Some(b)
            } else {
                v.as_str()
                    .map(|s| matches!(s, "true" | "TRUE" | "1" | "yes" | "YES"))
            }
        })
    }

    fn parse_decimalish(value: &Value) -> Option<Decimal> {
        match value {
            Value::Null => None,
            Value::String(s) => Decimal::from_str_exact(s.trim()).ok(),
            Value::Number(n) => Decimal::from_str_exact(&n.to_string()).ok(),
            _ => None,
        }
    }

    fn format_price(value: Decimal) -> String {
        value.round_dp(6).normalize().to_string()
    }

    fn from_cents_if_needed(value: Decimal) -> Decimal {
        if value > Decimal::ONE && value <= Decimal::new(100, 0) {
            value / Decimal::new(100, 0)
        } else {
            value
        }
    }

    fn extract_book_levels(value: &Value) -> Vec<OrderBookLevel> {
        let mut out = Vec::new();

        let Some(entries) = value.as_array() else {
            return out;
        };

        for entry in entries {
            match entry {
                Value::Array(pair) if pair.len() >= 2 => {
                    let Some(price) =
                        Self::parse_decimalish(&pair[0]).map(Self::from_cents_if_needed)
                    else {
                        continue;
                    };
                    let Some(size) = Self::parse_decimalish(&pair[1]) else {
                        continue;
                    };
                    out.push(OrderBookLevel {
                        price: Self::format_price(price),
                        size: size.normalize().to_string(),
                    });
                }
                Value::Object(_) => {
                    let price = Self::pick_obj(entry, &["price", "yes_price", "no_price"])
                        .and_then(Self::parse_decimalish)
                        .map(Self::from_cents_if_needed);
                    let size = Self::pick_obj(entry, &["size", "count", "quantity"])
                        .and_then(Self::parse_decimalish);

                    if let (Some(price), Some(size)) = (price, size) {
                        out.push(OrderBookLevel {
                            price: Self::format_price(price),
                            size: size.normalize().to_string(),
                        });
                    }
                }
                _ => {}
            }
        }

        out
    }

    fn map_market_summary(value: &Value) -> MarketSummary {
        let ticker = Self::pick_str(value, &["ticker", "market_ticker", "id"])
            .unwrap_or_default()
            .to_string();
        let question =
            Self::pick_str(value, &["title", "question", "market_title"]).map(ToString::to_string);
        let slug =
            Self::pick_str(value, &["slug", "ticker", "market_ticker"]).map(ToString::to_string);

        let yes_ask = Self::pick_obj(value, &["yes_ask", "ask_yes", "yesAsk"])
            .and_then(Self::parse_decimalish)
            .map(Self::from_cents_if_needed)
            .map(Self::format_price);
        let no_ask = Self::pick_obj(value, &["no_ask", "ask_no", "noAsk"])
            .and_then(Self::parse_decimalish)
            .map(Self::from_cents_if_needed)
            .map(Self::format_price);

        let token_ids = vec![format!("{}:yes", ticker), format!("{}:no", ticker)];
        let outcome_prices = vec![yes_ask.unwrap_or_default(), no_ask.unwrap_or_default()];

        MarketSummary {
            condition_id: ticker,
            question,
            slug,
            active: !Self::pick_bool(value, &["closed", "is_closed"]).unwrap_or(false),
            clob_token_ids: Some(
                serde_json::to_string(&token_ids).unwrap_or_else(|_| "[]".to_string()),
            ),
            outcome_prices: Some(
                serde_json::to_string(&outcome_prices).unwrap_or_else(|_| "[]".to_string()),
            ),
        }
    }

    fn map_market_response(value: &Value) -> MarketResponse {
        let summary = Self::map_market_summary(value);
        let token_ids: Vec<String> = summary
            .clob_token_ids
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_else(|| {
                vec![
                    format!("{}:yes", summary.condition_id),
                    format!("{}:no", summary.condition_id),
                ]
            });
        let prices: Vec<String> = summary
            .outcome_prices
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_else(|| vec![String::new(), String::new()]);

        let mut yes_extra = HashMap::new();
        yes_extra.insert("exchange".to_string(), Value::String("kalshi".to_string()));
        let no_extra = yes_extra.clone();

        let yes_token = TokenInfo {
            token_id: token_ids
                .first()
                .cloned()
                .unwrap_or_else(|| format!("{}:yes", summary.condition_id)),
            outcome: "YES".to_string(),
            price: prices.first().cloned().filter(|v| !v.is_empty()),
            extra: yes_extra,
        };
        let no_token = TokenInfo {
            token_id: token_ids
                .get(1)
                .cloned()
                .unwrap_or_else(|| format!("{}:no", summary.condition_id)),
            outcome: "NO".to_string(),
            price: prices.get(1).cloned().filter(|v| !v.is_empty()),
            extra: no_extra,
        };

        MarketResponse {
            condition_id: summary.condition_id,
            question_id: summary.slug,
            tokens: vec![yes_token, no_token],
            minimum_order_size: None,
            minimum_tick_size: None,
            active: summary.active,
            closed: !summary.active,
            end_date_iso: Self::pick_str(value, &["close_time", "expiration_time", "end_time"])
                .map(ToString::to_string),
            neg_risk: Some(false),
            extra: HashMap::new(),
        }
    }

    fn map_order_response(order: &Value, fallback_id: Option<&str>) -> OrderResponse {
        let id = Self::pick_str(order, &["order_id", "id", "client_order_id"])
            .map(ToString::to_string)
            .or_else(|| fallback_id.map(ToString::to_string))
            .unwrap_or_else(|| format!("kalshi-{}", Utc::now().timestamp_millis()));

        let status = Self::pick_str(order, &["status", "state"])
            .unwrap_or("resting")
            .to_uppercase();

        let side = Self::pick_str(order, &["side", "action"])
            .map(|s| s.to_uppercase())
            .or_else(|| Some("BUY".to_string()));

        let price = Self::pick_obj(order, &["price", "limit_price", "yes_price", "no_price"])
            .and_then(Self::parse_decimalish)
            .map(Self::from_cents_if_needed)
            .map(Self::format_price);

        let size = Self::pick_obj(order, &["count", "size", "quantity"])
            .and_then(Self::parse_decimalish)
            .map(|d| d.normalize().to_string());

        let filled = Self::pick_obj(order, &["filled_count", "filled", "size_matched"])
            .and_then(Self::parse_decimalish)
            .map(|d| d.normalize().to_string());

        OrderResponse {
            id,
            status,
            owner: None,
            market: Self::pick_str(order, &["ticker", "market_ticker"]).map(ToString::to_string),
            asset_id: Self::pick_str(order, &["ticker", "market_ticker"])
                .map(|t| format!("{}:yes", t)),
            side,
            original_size: size,
            size_matched: filled,
            price,
            associate_trades: None,
            created_at: Self::pick_str(order, &["created_time", "created_at"])
                .map(ToString::to_string),
            expiration: Self::pick_str(order, &["expiration_time", "expiration"])
                .map(ToString::to_string),
            order_type: Self::pick_str(order, &["type", "order_type"]).map(ToString::to_string),
        }
    }

    async fn fetch_orderbook(&self, ticker: &str) -> Result<OrderBookResponse> {
        let path = format!("/markets/{}/orderbook", ticker);
        let value = match self
            .request_json(Method::GET, &path, None, None, false)
            .await
        {
            Ok(value) => value,
            Err(_) => {
                self.request_json(
                    Method::GET,
                    &format!("/markets/{}", ticker),
                    None,
                    None,
                    false,
                )
                .await?
            }
        };

        let root = Self::pick_obj(&value, &["orderbook", "book"]).unwrap_or(&value);
        let asks = Self::pick_obj(root, &["asks", "sell"]).map(Self::extract_book_levels);
        let bids = Self::pick_obj(root, &["bids", "buy"]).map(Self::extract_book_levels);

        // Kalshi binary books often expose YES/NO ladders separately.
        let yes = Self::pick_obj(root, &["yes", "yes_orders"]).map(Self::extract_book_levels);
        let no = Self::pick_obj(root, &["no", "no_orders"]).map(Self::extract_book_levels);

        let mut resolved_bids = bids.unwrap_or_default();
        let mut resolved_asks = asks.unwrap_or_default();
        if resolved_bids.is_empty() {
            resolved_bids = yes.unwrap_or_default();
        }
        if resolved_asks.is_empty() {
            resolved_asks = no.unwrap_or_default();
        }

        Ok(OrderBookResponse {
            market: Some(ticker.to_string()),
            asset_id: format!("{}:yes", ticker),
            bids: resolved_bids,
            asks: resolved_asks,
            timestamp: Some(Utc::now().to_rfc3339()),
            hash: None,
        })
    }

    pub async fn get_order_book(&self, token_id: &str) -> Result<OrderBookResponse> {
        let (ticker, _) = OutcomeSide::from_token_id(token_id);
        self.fetch_orderbook(&ticker).await
    }

    pub async fn get_market(&self, ticker: &str) -> Result<MarketResponse> {
        let path = format!("/markets/{}", ticker);
        let value = self
            .request_json(Method::GET, &path, None, None, false)
            .await?;
        let market = Self::pick_obj(&value, &["market", "data"]).unwrap_or(&value);
        Ok(Self::map_market_response(market))
    }

    pub async fn search_markets(&self, query: &str) -> Result<Vec<MarketSummary>> {
        let params = vec![("status", "open".to_string()), ("limit", "200".to_string())];
        let value = self
            .request_json(Method::GET, "/markets", Some(&params), None, false)
            .await?;

        let mut out = Vec::new();
        if let Some(markets) = Self::pick_array(&value, &["markets", "data", "results"]) {
            for market in markets {
                let mapped = Self::map_market_summary(market);
                if query.trim().is_empty()
                    || mapped
                        .question
                        .as_deref()
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                        .contains(&query.to_ascii_lowercase())
                    || mapped
                        .slug
                        .as_deref()
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                        .contains(&query.to_ascii_lowercase())
                    || mapped
                        .condition_id
                        .to_ascii_lowercase()
                        .contains(&query.to_ascii_lowercase())
                {
                    out.push(mapped);
                }
            }
        }

        Ok(out)
    }

    pub async fn submit_order(&self, request: &OrderRequest) -> Result<OrderResponse> {
        if self.dry_run {
            return Ok(OrderResponse {
                id: request.client_order_id.clone(),
                status: "FILLED".to_string(),
                owner: None,
                market: None,
                asset_id: Some(request.token_id.clone()),
                side: Some(request.order_side.to_string()),
                original_size: Some(request.shares.to_string()),
                size_matched: Some(request.shares.to_string()),
                price: Some(request.limit_price.normalize().to_string()),
                associate_trades: None,
                created_at: Some(Utc::now().to_rfc3339()),
                expiration: None,
                order_type: Some(format!("{:?}", request.time_in_force)),
            });
        }

        let (ticker, side) = OutcomeSide::from_token_id(&request.token_id);
        let body = json!({
            "ticker": ticker,
            "client_order_id": request.client_order_id,
            "action": if matches!(request.order_side, OrderSide::Buy) { "buy" } else { "sell" },
            "side": side.as_str(),
            "type": "limit",
            "count": request.shares,
            "price": (request.limit_price * Decimal::new(100, 0)).round_dp(0).to_u64().unwrap_or(0),
            "time_in_force": format!("{:?}", request.time_in_force).to_lowercase(),
        });

        let value = self
            .request_json(Method::POST, "/portfolio/orders", None, Some(body), true)
            .await?;
        let order = Self::pick_obj(&value, &["order", "data", "result"]).unwrap_or(&value);
        Ok(Self::map_order_response(
            order,
            Some(&request.client_order_id),
        ))
    }

    pub async fn get_order(&self, order_id: &str) -> Result<OrderResponse> {
        let path = format!("/portfolio/orders/{}", order_id);
        let value = self
            .request_json(Method::GET, &path, None, None, true)
            .await?;
        let order = Self::pick_obj(&value, &["order", "data", "result"]).unwrap_or(&value);
        Ok(Self::map_order_response(order, Some(order_id)))
    }

    pub async fn cancel_order(&self, order_id: &str) -> Result<bool> {
        if self.dry_run {
            return Ok(true);
        }

        let path = format!("/portfolio/orders/{}/cancel", order_id);
        match self
            .request_json(Method::POST, &path, None, Some(json!({})), true)
            .await
        {
            Ok(_) => Ok(true),
            Err(first_err) => {
                let delete_path = format!("/portfolio/orders/{}", order_id);
                self.request_json(Method::DELETE, &delete_path, None, None, true)
                    .await
                    .map(|_| true)
                    .map_err(|_| first_err)
            }
        }
    }

    pub async fn get_balance(&self) -> Result<BalanceResponse> {
        if self.dry_run {
            return Ok(BalanceResponse {
                balance: "1000".to_string(),
                allowance: None,
            });
        }

        let value = self
            .request_json(Method::GET, "/portfolio/balance", None, None, true)
            .await?;
        let root = Self::pick_obj(&value, &["balance", "data"]).unwrap_or(&value);

        let bal = Self::pick_obj(root, &["balance", "available_balance", "cash"])
            .and_then(Self::parse_decimalish)
            .unwrap_or(Decimal::ZERO)
            .normalize()
            .to_string();

        Ok(BalanceResponse {
            balance: bal,
            allowance: None,
        })
    }

    pub async fn get_positions(&self) -> Result<Vec<PositionResponse>> {
        if self.dry_run {
            return Ok(Vec::new());
        }

        let value = self
            .request_json(Method::GET, "/portfolio/positions", None, None, true)
            .await?;
        let positions = Self::pick_array(&value, &["positions", "data", "results"]).unwrap_or(&[]);

        let mut out = Vec::new();
        for pos in positions {
            let ticker = Self::pick_str(pos, &["ticker", "market_ticker", "id"])
                .unwrap_or_default()
                .to_string();
            let side = Self::pick_str(pos, &["side", "outcome"])
                .unwrap_or("yes")
                .to_ascii_uppercase();
            let size = Self::pick_obj(pos, &["count", "size", "quantity"])
                .and_then(Self::parse_decimalish)
                .unwrap_or(Decimal::ZERO)
                .normalize()
                .to_string();

            out.push(PositionResponse {
                asset_id: format!("{}:{}", ticker, side.to_ascii_lowercase()),
                token_id: Some(format!("{}:{}", ticker, side.to_ascii_lowercase())),
                condition_id: Some(ticker.clone()),
                outcome: Some(side),
                outcome_index: None,
                size,
                avg_price: Self::pick_obj(pos, &["avg_price", "average_price"])
                    .and_then(Self::parse_decimalish)
                    .map(Self::from_cents_if_needed)
                    .map(Self::format_price),
                realized_pnl: Self::pick_obj(pos, &["realized_pnl", "pnl_realized"])
                    .and_then(Self::parse_decimalish)
                    .map(|d| d.normalize().to_string()),
                unrealized_pnl: Self::pick_obj(pos, &["unrealized_pnl", "pnl_unrealized"])
                    .and_then(Self::parse_decimalish)
                    .map(|d| d.normalize().to_string()),
                cur_price: Self::pick_obj(pos, &["mark_price", "price"])
                    .and_then(Self::parse_decimalish)
                    .map(Self::from_cents_if_needed)
                    .map(Self::format_price),
                redeemable: None,
                negative_risk: Some(false),
                extra: HashMap::new(),
            });
        }

        Ok(out)
    }

    pub async fn get_order_history(&self, limit: Option<u32>) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            return Ok(Vec::new());
        }

        let params = vec![
            ("limit", limit.unwrap_or(100).to_string()),
            ("status", "all".to_string()),
        ];
        let value = self
            .request_json(Method::GET, "/portfolio/orders", Some(&params), None, true)
            .await?;

        let orders = Self::pick_array(&value, &["orders", "data", "results"]).unwrap_or(&[]);
        Ok(orders
            .iter()
            .map(|order| Self::map_order_response(order, None))
            .collect())
    }

    pub async fn get_trades(&self, limit: Option<u32>) -> Result<Vec<TradeResponse>> {
        if self.dry_run {
            return Ok(Vec::new());
        }

        let params = vec![("limit", limit.unwrap_or(100).to_string())];
        let value = self
            .request_json(Method::GET, "/portfolio/fills", Some(&params), None, true)
            .await?;

        let fills = Self::pick_array(&value, &["fills", "data", "results"]).unwrap_or(&[]);
        let mut out = Vec::new();
        for fill in fills {
            out.push(TradeResponse {
                id: Self::pick_str(fill, &["fill_id", "id"]).map(ToString::to_string),
                order_id: Self::pick_str(fill, &["order_id"]).map(ToString::to_string),
                asset_id: Self::pick_str(fill, &["ticker", "market_ticker"])
                    .map(|t| format!("{}:yes", t))
                    .unwrap_or_default(),
                side: Self::pick_str(fill, &["side", "action"])
                    .unwrap_or_default()
                    .to_string(),
                price: Self::pick_obj(fill, &["price", "yes_price", "no_price"])
                    .and_then(Self::parse_decimalish)
                    .map(Self::from_cents_if_needed)
                    .map(Self::format_price)
                    .unwrap_or_else(|| "0".to_string()),
                size: Self::pick_obj(fill, &["count", "size", "quantity"])
                    .and_then(Self::parse_decimalish)
                    .map(|d| d.normalize().to_string())
                    .unwrap_or_else(|| "0".to_string()),
                fee: Self::pick_obj(fill, &["fee"])
                    .and_then(Self::parse_decimalish)
                    .map(|d| d.normalize().to_string()),
                timestamp: Self::pick_str(fill, &["created_time", "timestamp"])
                    .map(ToString::to_string),
                extra: HashMap::new(),
            });
        }

        Ok(out)
    }

    pub async fn get_best_prices(
        &self,
        token_id: &str,
    ) -> Result<(Option<Decimal>, Option<Decimal>)> {
        let (ticker, side) = OutcomeSide::from_token_id(token_id);
        let book = self.fetch_orderbook(&ticker).await?;

        if book.bids.is_empty() && book.asks.is_empty() {
            warn!(token_id, "Kalshi order book has no bids/asks");
            return Ok((None, None));
        }

        let mut bid = book
            .bids
            .first()
            .and_then(|l| Decimal::from_str_exact(l.price.trim()).ok());
        let mut ask = book
            .asks
            .first()
            .and_then(|l| Decimal::from_str_exact(l.price.trim()).ok());

        // If token requests NO side, invert from YES price when possible.
        if side == OutcomeSide::No {
            bid = bid.map(|v| (Decimal::ONE - v).max(Decimal::ZERO));
            ask = ask.map(|v| (Decimal::ONE - v).max(Decimal::ZERO));
        }

        Ok((bid, ask))
    }
}

#[async_trait]
impl ExchangeClient for KalshiClient {
    fn kind(&self) -> ExchangeKind {
        ExchangeKind::Kalshi
    }

    fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    async fn submit_order_gateway(&self, request: &OrderRequest) -> Result<OrderResponse> {
        KalshiClient::submit_order(self, request).await
    }

    async fn get_order(&self, order_id: &str) -> Result<OrderResponse> {
        KalshiClient::get_order(self, order_id).await
    }

    async fn cancel_order(&self, order_id: &str) -> Result<bool> {
        KalshiClient::cancel_order(self, order_id).await
    }

    async fn get_best_prices(&self, token_id: &str) -> Result<(Option<Decimal>, Option<Decimal>)> {
        KalshiClient::get_best_prices(self, token_id).await
    }

    fn infer_order_status(&self, order: &OrderResponse) -> OrderStatus {
        match order.status.trim().to_ascii_lowercase().as_str() {
            "filled" | "executed" => OrderStatus::Filled,
            "partially_filled" | "partial_fill" => OrderStatus::PartiallyFilled,
            "cancelled" | "canceled" => OrderStatus::Cancelled,
            "rejected" => OrderStatus::Rejected,
            "expired" => OrderStatus::Expired,
            "open" | "resting" | "active" | "pending" => OrderStatus::Submitted,
            _ => OrderStatus::Submitted,
        }
    }

    fn calculate_fill(&self, order: &OrderResponse) -> (u64, Option<Decimal>) {
        let filled = order
            .size_matched
            .as_deref()
            .and_then(|v| Decimal::from_str_exact(v).ok())
            .and_then(|v| v.round_dp(0).to_u64())
            .unwrap_or(0);

        let avg_price = order
            .price
            .as_deref()
            .and_then(|v| Decimal::from_str_exact(v).ok())
            .map(Self::from_cents_if_needed);

        (filled, avg_price)
    }

    async fn get_market(&self, market_id: &str) -> Result<MarketResponse> {
        KalshiClient::get_market(self, market_id).await
    }

    async fn search_markets(&self, query: &str) -> Result<Vec<MarketSummary>> {
        KalshiClient::search_markets(self, query).await
    }

    async fn get_balance(&self) -> Result<BalanceResponse> {
        KalshiClient::get_balance(self).await
    }

    async fn get_positions(&self) -> Result<Vec<PositionResponse>> {
        KalshiClient::get_positions(self).await
    }

    async fn get_order_history(&self, limit: Option<u32>) -> Result<Vec<OrderResponse>> {
        KalshiClient::get_order_history(self, limit).await
    }

    async fn get_trades(&self, limit: Option<u32>) -> Result<Vec<TradeResponse>> {
        KalshiClient::get_trades(self, limit).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_outcome_side_from_token_formats() {
        let (ticker, side) = OutcomeSide::from_token_id("BTC-2026:YES");
        assert_eq!(ticker, "BTC-2026");
        assert_eq!(side, OutcomeSide::Yes);

        let (ticker, side) = OutcomeSide::from_token_id("BTC-2026-NO");
        assert_eq!(ticker, "BTC-2026");
        assert_eq!(side, OutcomeSide::No);
    }

    #[test]
    fn from_cents_is_applied_for_small_integer_prices() {
        let cents = Decimal::new(42, 0);
        assert_eq!(
            KalshiClient::from_cents_if_needed(cents),
            Decimal::new(42, 2)
        );

        let decimal = Decimal::new(42, 2);
        assert_eq!(KalshiClient::from_cents_if_needed(decimal), decimal);
    }
}
