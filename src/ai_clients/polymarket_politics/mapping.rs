use super::{
    PoliticalEvent, PoliticalEventDetails, PoliticalMarketData, PoliticsOrderBook,
    PoliticsOrderBookLevel, PolymarketPoliticsClient, PolymarketPoliticsMarket,
};
use polymarket_client_sdk::clob::types::response::OrderBookSummaryResponse;
use polymarket_client_sdk::gamma::types::response::{
    Event as GammaEvent, Market as GammaMarket, Tag,
};

impl PolymarketPoliticsClient {
    pub(super) fn decimal_to_f64(value: rust_decimal::Decimal) -> Option<f64> {
        value.to_string().parse::<f64>().ok()
    }

    pub(super) fn map_tags(tags: Option<Vec<Tag>>) -> Vec<String> {
        tags.unwrap_or_default()
            .into_iter()
            .filter_map(|tag| tag.label.or(tag.slug))
            .collect()
    }

    pub(super) fn map_political_market_data(market: GammaMarket) -> PoliticalMarketData {
        let volume = market
            .volume
            .and_then(Self::decimal_to_f64)
            .or_else(|| market.volume_num.and_then(Self::decimal_to_f64));

        let outcome_prices = market.outcome_prices.map(|prices| {
            serde_json::to_string(
                &prices
                    .iter()
                    .map(|price| price.to_string())
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_default()
        });

        let clob_token_ids = market.clob_token_ids.map(|ids| {
            serde_json::to_string(&ids.iter().map(|id| id.to_string()).collect::<Vec<_>>())
                .unwrap_or_default()
        });

        let outcomes = market
            .outcomes
            .map(|outcomes| serde_json::to_string(&outcomes).unwrap_or_default());

        PoliticalMarketData {
            question: market.question.unwrap_or_default(),
            condition_id: market.condition_id.map(|condition| condition.to_string()),
            outcome_prices,
            clob_token_ids,
            volume,
            outcomes,
        }
    }

    pub(super) fn map_political_event(event: GammaEvent) -> PoliticalEvent {
        PoliticalEvent {
            id: event.id,
            title: event.title.unwrap_or_default(),
            slug: event.slug.unwrap_or_default(),
            closed: event.closed.unwrap_or(false),
            markets: event
                .markets
                .unwrap_or_default()
                .into_iter()
                .map(Self::map_political_market_data)
                .collect(),
        }
    }

    pub(super) fn map_political_event_details(event: GammaEvent) -> PoliticalEventDetails {
        let end_date = event.end_date.map(|ts| ts.to_rfc3339());
        let volume = event
            .volume
            .and_then(Self::decimal_to_f64)
            .or_else(|| event.volume_24hr.and_then(Self::decimal_to_f64));

        PoliticalEventDetails {
            id: event.id,
            title: event.title.unwrap_or_default(),
            slug: event.slug.unwrap_or_default(),
            closed: event.closed.unwrap_or(false),
            markets: event
                .markets
                .unwrap_or_default()
                .into_iter()
                .map(Self::map_political_market_data)
                .collect(),
            end_date,
            volume,
            description: event.description,
        }
    }

    pub(super) fn map_politics_market(market: GammaMarket) -> PolymarketPoliticsMarket {
        let volume = market
            .volume
            .and_then(Self::decimal_to_f64)
            .or_else(|| market.volume_num.and_then(Self::decimal_to_f64));

        let liquidity = market
            .liquidity
            .and_then(Self::decimal_to_f64)
            .or_else(|| market.liquidity_num.and_then(Self::decimal_to_f64));

        let outcome_prices = market.outcome_prices.map(|prices| {
            serde_json::to_string(
                &prices
                    .iter()
                    .map(|price| price.to_string())
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_default()
        });

        let clob_token_ids = market.clob_token_ids.map(|ids| {
            serde_json::to_string(&ids.iter().map(|id| id.to_string()).collect::<Vec<_>>())
                .unwrap_or_default()
        });

        PolymarketPoliticsMarket {
            condition_id: market
                .condition_id
                .map(|condition| condition.to_string())
                .unwrap_or_default(),
            question: market.question,
            slug: market.slug,
            active: market.active.unwrap_or(true),
            closed: market.closed.unwrap_or(false),
            end_date: market
                .end_date_iso
                .map(|date| date.to_string())
                .or_else(|| market.end_date.map(|date| date.to_rfc3339())),
            clob_token_ids,
            outcome_prices,
            volume,
            liquidity,
            description: market.description,
            tags: Self::map_tags(market.tags),
        }
    }

    pub(super) fn map_order_book_response(book: OrderBookSummaryResponse) -> PoliticsOrderBook {
        PoliticsOrderBook {
            market: Some(book.market.to_string()),
            asset_id: book.asset_id.to_string(),
            bids: book
                .bids
                .into_iter()
                .map(|level| PoliticsOrderBookLevel {
                    price: level.price.to_string(),
                    size: level.size.to_string(),
                })
                .collect(),
            asks: book
                .asks
                .into_iter()
                .map(|level| PoliticsOrderBookLevel {
                    price: level.price.to_string(),
                    size: level.size.to_string(),
                })
                .collect(),
            timestamp: Some(book.timestamp.timestamp_millis().to_string()),
        }
    }
}
