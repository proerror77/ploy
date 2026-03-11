// Polymarket Sports Markets Integration
// Fetches live sports betting markets from Polymarket using keyword filtering
// Based on: github.com/llSourcell/Poly-Trader

use crate::error::{PloyError, Result};
use polymarket_client_sdk::clob::types::request::OrderBookSummaryRequest;
use polymarket_client_sdk::clob::types::response::OrderBookSummaryResponse;
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk::gamma::types::request::{
    EventByIdRequest, MarketsRequest, SeriesByIdRequest,
};
use polymarket_client_sdk::gamma::types::response::{Event as GammaEvent, Market as GammaMarket};
use polymarket_client_sdk::gamma::Client as GammaClient;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;
use tracing::{debug, info, warn};

mod edge_analysis;
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

    fn decimal_to_f64(value: rust_decimal::Decimal) -> Option<f64> {
        value.to_string().parse::<f64>().ok()
    }

    fn map_tags(
        tags: Option<Vec<polymarket_client_sdk::gamma::types::response::Tag>>,
    ) -> Vec<String> {
        tags.unwrap_or_default()
            .into_iter()
            .filter_map(|t| t.label.or(t.slug))
            .collect()
    }

    fn map_live_game_market(market: GammaMarket) -> LiveGameMarket {
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

    fn map_live_game_event(event: GammaEvent) -> LiveGameEvent {
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

    fn map_event_details(event: GammaEvent) -> EventDetails {
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

    fn map_sports_market(market: GammaMarket) -> PolymarketSportsMarket {
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

    // ==================== LIVE GAMES API ====================

    /// Fetch all events from a sports series
    pub async fn fetch_series_events(&self, series_id: &str) -> Result<Vec<LiveGameEvent>> {
        let req = SeriesByIdRequest::builder().id(series_id).build();
        let series = self
            .gamma_client
            .series_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Gamma series fetch failed: {}", e)))?;

        let open_events: Vec<LiveGameEvent> = series
            .events
            .unwrap_or_default()
            .into_iter()
            .map(Self::map_live_game_event)
            .filter(|e| !e.closed)
            .collect();

        info!(
            "Found {} open events in series {}",
            open_events.len(),
            series_id
        );
        Ok(open_events)
    }

    /// Fetch NBA live game events
    pub async fn fetch_nba_live_games(&self) -> Result<Vec<LiveGameEvent>> {
        self.fetch_series_events(NBA_SERIES_ID).await
    }

    /// Filter games by date (format: "2026-01-03")
    pub async fn fetch_games_by_date(
        &self,
        series_id: &str,
        date: &str,
    ) -> Result<Vec<LiveGameEvent>> {
        let events = self.fetch_series_events(series_id).await?;

        let dated_events: Vec<LiveGameEvent> = events
            .into_iter()
            .filter(|e| e.slug.contains(date))
            .collect();

        info!("Found {} games on {}", dated_events.len(), date);
        Ok(dated_events)
    }

    /// Fetch today's NBA games
    pub async fn fetch_todays_nba_games(&self) -> Result<Vec<LiveGameEvent>> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        self.fetch_games_by_date(NBA_SERIES_ID, &today).await
    }

    /// Get full event details with markets
    pub async fn get_event_details(&self, event_id: &str) -> Result<EventDetails> {
        let req = EventByIdRequest::builder().id(event_id).build();
        let event = self
            .gamma_client
            .event_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Gamma event fetch failed: {}", e)))?;
        let event = Self::map_event_details(event);

        debug!("Event {} has {} markets", event.title, event.markets.len());
        Ok(event)
    }

    /// Find a live game by team names
    pub async fn find_live_game(&self, team1: &str, team2: &str) -> Result<Option<EventDetails>> {
        let team1_lower = team1.to_lowercase();
        let team2_lower = team2.to_lowercase();

        let events = self.fetch_nba_live_games().await?;

        for event in events {
            let title_lower = event.title.to_lowercase();
            if title_lower.contains(&team1_lower) && title_lower.contains(&team2_lower) {
                info!("Found live game: {}", event.title);
                return self.get_event_details(&event.id).await.map(Some);
            }
            // Also check slug for team abbreviations
            let slug_lower = event.slug.to_lowercase();
            if slug_lower.contains(&team1_lower) || slug_lower.contains(&team2_lower) {
                // Partial match, check if it's the right game
                let details = self.get_event_details(&event.id).await?;
                let detail_title = details.title.to_lowercase();
                if detail_title.contains(&team1_lower) || detail_title.contains(&team2_lower) {
                    info!("Found live game via slug: {}", details.title);
                    return Ok(Some(details));
                }
            }
        }

        warn!("No live game found for {} vs {}", team1, team2);
        Ok(None)
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

    /// Fetch currently live games (in-play)
    pub async fn fetch_live_games(&self, series_id: &str) -> Result<Vec<EventDetails>> {
        let events = self.fetch_series_events(series_id).await?;
        let mut live_games = Vec::new();

        for event in events {
            let details = self.get_event_details(&event.id).await?;
            if details.live && !details.ended {
                live_games.push(details);
            }
        }

        info!("Found {} live games", live_games.len());
        Ok(live_games)
    }

    /// Fetch live NBA games
    pub async fn fetch_nba_live_in_play(&self) -> Result<Vec<EventDetails>> {
        self.fetch_live_games(NBA_SERIES_ID).await
    }

    /// Fetch all today's games with full details
    pub async fn fetch_todays_games_with_details(
        &self,
        series_id: &str,
    ) -> Result<Vec<EventDetails>> {
        let now = chrono::Utc::now();
        let today = now.date_naive();
        // "Today NBA" should include the pre-game window and games in progress.
        // We use a time window rather than strict calendar matching to avoid UTC/Eastern date drift.
        let window_start = now - chrono::Duration::hours(18);
        let window_end = now + chrono::Duration::hours(36);

        let events = self.fetch_series_events(series_id).await?;
        let mut games = Vec::new();

        for event in events {
            if let Some(slug_date) = parse_trailing_slug_date(&event.slug) {
                // Limit network calls; open series can include many out-of-range events.
                if slug_date < today - chrono::Duration::days(2)
                    || slug_date > today + chrono::Duration::days(2)
                {
                    continue;
                }
            }

            let details = match self.get_event_details(&event.id).await {
                Ok(v) => v,
                Err(e) => {
                    debug!(event_id = %event.id, error = %e, "failed to fetch PM event details");
                    continue;
                }
            };

            let start_ts = details
                .start_time
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc));

            let include = if details.live && !details.ended {
                true
            } else if let Some(start_ts) = start_ts {
                start_ts >= window_start && start_ts <= window_end
            } else {
                details
                    .event_date
                    .as_deref()
                    .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                    .map(|d| d == today || d == (today - chrono::Duration::days(1)))
                    .unwrap_or(false)
            };

            if include {
                games.push(details);
            }
        }

        games.sort_by_key(|g| {
            g.start_time
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.timestamp())
                .unwrap_or(i64::MAX)
        });

        info!("Found {} games for today/live", games.len());
        Ok(games)
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

    fn map_order_book_response(book: OrderBookSummaryResponse) -> SportsOrderBook {
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
