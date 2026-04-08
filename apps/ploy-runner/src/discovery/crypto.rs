use chrono::{DateTime, Utc};
use polymarket_client_sdk::gamma::types::response::{Event, Market};
use rust_decimal::Decimal;
use serde_json::Value;

use crate::discovery::types::{MarketDescriptor, MarketFamily, MarketSemantics, SettlementSource};
use crate::reference_prices::{
    latest_reference_price, market_symbol_to_chainlink_symbol, ReferencePriceRegistry,
    ReferencePriceSource,
};

#[derive(Debug, Clone)]
pub struct DiscoveredCryptoMarket {
    pub descriptor: MarketDescriptor,
    pub compatibility_event_id: String,
    pub symbol: String,
    pub up_token: String,
    pub down_token: String,
    pub end_time: DateTime<Utc>,
    pub window_secs: u64,
    pub price_to_beat: Option<Decimal>,
    pub raw_event: Option<Value>,
    pub raw_market: Value,
}

pub async fn discover_crypto_markets(
    markets: &[Market],
    configured_symbols: &[String],
    reference_prices: &ReferencePriceRegistry,
    now: DateTime<Utc>,
) -> Vec<DiscoveredCryptoMarket> {
    let mut discovered = Vec::new();

    for market in markets {
        if let Some(item) =
            normalize_crypto_market(market, configured_symbols, reference_prices, now).await
        {
            discovered.push(item);
        }
    }

    discovered
}

async fn normalize_crypto_market(
    market: &Market,
    configured_symbols: &[String],
    reference_prices: &ReferencePriceRegistry,
    now: DateTime<Utc>,
) -> Option<DiscoveredCryptoMarket> {
    let question = market.question.as_deref().unwrap_or("");
    let strategy_symbol = infer_crypto_strategy_symbol(question)?;

    if !configured_symbols
        .iter()
        .any(|configured| configured.eq_ignore_ascii_case(strategy_symbol))
    {
        return None;
    }

    let token_ids = market.clob_token_ids.as_ref()?;
    if token_ids.len() != 2 {
        return None;
    }

    let end_time = market.end_date?;
    let window_secs = (end_time - now).num_seconds().max(0) as u64;
    let up_token = token_ids[0].to_string();
    let down_token = token_ids[1].to_string();
    let reference_symbol = market_symbol_to_chainlink_symbol(strategy_symbol);
    let event = market.events.as_ref().and_then(|events| events.first());

    let price_to_beat = latest_reference_price(
        reference_prices,
        ReferencePriceSource::Chainlink,
        &reference_symbol,
    )
    .await
    .map(|snapshot| snapshot.value)
    .or_else(|| usable_metadata_threshold(market.group_item_threshold.as_deref()));

    let descriptor = MarketDescriptor {
        market_family: MarketFamily::Crypto,
        event_id: event.map(|value| value.id.clone()),
        event_slug: event.and_then(|value| value.slug.clone()),
        market_id: market.id.clone(),
        market_slug: market.slug.clone(),
        title: market
            .question
            .clone()
            .or_else(|| event.and_then(|value| value.title.clone())),
        strategy_symbol: Some(strategy_symbol.to_string()),
        reference_symbol: Some(reference_symbol),
        settlement_source: SettlementSource::Chainlink,
        league: market
            .subcategory
            .clone()
            .or_else(|| market.category.clone()),
        sport: Some("crypto".to_string()),
        start_time: market
            .start_date
            .or_else(|| event.and_then(|value| value.start_date)),
        end_time: Some(end_time),
        token_ids: vec![up_token.clone(), down_token.clone()],
        market_semantics: MarketSemantics::UpDown,
        home_team: None,
        away_team: None,
        active: market.active,
        accepting_orders: market.accepting_orders,
    };

    Some(DiscoveredCryptoMarket {
        descriptor,
        compatibility_event_id: market.id.clone(),
        symbol: strategy_symbol.to_string(),
        up_token,
        down_token,
        end_time,
        window_secs,
        price_to_beat,
        raw_event: event.and_then(event_to_value),
        raw_market: serde_json::to_value(market).ok()?,
    })
}

