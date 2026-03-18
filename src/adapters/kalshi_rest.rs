//! Kalshi REST adapter (native Rust, no external SDK dependency).
//!
//! This client intentionally normalizes Kalshi payloads into the existing
//! Polymarket-shaped response structs so strategy/execution code can be reused.

use async_trait::async_trait;
use chrono::Utc;
use reqwest::{Client, Method};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{json, Value};
use std::collections::HashMap;
use tracing::debug;

mod auth_http;
mod market_data;

use super::{
    polymarket_clob::OrderBookResponse, BalanceResponse, MarketResponse, MarketSummary,
    OrderResponse, PositionResponse, TradeResponse,
};
use crate::domain::{OrderRequest, OrderSide, OrderStatus};
use crate::error::{PloyError, Result};
use crate::exchange::{ExchangeClient, ExchangeKind};
use auth_http::build_http_client;

const DEFAULT_KALSHI_API_BASE: &str = "https://api.elections.kalshi.com/trade-api/v2";

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

        let http = build_http_client()?;

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

    fn serialize_limit_price(limit_price: Decimal) -> Result<(u64, String)> {
        if limit_price <= Decimal::ZERO {
            return Err(PloyError::Validation(format!(
                "limit_price must be > 0 for Kalshi orders, got {}",
                limit_price
            )));
        }

        let scaled = limit_price * Decimal::new(100, 0);
        let rounded = scaled.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero);
        let cents = rounded.to_u64().ok_or_else(|| {
            PloyError::Validation(format!(
                "failed to serialize Kalshi limit_price={} to cents (rounded={})",
                limit_price, rounded
            ))
        })?;

        // Never send a 0-cent order for positive prices; guard tiny values to minimum 1 cent.
        let protected_cents = if cents == 0 { 1 } else { cents };
        Ok((protected_cents, limit_price.normalize().to_string()))
    }

    fn build_submit_order_body(request: &OrderRequest) -> Result<(Value, String, u64)> {
        let (ticker, side) = OutcomeSide::from_token_id(&request.token_id);
        let (price_cents, trace_dollars) = Self::serialize_limit_price(request.limit_price)?;

        Ok((
            json!({
                "ticker": ticker,
                "client_order_id": request.client_order_id,
                "action": if matches!(request.order_side, OrderSide::Buy) { "buy" } else { "sell" },
                "side": side.as_str(),
                "type": "limit",
                "count": request.shares,
                "price": price_cents,
                "time_in_force": format!("{:?}", request.time_in_force).to_lowercase(),
            }),
            trace_dollars,
            price_cents,
        ))
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

        let (body, trace_dollars, price_cents) = Self::build_submit_order_body(request)?;
        debug!(
            client_order_id = %request.client_order_id,
            token_id = %request.token_id,
            limit_price_dollars = %trace_dollars,
            limit_price_cents = price_cents,
            "Submitting Kalshi order with serialized limit price"
        );

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
    use crate::domain::{OrderType, Side, TimeInForce};
    use rust_decimal_macros::dec;

    fn sample_order_request(limit_price: Decimal) -> OrderRequest {
        OrderRequest {
            client_order_id: "cid-123".to_string(),
            idempotency_key: None,
            token_id: "BTC-2026:YES".to_string(),
            market_side: Side::Up,
            order_side: OrderSide::Buy,
            shares: 25,
            limit_price,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
        }
    }

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

    #[test]
    fn submit_order_body_keeps_compat_fields_and_internal_trace_price() {
        let request = sample_order_request(dec!(0.123456));
        let (body, serialized_price, serialized_cents) =
            KalshiClient::build_submit_order_body(&request).expect("body should serialize");

        assert_eq!(body.get("ticker").and_then(Value::as_str), Some("BTC-2026"));
        assert_eq!(body.get("action").and_then(Value::as_str), Some("buy"));
        assert_eq!(body.get("side").and_then(Value::as_str), Some("yes"));
        assert_eq!(body.get("count").and_then(Value::as_u64), Some(25));
        assert_eq!(body.get("price").and_then(Value::as_u64), Some(12));
        assert!(
            body.get("dollars").is_none(),
            "must not send unknown fields"
        );
        assert_eq!(serialized_price, "0.123456");
        assert_eq!(serialized_cents, 12);
    }

    #[test]
    fn serialize_limit_price_keeps_traceable_dollars_string() {
        let (price_cents, dollars) =
            KalshiClient::serialize_limit_price(dec!(0.009)).expect("price should serialize");
        assert_eq!(price_cents, 1);
        assert_eq!(dollars, "0.009");
    }

    #[test]
    fn serialize_limit_price_uses_midpoint_away_from_zero() {
        let (price_cents, dollars) =
            KalshiClient::serialize_limit_price(dec!(0.005)).expect("price should serialize");
        assert_eq!(price_cents, 1);
        assert_eq!(dollars, "0.005");
    }

    #[test]
    fn serialize_limit_price_applies_minimum_one_cent_for_tiny_positive_values() {
        let (price_cents, dollars) =
            KalshiClient::serialize_limit_price(dec!(0.001)).expect("price should serialize");
        assert_eq!(price_cents, 1);
        assert_eq!(dollars, "0.001");
    }

    #[test]
    fn serialize_limit_price_rejects_non_positive_price() {
        let err = KalshiClient::serialize_limit_price(Decimal::ZERO)
            .expect_err("zero price should be rejected");
        match err {
            PloyError::Validation(msg) => {
                assert!(msg.contains("limit_price must be > 0"));
            }
            other => panic!("expected validation error, got: {:?}", other),
        }
    }

    #[test]
    fn build_sign_payload_is_stable() {
        let payload = KalshiClient::build_sign_payload(
            "1700000000123",
            &Method::POST,
            "/portfolio/orders",
            "{\"a\":1}",
        );

        assert_eq!(payload, "1700000000123POST/portfolio/orders{\"a\":1}");
    }

    #[test]
    fn hmac_signature_for_payload_is_stable() {
        let payload = "1700000000123POST/portfolio/orders{\"a\":1}";
        let signature = KalshiClient::hmac_signature("testsecret", payload)
            .expect("signature should be generated");

        assert_eq!(signature, "ckCxXMAnY9OGsFnZKIKUqm8P4iKMdZs/SinJqLZFIcM=");
    }
}
