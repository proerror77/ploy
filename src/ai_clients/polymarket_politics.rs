// Polymarket Politics Markets Integration
// Fetches political prediction markets from Polymarket using keyword filtering
// Based on the sports integration pattern

use crate::error::{PloyError, Result};
use polymarket_client_sdk::gamma::types::request::{
    EventByIdRequest, MarketsRequest, SeriesByIdRequest,
};
use polymarket_client_sdk::gamma::types::response::{Event as GammaEvent, Market as GammaMarket};
use polymarket_client_sdk::gamma::Client as GammaClient;
use reqwest::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

const GAMMA_API_URL: &str = "https://gamma-api.polymarket.com";
const CLOB_API_URL: &str = "https://clob.polymarket.com";

/// Known political series IDs from Polymarket
pub const TRUMP_FAVORABILITY_SERIES: &str = "10034";
pub const TRUMP_APPROVAL_SERIES: &str = "10767";
pub const TRUMP_CABINET_SERIES: &str = "10746";
pub const CANADIAN_REFERENDUM_SERIES: &str = "10568";

/// Political keywords for filtering markets
pub const POLITICS_KEYWORDS: &[&str] = &[
    // US Leaders & Politicians
    "trump",
    "biden",
    "harris",
    "vance",
    "obama",
    "desantis",
    "newsom",
    "pelosi",
    "mcconnell",
    "schumer",
    "pence",
    "cruz",
    "aoc",
    "rfk",
    // Offices & Institutions
    "president",
    "presidential",
    "senate",
    "congress",
    "governor",
    "house",
    "supreme court",
    "scotus",
    "cabinet",
    "secretary",
    "attorney general",
    // Elections & Voting
    "election",
    "primary",
    "caucus",
    "electoral",
    "vote",
    "votes",
    "ballot",
    "midterm",
    "2024",
    "2025",
    "2026",
    "runoff",
    "recount",
    // Polling & Approval
    "approval",
    "favorability",
    "polls",
    "polling",
    "rating",
    "popularity",
    "fivethirtyeight",
    "realclearpolitics",
    // Parties & Movements
    "republican",
    "democrat",
    "gop",
    "dnc",
    "rnc",
    "conservative",
    "liberal",
    "maga",
    "progressive",
    // Political Events
    "impeachment",
    "resignation",
    "nomination",
    "confirmation",
    "pardon",
    "indictment",
    "trial",
    "conviction",
    // International Politics
    "referendum",
    "secession",
    "treaty",
    "nato",
    "un",
    "g7",
    "sanctions",
    // General Political Terms
    "political",
    "politics",
    "policy",
    "legislation",
    "bill",
    "law",
];

/// Political market categories
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoliticalCategory {
    /// Presidential approval, favorability
    Presidential,
    /// Senate, House elections
    Congressional,
    /// Approval ratings, polling
    Approval,
    /// Referendums, international events
    Geopolitical,
    /// Cabinet, nominations, confirmations
    Executive,
    /// All categories
    All,
}

impl PoliticalCategory {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "presidential" | "president" => Self::Presidential,
            "congressional" | "congress" | "senate" | "house" => Self::Congressional,
            "approval" | "polls" | "polling" => Self::Approval,
            "geopolitical" | "international" => Self::Geopolitical,
            "executive" | "cabinet" => Self::Executive,
            _ => Self::All,
        }
    }

    pub fn keywords(&self) -> &[&str] {
        match self {
            Self::Presidential => &[
                "president",
                "presidential",
                "trump",
                "biden",
                "harris",
                "desantis",
            ],
            Self::Congressional => &["senate", "congress", "house", "midterm", "election"],
            Self::Approval => &["approval", "favorability", "polls", "rating", "popularity"],
            Self::Geopolitical => &["referendum", "secession", "treaty", "nato", "sanctions"],
            Self::Executive => &[
                "cabinet",
                "secretary",
                "nomination",
                "confirmation",
                "resignation",
            ],
            Self::All => POLITICS_KEYWORDS,
        }
    }
}

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

/// Political event from series endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoliticalEvent {
    pub id: String,
    pub title: String,
    pub slug: String,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub markets: Vec<PoliticalMarketData>,
}

