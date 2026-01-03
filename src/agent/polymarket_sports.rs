// Polymarket Sports Markets Integration
// Fetches live sports betting markets from Polymarket using keyword filtering
// Based on: github.com/llSourcell/Poly-Trader

use crate::error::{PloyError, Result};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

const GAMMA_API_URL: &str = "https://gamma-api.polymarket.com";
const CLOB_API_URL: &str = "https://clob.polymarket.com";

/// Series IDs for different sports
pub const NBA_SERIES_ID: &str = "10345";
pub const NFL_SERIES_ID: &str = "10346"; // Placeholder, verify actual ID

/// Deserialize optional number that could be string or number
fn deserialize_optional_number<'de, D>(deserializer: D) -> std::result::Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(serde_json::Value::Number(n)) => {
            Ok(n.as_f64())
        }
        Some(serde_json::Value::String(s)) => {
            Ok(s.parse::<f64>().ok())
        }
        Some(_) => Ok(None),
    }
}

/// Sports keywords for filtering markets
pub const SPORTS_KEYWORDS: &[&str] = &[
    // NBA teams
    "lakers", "celtics", "warriors", "knicks", "heat", "bucks", "suns",
    "76ers", "nets", "bulls", "mavericks", "nuggets", "clippers", "grizzlies",
    "timberwolves", "pelicans", "thunder", "spurs", "rockets", "hawks",
    "hornets", "pistons", "pacers", "magic", "wizards", "raptors", "cavaliers",
    "kings", "blazers", "jazz",
    // NFL teams
    "chiefs", "eagles", "bills", "cowboys", "49ers", "dolphins", "ravens",
    "bengals", "lions", "packers", "vikings", "saints", "chargers", "raiders",
    "broncos", "seahawks", "commanders", "bears", "giants", "jets", "patriots",
    "steelers", "browns", "colts", "texans", "titans", "jaguars", "panthers",
    "falcons", "buccaneers", "cardinals", "rams",
    // General sports terms
    "nba", "nfl", "nhl", "mlb", "ncaa", "basketball", "football", "hockey",
    "baseball", "super bowl", "playoffs", "championship", "mvp", "finals",
    // Game patterns
    "win", "beat", "defeat", "vs", "game", "match", "score", "points",
];

/// Live game event from series endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveGameEvent {
    pub id: String,
    pub title: String,
    pub slug: String,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub markets: Vec<LiveGameMarket>,
}

/// Market within a live game event (moneyline, spread, O/U)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveGameMarket {
    /// Market question (e.g., "76ers vs. Knicks", "Spread: Knicks (-5.5)")
    pub question: String,

    /// Condition ID for trading
    #[serde(rename = "conditionId", alias = "condition_id")]
    pub condition_id: Option<String>,

    /// Current outcome prices as JSON string "[\"0.40\", \"0.60\"]"
    #[serde(rename = "outcomePrices", default)]
    pub outcome_prices: Option<String>,

    /// CLOB token IDs for trading as JSON string
    #[serde(rename = "clobTokenIds", default)]
    pub clob_token_ids: Option<String>,

    /// Trading volume
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    pub volume: Option<f64>,

    /// Outcomes as JSON string
    #[serde(default)]
    pub outcomes: Option<String>,
}

impl LiveGameMarket {
    /// Parse CLOB token IDs from JSON string
    pub fn get_token_ids(&self) -> Option<(String, String)> {
        let ids_str = self.clob_token_ids.as_ref()?;
        let ids: Vec<String> = serde_json::from_str(ids_str).ok()?;
        if ids.len() >= 2 {
            Some((ids[0].clone(), ids[1].clone()))
        } else {
            None
        }
    }

    /// Parse outcome prices from JSON string
    pub fn get_prices(&self) -> Option<(Decimal, Decimal)> {
        let prices_str = self.outcome_prices.as_ref()?;
        let prices: Vec<String> = serde_json::from_str(prices_str).ok()?;
        if prices.len() >= 2 {
            let p1 = prices[0].parse::<Decimal>().ok()?;
            let p2 = prices[1].parse::<Decimal>().ok()?;
            Some((p1, p2))
        } else {
            None
        }
    }

