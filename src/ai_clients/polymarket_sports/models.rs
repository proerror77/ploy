use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::{SPORTS_KEYWORDS, deserialize_bool_from_null, deserialize_optional_number};

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

    /// Live score (e.g., "124-87")
    pub score: Option<String>,
    /// Whether game is currently live
    #[serde(default, deserialize_with = "deserialize_bool_from_null")]
    pub live: bool,
    /// Current period (e.g., "Q4", "Q3", "1H", "2H")
    pub period: Option<String>,
    /// Time elapsed/remaining in period (e.g., "02:38")
    pub elapsed: Option<String>,
    /// Whether game has ended
    #[serde(default, deserialize_with = "deserialize_bool_from_null")]
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

        SPORTS_KEYWORDS.iter().any(|keyword| {
            question_lower.contains(keyword)
                || desc_lower.contains(keyword)
                || tags_lower.iter().any(|t| t.contains(keyword))
        })
    }

    /// Extract team names from question
    pub fn extract_teams(&self) -> Option<(String, String)> {
        let question = self.question.as_ref()?;

        if question.to_lowercase().find(" vs ").is_some() {
            let parts: Vec<&str> = question.splitn(2, " vs ").collect();
            if parts.len() == 2 {
                return Some((
                    parts[0].trim().to_string(),
                    parts[1]
                        .split('?')
                        .next()
                        .unwrap_or(parts[1])
                        .trim()
                        .to_string(),
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
