use super::*;
use polymarket_client_sdk::clob::types::response::OrderBookSummaryResponse;
use polymarket_client_sdk::gamma::types::response::{Event as GammaEvent, Market as GammaMarket};
use rust_decimal::Decimal;

impl PolymarketSportsClient {
    pub(super) fn decimal_to_f64(value: Decimal) -> Option<f64> {
        value.to_string().parse::<f64>().ok()
    }

    pub(super) fn map_tags(
        tags: Option<Vec<polymarket_client_sdk::gamma::types::response::Tag>>,
    ) -> Vec<String> {
        tags.unwrap_or_default()
            .into_iter()
            .filter_map(|t| t.label.or(t.slug))
            .collect()
    }

    pub(super) fn map_live_game_market(market: GammaMarket) -> LiveGameMarket {
        let volume = market
            .volume
            .and_then(Self::decimal_to_f64)
            .or_else(|| market.volume_num.and_then(Self::decimal_to_f64));

        let outcome_prices = market.outcome_prices.map(|prices| {
            serde_json::to_string(&prices.iter().map(|p| p.to_string()).collect::<Vec<_>>())
                .unwrap_or_default()
        });

        let clob_token_ids = market.clob_token_ids.map(|ids| {
            serde_json::to_string(&ids.iter().map(|id| id.to_string()).collect::<Vec<_>>())
                .unwrap_or_default()
        });

        let outcomes = market
            .outcomes
            .map(|o| serde_json::to_string(&o).unwrap_or_default());

        LiveGameMarket {
            question: market.question.unwrap_or_default(),
            condition_id: market.condition_id.map(|c| c.to_string()),
            outcome_prices,
            clob_token_ids,
            volume,
            outcomes,
        }
    }

    pub(super) fn map_live_game_event(event: GammaEvent) -> LiveGameEvent {
        LiveGameEvent {
            id: event.id,
            title: event.title.unwrap_or_default(),
            slug: event.slug.unwrap_or_default(),
            closed: event.closed.unwrap_or(false),
            markets: event
                .markets
                .unwrap_or_default()
                .into_iter()
                .map(Self::map_live_game_market)
                .collect(),
        }
    }

    pub(super) fn map_event_details(event: GammaEvent) -> EventDetails {
        let start_time = event
            .start_time
            .as_ref()
            .map(chrono::DateTime::<chrono::Utc>::to_rfc3339)
            .or_else(|| {
                event
                    .start_date
                    .as_ref()
                    .map(chrono::DateTime::<chrono::Utc>::to_rfc3339)
            });

        let event_date = event
            .event_date
            .map(|d| d.to_string())
            .or_else(|| {
                event
                    .start_time
                    .as_ref()
                    .map(|ts| ts.format("%Y-%m-%d").to_string())
            })
            .or_else(|| {
                event
                    .start_date
                    .as_ref()
                    .map(|ts| ts.format("%Y-%m-%d").to_string())
            });

        let volume = event
            .volume
            .and_then(Self::decimal_to_f64)
            .or_else(|| event.volume_24hr.and_then(Self::decimal_to_f64));

        EventDetails {
            id: event.id,
            title: event.title.unwrap_or_default(),
            slug: event.slug.unwrap_or_default(),
            closed: event.closed.unwrap_or(false),
            markets: event
                .markets
                .unwrap_or_default()
                .into_iter()
                .map(Self::map_live_game_market)
                .collect(),
            score: event.score,
            live: event.live.unwrap_or(false),
            period: event.period,
            elapsed: event.elapsed,
            ended: event.ended.unwrap_or(false),
            game_id: None,
            event_date,
            start_time,
            volume,
        }
    }

    pub(super) fn map_sports_market(market: GammaMarket) -> PolymarketSportsMarket {
        let volume = market
            .volume
            .and_then(Self::decimal_to_f64)
            .or_else(|| market.volume_num.and_then(Self::decimal_to_f64));

        let liquidity = market
            .liquidity
            .and_then(Self::decimal_to_f64)
            .or_else(|| market.liquidity_num.and_then(Self::decimal_to_f64));

        let outcome_prices = market.outcome_prices.map(|prices| {
            serde_json::to_string(&prices.iter().map(|p| p.to_string()).collect::<Vec<_>>())
                .unwrap_or_default()
        });

        let clob_token_ids = market.clob_token_ids.map(|ids| {
            serde_json::to_string(&ids.iter().map(|id| id.to_string()).collect::<Vec<_>>())
                .unwrap_or_default()
        });

        PolymarketSportsMarket {
            condition_id: market
                .condition_id
                .map(|c| c.to_string())
                .unwrap_or_default(),
            question: market.question,
            slug: market.slug,
            active: market.active.unwrap_or(true),
            closed: market.closed.unwrap_or(false),
            end_date: market
                .end_date_iso
                .map(|d| d.to_string())
                .or_else(|| market.end_date.map(|d| d.to_rfc3339())),
            clob_token_ids,
            outcome_prices,
            volume,
            liquidity,
            description: market.description,
            tags: Self::map_tags(market.tags),
        }
    }

    pub(super) fn map_order_book_response(book: OrderBookSummaryResponse) -> SportsOrderBook {
        SportsOrderBook {
            market: Some(book.market.to_string()),
            asset_id: book.asset_id.to_string(),
            bids: book
                .bids
                .into_iter()
                .map(|level| OrderBookLevel {
                    price: level.price.to_string(),
                    size: level.size.to_string(),
                })
                .collect(),
            asks: book
                .asks
                .into_iter()
                .map(|level| OrderBookLevel {
                    price: level.price.to_string(),
                    size: level.size.to_string(),
                })
                .collect(),
            timestamp: Some(book.timestamp.timestamp_millis().to_string()),
        }
    }
}
