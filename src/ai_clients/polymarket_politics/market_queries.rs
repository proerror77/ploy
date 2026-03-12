use super::{PoliticalCategory, PolymarketPoliticsClient, PolymarketPoliticsMarket};
use crate::error::{PloyError, Result};
use polymarket_client_sdk::gamma::types::request::MarketsRequest;
use tracing::{debug, info};

impl PolymarketPoliticsClient {
    /// Fetch all active markets from Gamma API
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

        let markets = markets
            .into_iter()
            .filter(|market| market.active.unwrap_or(true) && !market.closed.unwrap_or(false))
            .map(Self::map_politics_market)
            .collect::<Vec<_>>();

        debug!("Fetched {} total markets", markets.len());
        Ok(markets)
    }

    pub async fn fetch_politics_markets(&self) -> Result<Vec<PolymarketPoliticsMarket>> {
        let all_markets = self.fetch_all_markets(500).await?;
        let politics_markets = all_markets
            .into_iter()
            .filter(|market| market.is_politics_market() && market.active && !market.closed)
            .collect::<Vec<_>>();

        info!("Found {} active politics markets", politics_markets.len());
        Ok(politics_markets)
    }

    pub async fn fetch_by_category(
        &self,
        category: PoliticalCategory,
    ) -> Result<Vec<PolymarketPoliticsMarket>> {
        let all_markets = self.fetch_all_markets(500).await?;
        let filtered = all_markets
            .into_iter()
            .filter(|market| market.matches_category(category) && market.active && !market.closed)
            .collect::<Vec<_>>();

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

        let election_markets = politics_markets
            .into_iter()
            .filter(|market| {
                let question_lower = market
                    .question
                    .as_ref()
                    .map(|question| question.to_lowercase())
                    .unwrap_or_default();
                election_keywords
                    .iter()
                    .any(|keyword| question_lower.contains(keyword))
            })
            .collect::<Vec<_>>();

        info!("Found {} election markets", election_markets.len());
        Ok(election_markets)
    }

    pub async fn fetch_trump_markets(&self) -> Result<Vec<PolymarketPoliticsMarket>> {
        self.search_markets("trump").await
    }

    pub async fn search_markets(&self, keyword: &str) -> Result<Vec<PolymarketPoliticsMarket>> {
        let all_markets = self.fetch_all_markets(500).await?;
        let keyword_lower = keyword.to_lowercase();

        let matching = all_markets
            .into_iter()
            .filter(|market| {
                market.active
                    && !market.closed
                    && (market
                        .question
                        .as_ref()
                        .map(|question| question.to_lowercase().contains(&keyword_lower))
                        .unwrap_or(false)
                        || market
                            .description
                            .as_ref()
                            .map(|description| description.to_lowercase().contains(&keyword_lower))
                            .unwrap_or(false))
            })
            .collect::<Vec<_>>();

        info!("Found {} markets matching '{}'", matching.len(), keyword);
        Ok(matching)
    }

    pub async fn search_candidate(&self, name: &str) -> Result<Vec<PolymarketPoliticsMarket>> {
        self.search_markets(name).await
    }
}