fn event_to_value(event: &Event) -> Option<Value> {
    serde_json::to_value(event).ok()
}

fn usable_metadata_threshold(raw: Option<&str>) -> Option<Decimal> {
    raw.and_then(|value| value.parse::<Decimal>().ok())
        .filter(|threshold| *threshold > Decimal::ONE)
}

fn infer_crypto_strategy_symbol(question: &str) -> Option<&'static str> {
    let upper = question.to_ascii_uppercase();
    if upper.contains("BITCOIN") || upper.contains("BTC") {
        Some("BTCUSDT")
    } else if upper.contains("ETHEREUM") || upper.contains("ETH") {
        Some("ETHUSDT")
    } else if upper.contains("SOLANA") || upper.contains("SOL ") {
        Some("SOLUSDT")
    } else if upper.contains("XRP") {
        Some("XRPUSDT")
    } else if upper.contains("DOGECOIN") || upper.contains("DOGE") {
        Some("DOGEUSDT")
    } else if upper.contains("HYPE") {
        Some("HYPEUSDT")
    } else if upper.contains("BNB") || upper.contains("BINANCE COIN") {
        Some("BNBUSDT")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;
    use serde_json::json;

    use super::discover_crypto_markets;
    use crate::reference_prices::{
        new_reference_price_registry, upsert_reference_price, ReferenceAssetClass,
        ReferencePriceKey, ReferencePriceSnapshot, ReferencePriceSource,
    };

    #[tokio::test]
    async fn normalizes_crypto_updown_markets_into_catalog_descriptors() {
        let registry = new_reference_price_registry();
        upsert_reference_price(
            &registry,
            ReferencePriceSnapshot {
                key: ReferencePriceKey {
                    source: ReferencePriceSource::Chainlink,
                    symbol: "btc/usd".to_string(),
                },
                asset_class: ReferenceAssetClass::Crypto,
                value: dec!(67234.50),
                full_accuracy_value: None,
                source_timestamp: Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 0).unwrap(),
                received_at: Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 1).unwrap(),
                is_carried_forward: false,
            },
        )
        .await;

        let market: polymarket_client_sdk::gamma::types::response::Market =
            serde_json::from_value(json!({
                "id": "market-123",
                "question": "Will Bitcoin be up or down in 5 minutes?",
                "slug": "bitcoin-up-or-down-apr-6-0000",
                "endDate": "2026-04-06T00:05:00Z",
                "startDate": "2026-04-06T00:00:00Z",
                "groupItemThreshold": "0",
                "clobTokenIds": "[\"111\",\"222\"]",
                "active": true,
                "acceptingOrders": true,
                "events": [{
                    "id": "event-456",
                    "slug": "bitcoin-up-or-down-apr-6-0000",
                    "title": "Bitcoin Up Or Down",
                    "startDate": "2026-04-06T00:00:00Z"
                }]
            }))
            .unwrap();

        let discovered = discover_crypto_markets(
            &[market],
            &["BTCUSDT".to_string()],
            &registry,
            Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 30).unwrap(),
        )
        .await;

        assert_eq!(discovered.len(), 1);
        let item = &discovered[0];
        assert_eq!(item.descriptor.market_id, "market-123");
        assert_eq!(item.descriptor.event_id.as_deref(), Some("event-456"));
        assert_eq!(item.descriptor.strategy_symbol.as_deref(), Some("BTCUSDT"));
        assert_eq!(item.descriptor.reference_symbol.as_deref(), Some("btc/usd"));
        assert_eq!(item.price_to_beat, Some(dec!(67234.50)));
        assert_eq!(item.compatibility_event_id, "market-123");
        assert_eq!(item.up_token, "111");
        assert_eq!(item.down_token, "222");
    }
}
