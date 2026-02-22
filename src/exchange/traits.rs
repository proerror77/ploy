use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::adapters::{
    BalanceResponse, MarketResponse, MarketSummary, OrderResponse, PositionResponse, TradeResponse,
};
use crate::domain::{OrderRequest, OrderStatus};
use crate::error::{PloyError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeKind {
    Polymarket,
    Kalshi,
}

impl Default for ExchangeKind {
    fn default() -> Self {
        Self::Polymarket
    }
}

impl ExchangeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Polymarket => "polymarket",
            Self::Kalshi => "kalshi",
        }
    }
}

impl std::fmt::Display for ExchangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for ExchangeKind {
    type Err = &'static str;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "polymarket" | "pm" => Ok(Self::Polymarket),
            "kalshi" => Ok(Self::Kalshi),
            _ => Err("invalid exchange; expected polymarket|kalshi"),
        }
    }
}

pub fn parse_exchange_kind(raw: &str) -> Result<ExchangeKind> {
    ExchangeKind::from_str(raw).map_err(|e| PloyError::Validation(e.to_string()))
}

fn unsupported(feature: &str, exchange: ExchangeKind) -> PloyError {
    PloyError::Validation(format!(
        "{} is not implemented for exchange '{}'",
        feature,
        exchange.as_str()
    ))
}

#[async_trait]
pub trait ExchangeClient: Send + Sync {
    fn kind(&self) -> ExchangeKind;

    fn is_dry_run(&self) -> bool;

    async fn submit_order_gateway(&self, request: &OrderRequest) -> Result<OrderResponse>;

    async fn get_order(&self, order_id: &str) -> Result<OrderResponse>;

    async fn cancel_order(&self, order_id: &str) -> Result<bool>;

    async fn get_best_prices(&self, token_id: &str) -> Result<(Option<Decimal>, Option<Decimal>)>;

    fn infer_order_status(&self, order: &OrderResponse) -> OrderStatus;

    fn calculate_fill(&self, order: &OrderResponse) -> (u64, Option<Decimal>);

    async fn get_market(&self, _market_id: &str) -> Result<MarketResponse> {
        Err(unsupported("get_market", self.kind()))
    }

    async fn search_markets(&self, _query: &str) -> Result<Vec<MarketSummary>> {
        Err(unsupported("search_markets", self.kind()))
    }

    async fn get_balance(&self) -> Result<BalanceResponse> {
        Err(unsupported("get_balance", self.kind()))
    }

    async fn get_positions(&self) -> Result<Vec<PositionResponse>> {
        Err(unsupported("get_positions", self.kind()))
    }

    async fn get_order_history(&self, _limit: Option<u32>) -> Result<Vec<OrderResponse>> {
        Err(unsupported("get_order_history", self.kind()))
    }

    async fn get_trades(&self, _limit: Option<u32>) -> Result<Vec<TradeResponse>> {
        Err(unsupported("get_trades", self.kind()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_exchange_kind_accepts_aliases() {
        assert_eq!(
            parse_exchange_kind("polymarket").expect("polymarket should parse"),
            ExchangeKind::Polymarket
        );
        assert_eq!(
            parse_exchange_kind("pm").expect("pm alias should parse"),
            ExchangeKind::Polymarket
        );
        assert_eq!(
            parse_exchange_kind("kalshi").expect("kalshi should parse"),
            ExchangeKind::Kalshi
        );
    }

    #[test]
    fn parse_exchange_kind_rejects_unknown_value() {
        assert!(parse_exchange_kind("foo").is_err());
    }
}