    /// Check if this is a moneyline market (not spread or O/U)
    pub fn is_moneyline(&self) -> bool {
        let q = self.question.to_lowercase();
        !q.contains("spread") && !q.contains("o/u") && !q.contains("over") && !q.contains("under")
    }

    /// Check if this is a spread market
    pub fn is_spread(&self) -> bool {
        self.question.to_lowercase().contains("spread")
    }

    /// Check if this is an over/under market
    pub fn is_over_under(&self) -> bool {
        let q = self.question.to_lowercase();
        q.contains("o/u") || q.contains("over") || q.contains("under")
    }
}

/// Series response containing events
#[derive(Debug, Clone, Deserialize)]
pub struct SeriesResponse {
    pub id: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub events: Vec<LiveGameEvent>,
}

/// Full event details from /events/{id} endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDetails {
    pub id: String,
    pub title: String,
    pub slug: String,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub markets: Vec<LiveGameMarket>,

    // Live game fields
    /// Live score (e.g., "124-87")
    pub score: Option<String>,
    /// Whether game is currently live
    #[serde(default)]
    pub live: bool,
    /// Current period (e.g., "Q4", "Q3", "1H", "2H")
    pub period: Option<String>,
    /// Time elapsed/remaining in period (e.g., "02:38")
    pub elapsed: Option<String>,
    /// Whether game has ended
    #[serde(default)]
    pub ended: bool,
    /// External game ID for data provider
    #[serde(rename = "gameId")]
    pub game_id: Option<u64>,
    /// Event date (YYYY-MM-DD)
    #[serde(rename = "eventDate")]
    pub event_date: Option<String>,
    /// Start time ISO
    #[serde(rename = "startTime")]
    pub start_time: Option<String>,
    /// Total trading volume
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    pub volume: Option<f64>,
}

impl EventDetails {
    /// Get parsed scores as (home_score, away_score)
    pub fn get_scores(&self) -> Option<(u32, u32)> {
        let score_str = self.score.as_ref()?;
        let parts: Vec<&str> = score_str.split('-').collect();
        if parts.len() == 2 {
            let home = parts[0].trim().parse().ok()?;
            let away = parts[1].trim().parse().ok()?;
            Some((home, away))
        } else {
            None
        }
    }

    /// Format live status string (e.g., "LIVE Q4 - 02:38")
    pub fn live_status(&self) -> String {
        if self.ended {
            return "FINAL".to_string();
        }
        if !self.live {
            return "SCHEDULED".to_string();
        }
        match (&self.period, &self.elapsed) {
            (Some(p), Some(e)) => format!("LIVE {} - {}", p, e),
            (Some(p), None) => format!("LIVE {}", p),
            _ => "LIVE".to_string(),
        }
    }

    /// Get the moneyline market
    pub fn moneyline(&self) -> Option<&LiveGameMarket> {
        self.markets.iter().find(|m| m.is_moneyline())
    }

    /// Get spread markets
    pub fn spreads(&self) -> Vec<&LiveGameMarket> {
        self.markets.iter().filter(|m| m.is_spread()).collect()
    }

    /// Get over/under markets
    pub fn over_unders(&self) -> Vec<&LiveGameMarket> {
        self.markets.iter().filter(|m| m.is_over_under()).collect()
    }
}

/// Polymarket market from Gamma API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolymarketSportsMarket {
    /// Condition ID for CLOB trading
    #[serde(rename = "conditionId", alias = "condition_id")]
    pub condition_id: String,

    /// Market question (e.g., "Will the Lakers beat the Celtics?")
    pub question: Option<String>,

    /// Market slug for URL
    pub slug: Option<String>,

    /// Whether market is active
    #[serde(default)]
    pub active: bool,

    /// Whether market is closed
    #[serde(default)]
    pub closed: bool,

    /// End date for the market
    #[serde(rename = "endDate")]
    pub end_date: Option<String>,

    /// Token IDs for trading [YES, NO]
    #[serde(rename = "clobTokenIds", default)]
    pub clob_token_ids: Option<String>,

    /// Current outcome prices as JSON string
    #[serde(rename = "outcomePrices", default)]
    pub outcome_prices: Option<String>,

    /// Volume in USD (can be string or number from API)
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    pub volume: Option<f64>,

    /// Liquidity available (can be string or number from API)
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    pub liquidity: Option<f64>,

    /// Description
    pub description: Option<String>,

    /// Tags
    #[serde(default)]
    pub tags: Vec<String>,
}

