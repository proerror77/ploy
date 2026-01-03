//! Crypto market discovery
//!
//! Discovers crypto UP/DOWN markets from Polymarket series.

use crate::adapters::PolymarketClient;
use crate::error::Result;
use crate::strategy::core::{BinaryMarket, MarketDiscovery, MarketType};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tracing::{debug, info};

/// Known crypto series IDs
pub const SERIES_SOL_15M: &str = "10423";
pub const SERIES_ETH_15M: &str = "10191";
pub const SERIES_BTC_DAILY: &str = "41";

/// Crypto market discovery
pub struct CryptoMarketDiscovery {
    client: PolymarketClient,
    series_ids: Vec<String>,
}

impl CryptoMarketDiscovery {
    pub fn new(client: PolymarketClient) -> Self {
        Self {
            client,
            series_ids: vec![
                SERIES_SOL_15M.into(),
                SERIES_ETH_15M.into(),
                SERIES_BTC_DAILY.into(),
            ],
        }
    }
    
    pub fn with_series(client: PolymarketClient, series_ids: Vec<String>) -> Self {
        Self { client, series_ids }
    }
    
    /// Parse end date string to DateTime
    fn parse_end_date(end_date_str: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(end_date_str)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }
    
    /// Fetch markets for a specific series
    async fn fetch_series_markets(&self, series_id: &str) -> Result<Vec<BinaryMarket>> {
        let events = self.client.get_all_active_events(series_id).await?;
        info!("Found {} events in series {}", events.len(), series_id);
        
        let mut markets = Vec::new();
        
        // Only process first 5 events per series to avoid rate limits
        for event in events.into_iter().take(5) {
            // Get market details
            let event_details = match self.client.get_event_details(&event.id).await {
                Ok(e) => e,
                Err(e) => {
                    debug!("Failed to get event details for {}: {}", event.id, e);
                    continue;
                }
            };
            
            // Parse end time
            let end_time = event_details.end_date
                .as_ref()
                .and_then(|s| Self::parse_end_date(s))
                .unwrap_or_else(Utc::now);
            
            // Iterate through markets in the event
            for gamma_market in &event_details.markets {
                let condition_id = match &gamma_market.condition_id {
                    Some(cid) => cid.clone(),
                    None => continue,
                };
                
                // Get CLOB market for token IDs
                match self.client.get_market(&condition_id).await {
                    Ok(clob_market) => {
                        if clob_market.tokens.len() < 2 {
                            continue;
                        }
                        
                        let t1 = &clob_market.tokens[0];
                        let t2 = &clob_market.tokens[1];
                        
                        // Determine which is UP vs DOWN based on outcome
                        let (up_token, down_token) = if t1.outcome.to_lowercase().contains("up") 
                               || t1.outcome.to_lowercase().contains("yes") {
                            (t1.token_id.clone(), t2.token_id.clone())
                        } else {
                            (t2.token_id.clone(), t1.token_id.clone())
                        };
                        
                        let market = BinaryMarket::crypto_up_down(
                            event.id.clone(),
                            condition_id,
                            up_token,
                            down_token,
                            end_time,
                        );
                        
                        markets.push(market);
                    }
                    Err(e) => {
                        debug!("Failed to get CLOB market {}: {}", condition_id, e);
                    }
                }
            }
        }
        
        Ok(markets)
    }
}

#[async_trait]
impl MarketDiscovery for CryptoMarketDiscovery {
    fn market_type(&self) -> MarketType {
        MarketType::CryptoUpDown
    }
    
    async fn discover_markets(&self) -> Result<Vec<BinaryMarket>> {
        let mut all_markets = Vec::new();
        
        for series_id in &self.series_ids {
            match self.fetch_series_markets(series_id).await {
                Ok(markets) => {
                    info!("Discovered {} markets from series {}", markets.len(), series_id);
                    all_markets.extend(markets);
                }
                Err(e) => {
                    debug!("Failed to fetch series {}: {}", series_id, e);
                }
            }
        }
        
        Ok(all_markets)
    }
    
    async fn get_market(&self, event_id: &str) -> Result<Option<BinaryMarket>> {
        let event_details = self.client.get_event_details(event_id).await?;
        
        let end_time = event_details.end_date
            .as_ref()
            .and_then(|s| Self::parse_end_date(s))
            .unwrap_or_else(Utc::now);
        
        // Get first market with condition_id
        for gamma_market in &event_details.markets {
            if let Some(condition_id) = &gamma_market.condition_id {
                let clob_market = self.client.get_market(condition_id).await?;
                
                if clob_market.tokens.len() >= 2 {
                    let t1 = &clob_market.tokens[0];
                    let t2 = &clob_market.tokens[1];
                    
                    let (up_token, down_token) = if t1.outcome.to_lowercase().contains("up") {
                        (t1.token_id.clone(), t2.token_id.clone())
                    } else {
                        (t2.token_id.clone(), t1.token_id.clone())
                    };
                    
                    let market = BinaryMarket::crypto_up_down(
                        event_id.to_string(),
                        condition_id.clone(),
                        up_token,
                        down_token,
                        end_time,
                    );
                    
                    return Ok(Some(market));
                }
            }
        }
        
        Ok(None)
    }
}
