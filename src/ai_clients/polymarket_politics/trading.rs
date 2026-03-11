use super::{
    PoliticsMarketDetails, PoliticsOrderBook, PolymarketPoliticsClient, PolymarketPoliticsMarket,
};
use crate::error::{PloyError, Result};
use polymarket_client_sdk::clob::types::request::OrderBookSummaryRequest;
use std::str::FromStr;
use tracing::warn;

impl PolymarketPoliticsClient {
    pub async fn get_order_book(&self, token_id: &str) -> Result<PoliticsOrderBook> {
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

    pub async fn get_market_details(
        &self,
        market: PolymarketPoliticsMarket,
    ) -> Result<Option<PoliticsMarketDetails>> {
        let (yes_token, no_token) = match market.get_token_ids() {
            Some(ids) => ids,
            None => {
                warn!("No token IDs found for market: {:?}", market.question);
                return Ok(None);
            }
        };

        let yes_book = self.get_order_book(&yes_token).await.ok();
        let no_book = self.get_order_book(&no_token).await.ok();

        Ok(Some(PoliticsMarketDetails {
            market,
            yes_token_id: yes_token,
            no_token_id: no_token,
            yes_book,
            no_book,
        }))
    }
}