/// Market within a political event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoliticalMarketData {
    /// Market question (e.g., "Trump positive favorability on April 1?")
    pub question: String,

    /// Condition ID for trading
    #[serde(rename = "conditionId", alias = "condition_id")]
    pub condition_id: Option<String>,

    /// Current outcome prices as JSON string "[\"0.52\", \"0.48\"]"
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

impl PoliticalMarketData {
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

    /// Check if this is an approval/favorability market
    pub fn is_approval_market(&self) -> bool {
        let q = self.question.to_lowercase();
        q.contains("approval") || q.contains("favorability") || q.contains("rating")
    }

    /// Check if this is an election market
    pub fn is_election_market(&self) -> bool {
        let q = self.question.to_lowercase();
        q.contains("election") || q.contains("win") || q.contains("primary")
    }
}

/// Series response containing events
#[derive(Debug, Clone, Deserialize)]
pub struct PoliticalSeriesResponse {
    pub id: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub events: Vec<PoliticalEvent>,
}

/// Full event details from /events/{id} endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoliticalEventDetails {
    pub id: String,
    pub title: String,
    pub slug: String,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub markets: Vec<PoliticalMarketData>,

    /// End date for the event
    #[serde(rename = "endDate")]
    pub end_date: Option<String>,

    /// Total trading volume
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    pub volume: Option<f64>,

    /// Description
    pub description: Option<String>,
}

impl PoliticalEventDetails {
    /// Get the primary market
    pub fn primary_market(&self) -> Option<&PoliticalMarketData> {
        self.markets.first()
    }

    /// Get formatted end date
    pub fn end_date_formatted(&self) -> String {
        self.end_date.clone().unwrap_or_else(|| "TBD".to_string())
    }
}

/// Polymarket political market from Gamma API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolymarketPoliticsMarket {
    /// Condition ID for CLOB trading
    #[serde(rename = "conditionId", alias = "condition_id")]
    pub condition_id: String,

    /// Market question (e.g., "Trump positive favorability on April 1?")
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

    /// Volume in USD
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    pub volume: Option<f64>,

    /// Liquidity available
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    pub liquidity: Option<f64>,

    /// Description
    pub description: Option<String>,

    /// Tags
    #[serde(default)]
    pub tags: Vec<String>,
}

impl PolymarketPoliticsMarket {
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

    /// Check if this is a politics market based on keywords
    pub fn is_politics_market(&self) -> bool {
        let question_lower = self
            .question
            .as_ref()
            .map(|q| q.to_lowercase())
            .unwrap_or_default();

        let desc_lower = self
            .description
            .as_ref()
            .map(|d| d.to_lowercase())
            .unwrap_or_default();

        let tags_lower: Vec<String> = self.tags.iter().map(|t| t.to_lowercase()).collect();

        // Check for explicit politics tag first
        if tags_lower
            .iter()
            .any(|t| t.contains("politic") || t.contains("election"))
        {
            return true;
        }

        POLITICS_KEYWORDS.iter().any(|keyword| {
            question_lower.contains(keyword)
                || desc_lower.contains(keyword)
                || tags_lower.iter().any(|t| t.contains(keyword))
        })
    }

    /// Check if market matches a specific category
    pub fn matches_category(&self, category: PoliticalCategory) -> bool {
        if category == PoliticalCategory::All {
            return self.is_politics_market();
        }

        let question_lower = self
            .question
            .as_ref()
            .map(|q| q.to_lowercase())
            .unwrap_or_default();

        category
            .keywords()
            .iter()
            .any(|k| question_lower.contains(k))
    }

    /// Extract candidate/subject from question
    pub fn extract_subject(&self) -> Option<String> {
        let question = self.question.as_ref()?;

        // Common patterns: "Trump approval...", "Will Biden..."
        let subjects = [
            "trump", "biden", "harris", "desantis", "newsom", "vance", "pence",
        ];

        for subject in subjects {
            if question.to_lowercase().contains(subject) {
                return Some(subject.to_string());
            }
        }

        None
    }
}

/// Order book level from CLOB
#[derive(Debug, Clone, Deserialize)]
pub struct PoliticsOrderBookLevel {
    pub price: String,
    pub size: String,
}

/// Order book response from CLOB API
#[derive(Debug, Clone, Deserialize)]
pub struct PoliticsOrderBook {
    pub market: Option<String>,
    pub asset_id: String,
    pub bids: Vec<PoliticsOrderBookLevel>,
    pub asks: Vec<PoliticsOrderBookLevel>,
    pub timestamp: Option<String>,
}

