//! Sports market discovery
//!
//! Discovers sports betting markets from Polymarket.

use crate::adapters::PolymarketClient;
use crate::error::Result;
use crate::strategy::core::{BinaryMarket, MarketDiscovery, MarketType};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Supported sports leagues
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SportsLeague {
    NBA,
    NFL,
    MLB,
    NHL,
    Soccer,
    UFC,
    Custom,
}

impl std::fmt::Display for SportsLeague {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SportsLeague::NBA => write!(f, "NBA"),
            SportsLeague::NFL => write!(f, "NFL"),
            SportsLeague::MLB => write!(f, "MLB"),
            SportsLeague::NHL => write!(f, "NHL"),
            SportsLeague::Soccer => write!(f, "Soccer"),
            SportsLeague::UFC => write!(f, "UFC"),
            SportsLeague::Custom => write!(f, "Custom"),
        }
    }
}

/// Sports market discovery
pub struct SportsMarketDiscovery {
    client: PolymarketClient,
    leagues: Vec<SportsLeague>,
}

impl SportsMarketDiscovery {
    pub fn new(client: PolymarketClient) -> Self {
        Self {
            client,
            leagues: vec![SportsLeague::NBA, SportsLeague::NFL],
        }
    }

    pub fn with_leagues(client: PolymarketClient, leagues: Vec<SportsLeague>) -> Self {
        Self { client, leagues }
    }

    /// Get search keywords for a league
    fn league_keywords(&self, league: SportsLeague) -> Vec<&'static str> {
        match league {
            SportsLeague::NBA => vec!["NBA", "Lakers", "Celtics", "Warriors", "Knicks", "Bulls", "Heat", "Bucks"],
            SportsLeague::NFL => vec!["NFL", "Super Bowl", "Chiefs", "Eagles", "Cowboys", "Patriots", "49ers"],
            SportsLeague::MLB => vec!["MLB", "World Series", "Yankees", "Dodgers", "Red Sox"],
            SportsLeague::NHL => vec!["NHL", "Stanley Cup", "Bruins", "Rangers", "Maple Leafs"],
            SportsLeague::Soccer => vec!["Premier League", "Champions League", "World Cup", "UEFA"],
            SportsLeague::UFC => vec!["UFC", "MMA", "Octagon"],
            SportsLeague::Custom => vec!["sports"],
        }
    }

    /// Parse end date string to DateTime
    fn parse_end_date(end_date_str: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(end_date_str)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }

    /// Fetch markets for a specific league
    async fn fetch_league_markets(&self, league: SportsLeague) -> Result<Vec<BinaryMarket>> {
        let keywords = self.league_keywords(league);
        info!("Searching for {} markets with keywords: {:?}", league, keywords);

        // Fetch all active events from Gamma API
        let events = self.client.get_active_sports_events(&keywords[0]).await?;

        let mut markets = Vec::new();
        let now = Utc::now();

        for event in events {
            // Skip events that have already ended
            let end_time = event.end_date
                .as_ref()
                .and_then(|s| Self::parse_end_date(s))
                .unwrap_or(now);

            if end_time < now {
                continue;
            }

            // Check if event title matches any of our keywords
            let title = match &event.title {
                Some(t) => t,
                None => continue,
            };
            let title_lower = title.to_lowercase();
            let matches_league = keywords.iter().any(|kw| title_lower.contains(&kw.to_lowercase()));

            if !matches_league {
                continue;
            }

            // Process each market in the event
            for gamma_market in &event.markets {
                let condition_id = match &gamma_market.condition_id {
                    Some(cid) => cid.clone(),
                    None => continue,
                };

                // Get CLOB market for token IDs
                match self.client.get_market(&condition_id).await {
                    Ok(mut clob_market) => {
                        if clob_market.tokens.len() < 2 {
                            continue;
                        }

                        // Move token IDs out of owned vec to avoid cloning
                        let is_first_yes = clob_market.tokens[0].outcome.to_lowercase() == "yes";
                        let mut tokens = clob_market.tokens.drain(..2);
                        let first = tokens.next().unwrap();
                        let second = tokens.next().unwrap();
                        let (yes_token, no_token) = if is_first_yes {
                            (first.token_id, second.token_id)
                        } else {
                            (second.token_id, first.token_id)
                        };

                        // Get market question for metadata
                        let question = gamma_market.question.clone()
                            .or_else(|| event.title.clone())
                            .unwrap_or_else(|| "Unknown".to_string());

                        let market = BinaryMarket {
                            event_id: event.id.clone(),
                            condition_id,
                            yes_token_id: yes_token,
                            no_token_id: no_token,
                            yes_label: "Yes".to_string(),
                            no_label: "No".to_string(),
                            end_time,
                            market_type: MarketType::SportsMoneyline,
                            metadata: Some(question),
                        };

                        markets.push(market);
                    }
                    Err(e) => {
                        debug!("Failed to get CLOB market {}: {}", condition_id, e);
                    }
                }
            }
        }

        info!("Found {} {} binary markets", markets.len(), league);
        Ok(markets)
    }
}

#[async_trait]
impl MarketDiscovery for SportsMarketDiscovery {
    fn market_type(&self) -> MarketType {
        MarketType::SportsMoneyline
    }

    async fn discover_markets(&self) -> Result<Vec<BinaryMarket>> {
        let mut all_markets = Vec::new();

        for league in &self.leagues {
            match self.fetch_league_markets(*league).await {
                Ok(markets) => {
                    info!("Discovered {} markets for {}", markets.len(), league);
                    all_markets.extend(markets);
                }
                Err(e) => {
                    warn!("Failed to fetch {} markets: {}", league, e);
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
                let mut clob_market = self.client.get_market(condition_id).await?;

                if clob_market.tokens.len() >= 2 {
                    // Move token IDs out of owned vec to avoid cloning
                    let is_first_yes = clob_market.tokens[0].outcome.to_lowercase() == "yes";
                    let mut tokens = clob_market.tokens.drain(..2);
                    let first = tokens.next().unwrap();
                    let second = tokens.next().unwrap();
                    let (yes_token, no_token) = if is_first_yes {
                        (first.token_id, second.token_id)
                    } else {
                        (second.token_id, first.token_id)
                    };

                    let question = gamma_market.question.clone()
                        .or_else(|| event_details.title.clone())
                        .unwrap_or_else(|| "Unknown".to_string());

                    let market = BinaryMarket {
                        event_id: event_id.to_string(),
                        condition_id: condition_id.clone(),
                        yes_token_id: yes_token,
                        no_token_id: no_token,
                        yes_label: "Yes".to_string(),
                        no_label: "No".to_string(),
                        end_time,
                        market_type: MarketType::SportsMoneyline,
                        metadata: Some(question),
                    };

                    return Ok(Some(market));
                }
            }
        }

        Ok(None)
    }
}
