use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Known political series IDs from Polymarket
pub const TRUMP_FAVORABILITY_SERIES: &str = "10034";
pub const TRUMP_APPROVAL_SERIES: &str = "10767";
pub const TRUMP_CABINET_SERIES: &str = "10746";
pub const CANADIAN_REFERENDUM_SERIES: &str = "10568";

/// Political keywords for filtering markets
pub const POLITICS_KEYWORDS: &[&str] = &[
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
    "approval",
    "favorability",
    "polls",
    "polling",
    "rating",
    "popularity",
    "fivethirtyeight",
    "realclearpolitics",
    "republican",
    "democrat",
    "gop",
    "dnc",
    "rnc",
    "conservative",
    "liberal",
    "maga",
    "progressive",
    "impeachment",
    "resignation",
    "nomination",
    "confirmation",
    "pardon",
    "indictment",
    "trial",
    "conviction",
    "referendum",
    "secession",
    "treaty",
    "nato",
    "un",
    "g7",
    "sanctions",
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
    Presidential,
    Congressional,
    Approval,
    Geopolitical,
    Executive,
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
    pub question: String,
    #[serde(rename = "conditionId", alias = "condition_id")]
    pub condition_id: Option<String>,
    #[serde(rename = "outcomePrices", default)]
    pub outcome_prices: Option<String>,
    #[serde(rename = "clobTokenIds", default)]
    pub clob_token_ids: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    pub volume: Option<f64>,
    #[serde(default)]
    pub outcomes: Option<String>,
}

impl PoliticalMarketData {
    pub fn get_token_ids(&self) -> Option<(String, String)> {
        let ids_str = self.clob_token_ids.as_ref()?;
        let ids: Vec<String> = serde_json::from_str(ids_str).ok()?;
        if ids.len() >= 2 {
            Some((ids[0].clone(), ids[1].clone()))
        } else {
            None
        }
    }

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

    pub fn is_approval_market(&self) -> bool {
        let q = self.question.to_lowercase();
        q.contains("approval") || q.contains("favorability") || q.contains("rating")
    }

    pub fn is_election_market(&self) -> bool {
        let q = self.question.to_lowercase();
        q.contains("election") || q.contains("win") || q.contains("primary")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PoliticalSeriesResponse {
    pub id: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub events: Vec<PoliticalEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoliticalEventDetails {
    pub id: String,
    pub title: String,
    pub slug: String,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub markets: Vec<PoliticalMarketData>,
    #[serde(rename = "endDate")]
    pub end_date: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    pub volume: Option<f64>,
    pub description: Option<String>,
}

impl PoliticalEventDetails {
    pub fn primary_market(&self) -> Option<&PoliticalMarketData> {
        self.markets.first()
    }

    pub fn end_date_formatted(&self) -> String {
        self.end_date.clone().unwrap_or_else(|| "TBD".to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolymarketPoliticsMarket {
    #[serde(rename = "conditionId", alias = "condition_id")]
    pub condition_id: String,
    pub question: Option<String>,
    pub slug: Option<String>,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub closed: bool,
    #[serde(rename = "endDate")]
    pub end_date: Option<String>,
    #[serde(rename = "clobTokenIds", default)]
    pub clob_token_ids: Option<String>,
    #[serde(rename = "outcomePrices", default)]
    pub outcome_prices: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    pub volume: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    pub liquidity: Option<f64>,
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl PolymarketPoliticsMarket {
    pub fn get_token_ids(&self) -> Option<(String, String)> {
        let ids_str = self.clob_token_ids.as_ref()?;
        let ids: Vec<String> = serde_json::from_str(ids_str).ok()?;
        if ids.len() >= 2 {
            Some((ids[0].clone(), ids[1].clone()))
        } else {
            None
        }
    }

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

    pub fn extract_subject(&self) -> Option<String> {
        let question = self.question.as_ref()?;
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

#[derive(Debug, Clone, Deserialize)]
pub struct PoliticsOrderBookLevel {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PoliticsOrderBook {
    pub market: Option<String>,
    pub asset_id: String,
    pub bids: Vec<PoliticsOrderBookLevel>,
    pub asks: Vec<PoliticsOrderBookLevel>,
    pub timestamp: Option<String>,
}

impl PoliticsOrderBook {
    pub fn best_bid(&self) -> Option<Decimal> {
        self.bids.first()?.price.parse().ok()
    }

    pub fn best_ask(&self) -> Option<Decimal> {
        self.asks.first()?.price.parse().ok()
    }

    pub fn mid_price(&self) -> Option<Decimal> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some((bid + ask) / Decimal::from(2))
    }

    pub fn spread(&self) -> Option<Decimal> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some(ask - bid)
    }

    pub fn implied_probability(&self) -> Option<Decimal> {
        self.mid_price()
    }
}

#[derive(Debug, Clone)]
pub struct PoliticsMarketDetails {
    pub market: PolymarketPoliticsMarket,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub yes_book: Option<PoliticsOrderBook>,
    pub no_book: Option<PoliticsOrderBook>,
}

impl PoliticsMarketDetails {
    pub fn yes_price(&self) -> Option<Decimal> {
        self.yes_book.as_ref()?.mid_price()
    }

    pub fn no_price(&self) -> Option<Decimal> {
        self.no_book.as_ref()?.mid_price()
    }
}