impl PoliticsOrderBook {
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

/// Politics market with full trading details
#[derive(Debug, Clone)]
pub struct PoliticsMarketDetails {
    pub market: PolymarketPoliticsMarket,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub yes_book: Option<PoliticsOrderBook>,
    pub no_book: Option<PoliticsOrderBook>,
}

impl PoliticsMarketDetails {
    /// Get current YES price (implied probability)
    pub fn yes_price(&self) -> Option<Decimal> {
        self.yes_book.as_ref()?.mid_price()
    }

    /// Get current NO price
    pub fn no_price(&self) -> Option<Decimal> {
        self.no_book.as_ref()?.mid_price()
    }
}

/// Polymarket Politics Client for fetching and trading political markets
pub struct PolymarketPoliticsClient {
    client: Client,
    gamma_client: GammaClient,
    clob_url: String,
}

impl PolymarketPoliticsClient {
    /// Create new politics client
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| PloyError::Internal(format!("HTTP client error: {}", e)))?;
        let gamma_client = GammaClient::new(GAMMA_API_URL)
            .map_err(|e| PloyError::Internal(format!("Gamma client error: {}", e)))?;

        Ok(Self {
            client,
            gamma_client,
            clob_url: CLOB_API_URL.to_string(),
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

    fn map_political_market_data(market: GammaMarket) -> PoliticalMarketData {
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

        PoliticalMarketData {
            question: market.question.unwrap_or_default(),
            condition_id: market.condition_id.map(|c| c.to_string()),
            outcome_prices,
            clob_token_ids,
            volume,
            outcomes,
        }
    }

    fn map_political_event(event: GammaEvent) -> PoliticalEvent {
        PoliticalEvent {
            id: event.id,
            title: event.title.unwrap_or_default(),
            slug: event.slug.unwrap_or_default(),
            closed: event.closed.unwrap_or(false),
            markets: event
                .markets
                .unwrap_or_default()
                .into_iter()
                .map(Self::map_political_market_data)
                .collect(),
        }
    }

    fn map_political_event_details(event: GammaEvent) -> PoliticalEventDetails {
        let end_date = event.end_date.map(|ts| ts.to_rfc3339());
        let volume = event
            .volume
            .and_then(Self::decimal_to_f64)
            .or_else(|| event.volume_24hr.and_then(Self::decimal_to_f64));

        PoliticalEventDetails {
            id: event.id,
            title: event.title.unwrap_or_default(),
            slug: event.slug.unwrap_or_default(),
            closed: event.closed.unwrap_or(false),
            markets: event
                .markets
                .unwrap_or_default()
                .into_iter()
                .map(Self::map_political_market_data)
                .collect(),
            end_date,
            volume,
            description: event.description,
        }
    }

    fn map_politics_market(market: GammaMarket) -> PolymarketPoliticsMarket {
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

        PolymarketPoliticsMarket {
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

    /// Fetch politics markets using keyword filtering
    pub async fn fetch_politics_markets(&self) -> Result<Vec<PolymarketPoliticsMarket>> {
        let all_markets = self.fetch_all_markets(500).await?;

        let politics_markets: Vec<PolymarketPoliticsMarket> = all_markets
            .into_iter()
            .filter(|m| m.is_politics_market() && m.active && !m.closed)
            .collect();

        info!("Found {} active politics markets", politics_markets.len());
        Ok(politics_markets)
    }

    /// Fetch markets by category
    pub async fn fetch_by_category(
        &self,
        category: PoliticalCategory,
    ) -> Result<Vec<PolymarketPoliticsMarket>> {
        let all_markets = self.fetch_all_markets(500).await?;

        let filtered: Vec<PolymarketPoliticsMarket> = all_markets
            .into_iter()
            .filter(|m| m.matches_category(category) && m.active && !m.closed)
            .collect();

        info!(
            "Found {} {} markets",
            filtered.len(),
            format!("{:?}", category)
        );
        Ok(filtered)
    }

    /// Fetch approval/favorability markets
    pub async fn fetch_approval_markets(&self) -> Result<Vec<PolymarketPoliticsMarket>> {
        self.fetch_by_category(PoliticalCategory::Approval).await
    }

    /// Fetch election markets
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

    /// Fetch Trump-related markets
    pub async fn fetch_trump_markets(&self) -> Result<Vec<PolymarketPoliticsMarket>> {
        self.search_markets("trump").await
    }

    /// Search markets by keyword
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

    /// Search for specific candidate
    pub async fn search_candidate(&self, name: &str) -> Result<Vec<PolymarketPoliticsMarket>> {
        self.search_markets(name).await
    }

    // ==================== SERIES API ====================

    /// Fetch all events from a political series
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

    /// Fetch Trump favorability events
    pub async fn fetch_trump_favorability_events(&self) -> Result<Vec<PoliticalEvent>> {
        self.fetch_series_events(TRUMP_FAVORABILITY_SERIES).await
    }

    /// Fetch Trump approval events
    pub async fn fetch_trump_approval_events(&self) -> Result<Vec<PoliticalEvent>> {
        self.fetch_series_events(TRUMP_APPROVAL_SERIES).await
    }

    /// Get full event details
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

    // ==================== ORDER BOOK ====================

    /// Get order book for a token
    pub async fn get_order_book(&self, token_id: &str) -> Result<PoliticsOrderBook> {
        let url = format!("{}/book", self.clob_url);

        let resp = self
            .client
            .get(&url)
            .query(&[("token_id", token_id)])
            .send()
            .await
            .map_err(|e| PloyError::Internal(format!("Network error: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PloyError::Internal(format!(
                "CLOB API error {}: {}",
                status, text
            )));
        }

        let book: PoliticsOrderBook = resp
            .json()
            .await
            .map_err(|e| PloyError::Internal(format!("Parse error: {}", e)))?;

        Ok(book)
    }

    /// Get full market details with order books
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

/// Edge analysis comparing Polymarket with polling data
#[derive(Debug, Clone)]
pub struct PoliticsEdgeAnalysis {
    pub market: String,
    pub polymarket_yes_prob: Decimal,
    pub polymarket_no_prob: Decimal,
    pub poll_yes_prob: Decimal,
    pub poll_no_prob: Decimal,
    pub yes_edge: Decimal,
    pub no_edge: Decimal,
    pub recommended_side: String,
    pub edge: Decimal,
    pub yes_token_id: String,
    pub no_token_id: String,
}

impl PoliticsEdgeAnalysis {
    /// Calculate edge between Polymarket and polling data
    pub fn calculate(details: &PoliticsMarketDetails, poll_yes_prob: Decimal) -> Option<Self> {
        let poly_yes = details.yes_price()?;
        let poly_no = details.no_price()?;
        let poll_no = Decimal::ONE - poll_yes_prob;

        let yes_edge = poll_yes_prob - poly_yes;
        let no_edge = poll_no - poly_no;

        let (recommended_side, edge) = if yes_edge > no_edge {
            ("YES".to_string(), yes_edge)
        } else {
            ("NO".to_string(), no_edge)
        };

        Some(Self {
            market: details.market.question.clone().unwrap_or_default(),
            polymarket_yes_prob: poly_yes,
            polymarket_no_prob: poly_no,
            poll_yes_prob,
            poll_no_prob: poll_no,
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
    fn test_politics_keyword_detection() {
        let market = PolymarketPoliticsMarket {
            condition_id: "test".to_string(),
            question: Some("Trump positive favorability on April 1?".to_string()),
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

        assert!(market.is_politics_market());
    }

    #[test]
    fn test_category_matching() {
        let market = PolymarketPoliticsMarket {
            condition_id: "test".to_string(),
            question: Some("Biden approval rating above 40%?".to_string()),
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

        assert!(market.matches_category(PoliticalCategory::Approval));
        assert!(market.matches_category(PoliticalCategory::All));
    }

    #[test]
    fn test_subject_extraction() {
        let market = PolymarketPoliticsMarket {
            condition_id: "test".to_string(),
            question: Some("Will Trump win 2024 election?".to_string()),
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

        let subject = market.extract_subject();
        assert!(subject.is_some());
        assert_eq!(subject.unwrap(), "trump");
    }

    #[test]
    fn test_token_id_parsing() {
        let market = PolymarketPoliticsMarket {
            condition_id: "test".to_string(),
            question: None,
            slug: None,
            active: true,
            closed: false,
            end_date: None,
            clob_token_ids: Some(r#"["token1", "token2"]"#.to_string()),
            outcome_prices: Some(r#"["0.52", "0.48"]"#.to_string()),
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
