use super::{KalshiClient, MarketResponse, MarketSummary, Method, OutcomeSide, Result, Utc, Value};
use crate::adapters::polymarket_clob::{OrderBookLevel, TokenInfo};
use rust_decimal::Decimal;
use std::collections::HashMap;

impl KalshiClient {
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

    async fn fetch_orderbook(&self, ticker: &str) -> Result<super::OrderBookResponse> {
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

        Ok(super::OrderBookResponse {
            market: Some(ticker.to_string()),
            asset_id: format!("{}:yes", ticker),
            bids: resolved_bids,
            asks: resolved_asks,
            timestamp: Some(Utc::now().to_rfc3339()),
            hash: None,
        })
    }

    pub async fn get_order_book(&self, token_id: &str) -> Result<super::OrderBookResponse> {
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

    pub async fn get_best_prices(
        &self,
        token_id: &str,
    ) -> Result<(Option<Decimal>, Option<Decimal>)> {
        let (ticker, side) = OutcomeSide::from_token_id(token_id);
        let book = self.fetch_orderbook(&ticker).await?;

        if book.bids.is_empty() && book.asks.is_empty() {
            tracing::warn!(token_id, "Kalshi order book has no bids/asks");
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

        if side == OutcomeSide::No {
            bid = bid.map(|v| (Decimal::ONE - v).max(Decimal::ZERO));
            ask = ask.map(|v| (Decimal::ONE - v).max(Decimal::ZERO));
        }

        Ok((bid, ask))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn map_market_summary_serializes_binary_token_shape() {
        let summary = KalshiClient::map_market_summary(&json!({
            "ticker": "BTC-2026",
            "title": "Will BTC break ATH?",
            "yes_ask": 42,
            "no_ask": "58",
            "closed": false
        }));

        assert_eq!(summary.condition_id, "BTC-2026");
        assert_eq!(summary.question.as_deref(), Some("Will BTC break ATH?"));
        assert_eq!(
            summary.outcome_prices.as_deref(),
            Some("[\"0.42\",\"0.58\"]")
        );
    }

    #[test]
    fn extract_book_levels_supports_array_and_object_shapes() {
        let levels = KalshiClient::extract_book_levels(&json!([
            [42, 100],
            {"price": "0.57", "size": "12"}
        ]));

        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].price, "0.42");
        assert_eq!(levels[0].size, "100");
        assert_eq!(levels[1].price, "0.57");
        assert_eq!(levels[1].size, "12");
    }
}