impl PolymarketSportsMarket {
    /// Parse CLOB token IDs from JSON string
    pub fn get_token_ids(&self) -> Option<(String, String)> {
        let ids_str = self.clob_token_ids.as_ref()?;
        let ids: Vec<String> = serde_json::from_str(ids_str).ok()?;
        if ids.len() >= 2 {
            Some((ids[0].clone(), ids[1].clone()))
        } else {
            None
        }
    }

    /// Parse outcome prices from JSON string
    pub fn get_prices(&self) -> Option<(Decimal, Decimal)> {
        let prices_str = self.outcome_prices.as_ref()?;
        let prices: Vec<String> = serde_json::from_str(prices_str).ok()?;
        if prices.len() >= 2 {
            let yes_price = prices[0].parse::<Decimal>().ok()?;
            let no_price = prices[1].parse::<Decimal>().ok()?;
            Some((yes_price, no_price))
        } else {
            None
        }
    }

    /// Check if this is a sports market based on keywords
    pub fn is_sports_market(&self) -> bool {
        let question_lower = self.question.as_ref()
            .map(|q| q.to_lowercase())
            .unwrap_or_default();

        let desc_lower = self.description.as_ref()
            .map(|d| d.to_lowercase())
            .unwrap_or_default();

        let tags_lower: Vec<String> = self.tags.iter()
            .map(|t| t.to_lowercase())
            .collect();

        SPORTS_KEYWORDS.iter().any(|keyword| {
            question_lower.contains(keyword) ||
            desc_lower.contains(keyword) ||
            tags_lower.iter().any(|t| t.contains(keyword))
        })
    }

    /// Extract team names from question
    pub fn extract_teams(&self) -> Option<(String, String)> {
        let question = self.question.as_ref()?;

        // Try patterns like "Team A vs Team B" or "Team A to beat Team B"
        if let Some(vs_pos) = question.to_lowercase().find(" vs ") {
            let parts: Vec<&str> = question.splitn(2, " vs ").collect();
            if parts.len() == 2 {
                return Some((
                    parts[0].trim().to_string(),
                    parts[1].split('?').next().unwrap_or(parts[1]).trim().to_string()
                ));
            }
        }

        if let Some(beat_pos) = question.to_lowercase().find(" beat ") {
            let before = &question[..beat_pos];
            let after = &question[beat_pos + 6..];
            let team1 = before.split_whitespace().last().unwrap_or(before);
            let team2 = after.split('?').next().unwrap_or(after).trim();
            return Some((team1.to_string(), team2.to_string()));
        }

        None
    }
}

/// Order book level from CLOB
#[derive(Debug, Clone, Deserialize)]
pub struct OrderBookLevel {
    pub price: String,
    pub size: String,
}

/// Order book response from CLOB API
#[derive(Debug, Clone, Deserialize)]
pub struct SportsOrderBook {
    pub market: Option<String>,
    pub asset_id: String,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub timestamp: Option<String>,
}

impl SportsOrderBook {
    /// Get best bid price
    pub fn best_bid(&self) -> Option<Decimal> {
        self.bids.first()?.price.parse().ok()
    }

    /// Get best ask price
    pub fn best_ask(&self) -> Option<Decimal> {
        self.asks.first()?.price.parse().ok()
    }

    /// Get mid price
    pub fn mid_price(&self) -> Option<Decimal> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some((bid + ask) / Decimal::from(2))
    }

    /// Get spread
    pub fn spread(&self) -> Option<Decimal> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some(ask - bid)
    }

    /// Calculate implied probability from YES token price
    pub fn implied_probability(&self) -> Option<Decimal> {
        self.mid_price()
    }
}

/// Sports market with full trading details
#[derive(Debug, Clone)]
pub struct SportsMarketDetails {
    pub market: PolymarketSportsMarket,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub yes_book: Option<SportsOrderBook>,
    pub no_book: Option<SportsOrderBook>,
}

