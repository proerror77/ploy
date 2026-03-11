// Polymarket Sports Markets Integration
// Fetches live sports betting markets from Polymarket using keyword filtering
// Based on: github.com/llSourcell/Poly-Trader

use crate::error::{PloyError, Result};
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk::gamma::Client as GammaClient;
use serde::Deserialize;
mod edge_analysis;
mod live_games;
mod mapping;
mod market_queries;
mod models;
mod pricing_models;

pub use edge_analysis::PolymarketEdgeAnalysis;
pub use models::{
    EventDetails, LiveGameEvent, LiveGameMarket, PolymarketSportsMarket, SeriesResponse,
};
pub use pricing_models::{OrderBookLevel, SportsMarketDetails, SportsOrderBook};

const GAMMA_API_URL: &str = "https://gamma-api.polymarket.com";
const CLOB_API_URL: &str = "https://clob.polymarket.com";

/// Series IDs for different sports
pub const NBA_SERIES_ID: &str = "10345";
pub const NFL_SERIES_ID: &str = "10346"; // Placeholder, verify actual ID

/// Deserialize optional number that could be string or number
fn deserialize_optional_number<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(serde_json::Value::Number(n)) => Ok(n.as_f64()),
        Some(serde_json::Value::String(s)) => Ok(s.parse::<f64>().ok()),
        Some(_) => Ok(None),
    }
}

/// Gamma sometimes returns booleans as `null` for scheduled events.
fn deserialize_bool_from_null<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<bool>::deserialize(deserializer)?.unwrap_or(false))
}

fn parse_trailing_slug_date(slug: &str) -> Option<chrono::NaiveDate> {
    if slug.len() < 10 {
        return None;
    }
    chrono::NaiveDate::parse_from_str(&slug[slug.len().saturating_sub(10)..], "%Y-%m-%d").ok()
}

/// Sports keywords for filtering markets
pub const SPORTS_KEYWORDS: &[&str] = &[
    // NBA teams
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
    "spurs",
    "rockets",
    "hawks",
    "hornets",
    "pistons",
    "pacers",
    "magic",
    "wizards",
    "raptors",
    "cavaliers",
    "kings",
    "blazers",
    "jazz",
    // NFL teams
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
    "vikings",
    "saints",
    "chargers",
    "raiders",
    "broncos",
    "seahawks",
    "commanders",
    "bears",
    "giants",
    "jets",
    "patriots",
    "steelers",
    "browns",
    "colts",
    "texans",
    "titans",
    "jaguars",
    "panthers",
    "falcons",
    "buccaneers",
    "cardinals",
    "rams",
    // General sports terms
    "nba",
    "nfl",
    "nhl",
    "mlb",
    "ncaa",
    "basketball",
    "football",
    "hockey",
    "baseball",
    "super bowl",
    "playoffs",
    "championship",
    "mvp",
    "finals",
    // Game patterns
    "win",
    "beat",
    "defeat",
    "vs",
    "game",
    "match",
    "score",
    "points",
];

/// Polymarket Sports Client for fetching and trading sports markets
pub struct PolymarketSportsClient {
    gamma_client: GammaClient,
    clob_client: ClobClient,
}

impl PolymarketSportsClient {
    /// Create new sports client
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

    /// Get moneyline market from event
    pub fn extract_moneyline<'a>(&self, event: &'a EventDetails) -> Option<&'a LiveGameMarket> {
        event.markets.iter().find(|m| m.is_moneyline())
    }

    /// Get all spread markets from event
    pub fn extract_spreads<'a>(&self, event: &'a EventDetails) -> Vec<&'a LiveGameMarket> {
        event.markets.iter().filter(|m| m.is_spread()).collect()
    }

    /// Get all over/under markets from event
    pub fn extract_over_unders<'a>(&self, event: &'a EventDetails) -> Vec<&'a LiveGameMarket> {
        event.markets.iter().filter(|m| m.is_over_under()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sports_keyword_detection() {
        let market = PolymarketSportsMarket {
            condition_id: "test".to_string(),
            question: Some("Will the Lakers beat the Celtics?".to_string()),
            slug: None,
            active: true,
            closed: false,
            end_date: None,
            clob_token_ids: None,
            outcome_prices: None,
            volume: None,
            liquidity: None,
            description: None,
            tags: vec![],
        };

        assert!(market.is_sports_market());
    }

    #[test]
    fn test_team_extraction() {
        let market = PolymarketSportsMarket {
            condition_id: "test".to_string(),
            question: Some("Lakers vs Celtics".to_string()),
            slug: None,
            active: true,
            closed: false,
            end_date: None,
            clob_token_ids: None,
            outcome_prices: None,
            volume: None,
            liquidity: None,
            description: None,
            tags: vec![],
        };

        let teams = market.extract_teams();
        assert!(teams.is_some());
        let (team1, team2) = teams.unwrap();
        assert_eq!(team1, "Lakers");
        assert_eq!(team2, "Celtics");
    }

    #[test]
    fn test_token_id_parsing() {
        let market = PolymarketSportsMarket {
            condition_id: "test".to_string(),
            question: None,
            slug: None,
            active: true,
            closed: false,
            end_date: None,
            clob_token_ids: Some(r#"["token1", "token2"]"#.to_string()),
            outcome_prices: Some(r#"["0.55", "0.45"]"#.to_string()),
            volume: None,
            liquidity: None,
            description: None,
            tags: vec![],
        };

        let tokens = market.get_token_ids();
        assert!(tokens.is_some());
        let (yes, no) = tokens.unwrap();
        assert_eq!(yes, "token1");
        assert_eq!(no, "token2");

        let prices = market.get_prices();
        assert!(prices.is_some());
    }

    #[test]
    fn test_event_details_null_bools() {
        let raw = r#"
        {
          "id": "207673",
          "title": "Nets vs. Cavaliers",
          "slug": "nba-bkn-cle-2026-02-19",
          "closed": false,
          "live": null,
          "ended": null,
          "period": "NS",
          "eventDate": "2026-02-19",
          "startTime": "2026-02-20T00:00:00Z",
          "markets": []
        }
        "#;

        let event: EventDetails = serde_json::from_str(raw).expect("should parse EventDetails");
        assert!(!event.live);
        assert!(!event.ended);
        assert_eq!(event.event_date.as_deref(), Some("2026-02-19"));
    }
}
