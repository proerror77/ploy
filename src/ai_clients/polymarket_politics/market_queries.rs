use super::{
    PoliticalCategory, PoliticalEvent, PoliticalEventDetails, PolymarketPoliticsClient,
    PolymarketPoliticsMarket, TRUMP_APPROVAL_SERIES, TRUMP_FAVORABILITY_SERIES,
};
use crate::error::{PloyError, Result};
use polymarket_client_sdk::gamma::types::request::{
    EventByIdRequest, MarketsRequest, SeriesByIdRequest,
};
use tracing::{debug, info};

impl PolymarketPoliticsClient {
    pub async fn fetch_all_markets(&self, limit: u32) -> Result<Vec<PolymarketPoliticsMarket>> {
        let req = MarketsRequest::builder()
            .limit(i32::try_from(limit).unwrap_or(i32::MAX))
            .closed(false)
            .build();
        let markets = self
            .gamma_client
            .markets(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Gamma markets fetch failed: {}", e)))?;

        let markets: Vec<PolymarketPoliticsMarket> = markets
            .into_iter()
            .filter(|m| m.active.unwrap_or(true) && !m.closed.unwrap_or(false))
            .map(Self::map_politics_market)
            .collect();

        debug!("Fetched {} total markets", markets.len());
        Ok(markets)
    }

    pub async fn fetch_politics_markets(&self) -> Result<Vec<PolymarketPoliticsMarket>> {
        let all_markets = self.fetch_all_markets(500).await?;

        let politics_markets: Vec<PolymarketPoliticsMarket> = all_markets
            .into_iter()
            .filter(|m| m.is_politics_market() && m.active && !m.closed)
            .collect();

        info!("Found {} active politics markets", politics_markets.len());
        Ok(politics_markets)
    }

    pub async fn fetch_by_category(
        &self,
        category: PoliticalCategory,
    ) -> Result<Vec<PolymarketPoliticsMarket>> {
        let all_markets = self.fetch_all_markets(500).await?;

        let filtered: Vec<PolymarketPoliticsMarket> = all_markets
            .into_iter()
            .filter(|m| m.matches_category(category) && m.active && !m.closed)
            .collect();

        info!("Found {} {:?} markets", filtered.len(), category);
        Ok(filtered)
    }

    pub async fn fetch_approval_markets(&self) -> Result<Vec<PolymarketPoliticsMarket>> {
        self.fetch_by_category(PoliticalCategory::Approval).await
    }

    pub async fn fetch_election_markets(&self) -> Result<Vec<PolymarketPoliticsMarket>> {
        let politics_markets = self.fetch_politics_markets().await?;

        let election_keywords = [
            "election", "win", "primary", "caucus", "2024", "2025", "2026", "midterm",
        ];

        let election_markets: Vec<PolymarketPoliticsMarket> = politics_markets
            .into_iter()
            .filter(|m| {
                let question_lower = m
                    .question
                    .as_ref()
                    .map(|q| q.to_lowercase())
                    .unwrap_or_default();
                election_keywords.iter().any(|k| question_lower.contains(k))
            })
            .collect();

        info!("Found {} election markets", election_markets.len());
        Ok(election_markets)
    }

    pub async fn fetch_trump_markets(&self) -> Result<Vec<PolymarketPoliticsMarket>> {
        self.search_markets("trump").await
    }

    pub async fn search_markets(&self, keyword: &str) -> Result<Vec<PolymarketPoliticsMarket>> {
        let all_markets = self.fetch_all_markets(500).await?;
        let keyword_lower = keyword.to_lowercase();

        let matching: Vec<PolymarketPoliticsMarket> = all_markets
            .into_iter()
            .filter(|m| {
                m.active
                    && !m.closed
                    && (m
                        .question
                        .as_ref()
                        .map(|q| q.to_lowercase().contains(&keyword_lower))
                        .unwrap_or(false)
                        || m.description
                            .as_ref()
                            .map(|d| d.to_lowercase().contains(&keyword_lower))
                            .unwrap_or(false))
            })
            .collect();

        info!("Found {} markets matching '{}'", matching.len(), keyword);
        Ok(matching)
    }

    pub async fn search_candidate(&self, name: &str) -> Result<Vec<PolymarketPoliticsMarket>> {
        self.search_markets(name).await
    }

    pub async fn fetch_series_events(&self, series_id: &str) -> Result<Vec<PoliticalEvent>> {
        let req = SeriesByIdRequest::builder().id(series_id).build();
        let series = self
            .gamma_client
            .series_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Gamma series fetch failed: {}", e)))?;

        let open_events: Vec<PoliticalEvent> = series
            .events
            .unwrap_or_default()
            .into_iter()
            .map(Self::map_political_event)
            .filter(|e| !e.closed)
            .collect();

        info!(
            "Found {} open events in series {}",
            open_events.len(),
            series_id
        );
        Ok(open_events)
    }

    pub async fn fetch_trump_favorability_events(&self) -> Result<Vec<PoliticalEvent>> {
        self.fetch_series_events(TRUMP_FAVORABILITY_SERIES).await
    }

    pub async fn fetch_trump_approval_events(&self) -> Result<Vec<PoliticalEvent>> {
        self.fetch_series_events(TRUMP_APPROVAL_SERIES).await
    }

    pub async fn get_event_details(&self, event_id: &str) -> Result<PoliticalEventDetails> {
        let req = EventByIdRequest::builder().id(event_id).build();
        let event = self
            .gamma_client
            .event_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Gamma event fetch failed: {}", e)))?;
        let event = Self::map_political_event_details(event);

        debug!("Event {} has {} markets", event.title, event.markets.len());
        Ok(event)
    }
}
