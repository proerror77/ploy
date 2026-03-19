use super::*;
use polymarket_client_sdk::clob::types::request::OrderBookSummaryRequest;
use polymarket_client_sdk::gamma::types::request::MarketsRequest;
use std::str::FromStr;
use tracing::{debug, info, warn};

impl PolymarketSportsClient {
    /// Fetch all active markets from Gamma API
    pub async fn fetch_all_markets(&self, limit: u32) -> Result<Vec<PolymarketSportsMarket>> {
        let req = MarketsRequest::builder()
            .limit(i32::try_from(limit).unwrap_or(i32::MAX))
            .closed(false)
            .build();
        let markets = self
            .gamma_client
            .markets(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Gamma markets fetch failed: {}", e)))?;

        let markets: Vec<PolymarketSportsMarket> = markets
            .into_iter()
            .filter(|m| m.active.unwrap_or(true) && !m.closed.unwrap_or(false))
            .map(Self::map_sports_market)
            .collect();

        debug!("Fetched {} total markets", markets.len());
        Ok(markets)
    }

    /// Fetch sports markets using keyword filtering
    pub async fn fetch_sports_markets(&self) -> Result<Vec<PolymarketSportsMarket>> {
        let all_markets = self.fetch_all_markets(500).await?;

        let sports_markets: Vec<PolymarketSportsMarket> = all_markets
            .into_iter()
            .filter(|m| m.is_sports_market() && m.active && !m.closed)
            .collect();

        info!("Found {} active sports markets", sports_markets.len());
        Ok(sports_markets)
    }

    /// Fetch NBA-specific markets
    pub async fn fetch_nba_markets(&self) -> Result<Vec<PolymarketSportsMarket>> {
        let sports_markets = self.fetch_sports_markets().await?;

        let nba_keywords = [
            "nba",
            "lakers",
            "celtics",
            "warriors",
            "knicks",
            "heat",
            "bucks",
            "suns",
            "76ers",
            "nets",
            "bulls",
            "mavericks",
            "nuggets",
            "clippers",
            "grizzlies",
            "timberwolves",
            "pelicans",
            "thunder",
            "cavaliers",
            "kings",
            "hornets",
        ];

        let nba_markets: Vec<PolymarketSportsMarket> = sports_markets
            .into_iter()
            .filter(|m| {
                let question_lower = m
                    .question
                    .as_ref()
                    .map(|q| q.to_lowercase())
                    .unwrap_or_default();
                nba_keywords.iter().any(|k| question_lower.contains(k))
            })
            .collect();

        info!("Found {} NBA markets", nba_markets.len());
        Ok(nba_markets)
    }

    /// Fetch NFL-specific markets
    pub async fn fetch_nfl_markets(&self) -> Result<Vec<PolymarketSportsMarket>> {
        let sports_markets = self.fetch_sports_markets().await?;

        let nfl_keywords = [
            "nfl",
            "chiefs",
            "eagles",
            "bills",
            "cowboys",
            "49ers",
            "dolphins",
            "ravens",
            "bengals",
            "lions",
            "packers",
            "super bowl",
            "touchdown",
            "quarterback",
        ];

        let nfl_markets: Vec<PolymarketSportsMarket> = sports_markets
            .into_iter()
            .filter(|m| {
                let question_lower = m
                    .question
                    .as_ref()
                    .map(|q| q.to_lowercase())
                    .unwrap_or_default();
                nfl_keywords.iter().any(|k| question_lower.contains(k))
            })
            .collect();

        info!("Found {} NFL markets", nfl_markets.len());
        Ok(nfl_markets)
    }

    /// Search markets by specific keyword
    pub async fn search_markets(&self, keyword: &str) -> Result<Vec<PolymarketSportsMarket>> {
        let all_markets = self.fetch_all_markets(500).await?;
        let keyword_lower = keyword.to_lowercase();

        let matching: Vec<PolymarketSportsMarket> = all_markets
            .into_iter()
            .filter(|m| {
                m.active
                    && !m.closed
                    && m.question
                        .as_ref()
                        .map(|q| q.to_lowercase().contains(&keyword_lower))
                        .unwrap_or(false)
            })
            .collect();

        info!("Found {} markets matching '{}'", matching.len(), keyword);
        Ok(matching)
    }

    /// Get order book for a token
    pub async fn get_order_book(&self, token_id: &str) -> Result<SportsOrderBook> {
        let token_id = alloy::primitives::U256::from_str(token_id)
            .map_err(|e| PloyError::Internal(format!("Invalid token_id '{}': {}", token_id, e)))?;
        let req = OrderBookSummaryRequest::builder()
            .token_id(token_id)
            .build();
        let book = self
            .clob_client
            .order_book(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("CLOB order_book failed: {}", e)))?;

        Ok(Self::map_order_book_response(book))
    }

    /// Get full market details with order books
    pub async fn get_market_details(
        &self,
        market: PolymarketSportsMarket,
    ) -> Result<Option<SportsMarketDetails>> {
        let (yes_token, no_token) = match market.get_token_ids() {
            Some(ids) => ids,
            None => {
                warn!("No token IDs found for market: {:?}", market.question);
                return Ok(None);
            }
        };

        let yes_book = self.get_order_book(&yes_token).await.ok();
        let no_book = self.get_order_book(&no_token).await.ok();

        Ok(Some(SportsMarketDetails {
            market,
            yes_token_id: yes_token,
            no_token_id: no_token,
            yes_book,
            no_book,
        }))
    }

    /// Find market for a specific game (e.g., "Lakers vs Celtics")
    pub async fn find_game_market(
        &self,
        team1: &str,
        team2: &str,
    ) -> Result<Option<SportsMarketDetails>> {
        let team1_lower = team1.to_lowercase();
        let team2_lower = team2.to_lowercase();

        let markets = self.fetch_sports_markets().await?;

        for market in markets {
            let question_lower = market
                .question
                .as_ref()
                .map(|q| q.to_lowercase())
                .unwrap_or_default();

            if question_lower.contains(&team1_lower) && question_lower.contains(&team2_lower) {
                info!("Found matching market: {:?}", market.question);
                return self.get_market_details(market).await;
            }
        }

        warn!("No market found for {} vs {}", team1, team2);
        Ok(None)
    }
}
