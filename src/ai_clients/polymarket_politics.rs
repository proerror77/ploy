// Polymarket Politics Markets Integration
// Fetches political prediction markets from Polymarket using keyword filtering
// Based on the sports integration pattern

use crate::error::{PloyError, Result};
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk::gamma::Client as GammaClient;

mod edge_analysis;
mod mappers;
mod market_queries;
mod models;
#[cfg(test)]
mod tests;
mod trading;

pub use edge_analysis::PoliticsEdgeAnalysis;
pub use models::{
    PoliticalCategory, PoliticalEvent, PoliticalEventDetails, PoliticalMarketData,
    PoliticalSeriesResponse, PoliticsMarketDetails, PoliticsOrderBook, PoliticsOrderBookLevel,
    PolymarketPoliticsMarket, CANADIAN_REFERENDUM_SERIES, POLITICS_KEYWORDS, TRUMP_APPROVAL_SERIES,
    TRUMP_CABINET_SERIES, TRUMP_FAVORABILITY_SERIES,
};

const GAMMA_API_URL: &str = "https://gamma-api.polymarket.com";
const CLOB_API_URL: &str = "https://clob.polymarket.com";

/// Polymarket Politics Client for fetching and trading political markets
pub struct PolymarketPoliticsClient {
    gamma_client: GammaClient,
    clob_client: ClobClient,
}

impl PolymarketPoliticsClient {
    /// Create new politics client
    pub fn new() -> Result<Self> {
        let gamma_client = GammaClient::new(GAMMA_API_URL)
            .map_err(|e| PloyError::Internal(format!("Gamma client error: {}", e)))?;
        let clob_client = ClobClient::new(CLOB_API_URL, ClobConfig::default())
            .map_err(|e| PloyError::Internal(format!("CLOB client error: {}", e)))?;

        Ok(Self {
            gamma_client,
            clob_client,
        })
    }

    fn decimal_to_f64(value: rust_decimal::Decimal) -> Option<f64> {
        value.to_string().parse::<f64>().ok()
    }
}