impl SportsMarketDetails {
    /// Get current YES price (implied probability for home/favorite)
    pub fn yes_price(&self) -> Option<Decimal> {
        self.yes_book.as_ref()?.mid_price()
    }

    /// Get current NO price (implied probability against)
    pub fn no_price(&self) -> Option<Decimal> {
        self.no_book.as_ref()?.mid_price()
    }
}

/// Polymarket Sports Client for fetching and trading sports markets
pub struct PolymarketSportsClient {
    client: Client,
    gamma_url: String,
    clob_url: String,
}

impl PolymarketSportsClient {
    /// Create new sports client
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| PloyError::Internal(format!("HTTP client error: {}", e)))?;

        Ok(Self {
            client,
            gamma_url: GAMMA_API_URL.to_string(),
            clob_url: CLOB_API_URL.to_string(),
        })
    }

    /// Fetch all active markets from Gamma API
    pub async fn fetch_all_markets(&self, limit: u32) -> Result<Vec<PolymarketSportsMarket>> {
        let url = format!("{}/markets", self.gamma_url);

        let resp = self.client
            .get(&url)
            .query(&[
                ("limit", limit.to_string()),
                ("active", "true".to_string()),
                ("closed", "false".to_string()),
            ])
            .send()
            .await
            .map_err(|e| PloyError::Internal(format!("Network error: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::Internal(format!("Gamma API error {}: {}", status, text)));
        }

        let markets: Vec<PolymarketSportsMarket> = resp.json().await
            .map_err(|e| PloyError::Internal(format!("Parse error: {}", e)))?;

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

        let nba_keywords = ["nba", "lakers", "celtics", "warriors", "knicks", "heat",
                           "bucks", "suns", "76ers", "nets", "bulls", "mavericks",
                           "nuggets", "clippers", "grizzlies", "timberwolves",
                           "pelicans", "thunder", "cavaliers", "kings", "hornets"];

        let nba_markets: Vec<PolymarketSportsMarket> = sports_markets
            .into_iter()
            .filter(|m| {
                let question_lower = m.question.as_ref()
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

        let nfl_keywords = ["nfl", "chiefs", "eagles", "bills", "cowboys", "49ers",
                           "dolphins", "ravens", "bengals", "lions", "packers",
                           "super bowl", "touchdown", "quarterback"];

        let nfl_markets: Vec<PolymarketSportsMarket> = sports_markets
            .into_iter()
            .filter(|m| {
                let question_lower = m.question.as_ref()
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
                m.active && !m.closed &&
                m.question.as_ref()
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
        let url = format!("{}/series/{}", self.gamma_url, series_id);

        let resp = self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| PloyError::Internal(format!("Network error: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::Internal(format!("Series API error {}: {}", status, text)));
        }

        let series: SeriesResponse = resp.json().await
            .map_err(|e| PloyError::Internal(format!("Parse error: {}", e)))?;

        let open_events: Vec<LiveGameEvent> = series.events
            .into_iter()
            .filter(|e| !e.closed)
            .collect();

        info!("Found {} open events in series {}", open_events.len(), series_id);
        Ok(open_events)
    }

    /// Fetch NBA live game events
    pub async fn fetch_nba_live_games(&self) -> Result<Vec<LiveGameEvent>> {
        self.fetch_series_events(NBA_SERIES_ID).await
    }

    /// Filter games by date (format: "2026-01-03")
    pub async fn fetch_games_by_date(&self, series_id: &str, date: &str) -> Result<Vec<LiveGameEvent>> {
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
        let url = format!("{}/events/{}", self.gamma_url, event_id);

        let resp = self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| PloyError::Internal(format!("Network error: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::Internal(format!("Event API error {}: {}", status, text)));
        }

        let event: EventDetails = resp.json().await
            .map_err(|e| PloyError::Internal(format!("Parse error: {}", e)))?;

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
    pub async fn fetch_todays_games_with_details(&self, series_id: &str) -> Result<Vec<EventDetails>> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let events = self.fetch_series_events(series_id).await?;
        let mut games = Vec::new();

        for event in events {
            if event.slug.contains(&today) || event.slug.contains(&today.replace("-", "")) {
                let details = self.get_event_details(&event.id).await?;
                games.push(details);
            }
        }

        // Also check for games from yesterday that might still be live
        let yesterday = (chrono::Utc::now() - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
        for event in self.fetch_series_events(series_id).await? {
            if event.slug.contains(&yesterday) {
                let details = self.get_event_details(&event.id).await?;
                if details.live && !details.ended {
                    games.push(details);
                }
            }
        }

        info!("Found {} games for today/live", games.len());
        Ok(games)
    }

    /// Get order book for a token
    pub async fn get_order_book(&self, token_id: &str) -> Result<SportsOrderBook> {
        let url = format!("{}/book", self.clob_url);

        let resp = self.client
            .get(&url)
            .query(&[("token_id", token_id)])
            .send()
            .await
            .map_err(|e| PloyError::Internal(format!("Network error: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::Internal(format!("CLOB API error {}: {}", status, text)));
        }

        let book: SportsOrderBook = resp.json().await
            .map_err(|e| PloyError::Internal(format!("Parse error: {}", e)))?;

        Ok(book)
    }

    /// Get full market details with order books
    pub async fn get_market_details(&self, market: PolymarketSportsMarket) -> Result<Option<SportsMarketDetails>> {
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
    pub async fn find_game_market(&self, team1: &str, team2: &str) -> Result<Option<SportsMarketDetails>> {
        let team1_lower = team1.to_lowercase();
        let team2_lower = team2.to_lowercase();

        let markets = self.fetch_sports_markets().await?;

        for market in markets {
            let question_lower = market.question.as_ref()
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

/// Edge analysis comparing Polymarket with sportsbook odds
#[derive(Debug, Clone)]
pub struct PolymarketEdgeAnalysis {
    pub market: String,
    pub polymarket_yes_prob: Decimal,
    pub polymarket_no_prob: Decimal,
    pub sportsbook_yes_prob: Decimal,
    pub sportsbook_no_prob: Decimal,
    pub yes_edge: Decimal,
    pub no_edge: Decimal,
    pub recommended_side: String,
    pub edge: Decimal,
    pub yes_token_id: String,
    pub no_token_id: String,
}

impl PolymarketEdgeAnalysis {
    /// Calculate edge between Polymarket and sportsbook
    pub fn calculate(
        details: &SportsMarketDetails,
        sportsbook_yes_prob: Decimal,
    ) -> Option<Self> {
        let poly_yes = details.yes_price()?;
        let poly_no = details.no_price()?;
        let sb_no = Decimal::ONE - sportsbook_yes_prob;

        let yes_edge = sportsbook_yes_prob - poly_yes;
        let no_edge = sb_no - poly_no;

        let (recommended_side, edge) = if yes_edge > no_edge {
            ("YES".to_string(), yes_edge)
        } else {
            ("NO".to_string(), no_edge)
        };

        Some(Self {
            market: details.market.question.clone().unwrap_or_default(),
            polymarket_yes_prob: poly_yes,
            polymarket_no_prob: poly_no,
            sportsbook_yes_prob,
            sportsbook_no_prob: sb_no,
            yes_edge,
            no_edge,
            recommended_side,
            edge,
            yes_token_id: details.yes_token_id.clone(),
            no_token_id: details.no_token_id.clone(),
        })
    }

    /// Check if edge is significant (> 5%)
    pub fn is_significant(&self) -> bool {
        self.edge > Decimal::from_str_exact("0.05").unwrap_or(Decimal::ZERO)
    }

    /// Get recommended token ID for betting
    pub fn recommended_token(&self) -> &str {
        if self.recommended_side == "YES" {
            &self.yes_token_id
        } else {
            &self.no_token_id
        }
    }

    /// Calculate Kelly criterion bet fraction
    pub fn kelly_fraction(&self) -> Decimal {
        if self.edge <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let odds = if self.recommended_side == "YES" {
            Decimal::ONE / self.polymarket_yes_prob - Decimal::ONE
        } else {
            Decimal::ONE / self.polymarket_no_prob - Decimal::ONE
        };

        if odds > Decimal::ZERO {
            self.edge / odds
        } else {
            Decimal::ZERO
        }
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
}
