//! Enhanced Sports Event Analyst with Multi-Source Data Aggregation
//!
//! Improvements:
//! - Multi-source data aggregation for reliability
//! - Data quality scoring and validation
//! - Intelligent caching and fallback
//! - Polymarket moneyline analysis

use crate::agent::grok::GrokClient;
use crate::agent::client::{ClaudeAgentClient, AgentClientConfig};
use crate::agent::sports_data::{SportsDataFetcher, StructuredGameData, format_for_claude};
use crate::agent::sports_data_aggregator::{SportsDataAggregator, AggregatedGameData};
use crate::adapters::polymarket_clob::GAMMA_API_URL;
use crate::error::{PloyError, Result};
use polymarket_client_sdk::gamma::types::request::{EventByIdRequest, SearchRequest};
use polymarket_client_sdk::gamma::types::response::{Event as GammaEvent, Market as GammaMarket};
use polymarket_client_sdk::gamma::Client as GammaClient;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, info, warn};

/// Sports event analysis result
#[derive(Debug, Clone)]
pub struct SportsAnalysis {
    /// Event identifier
    pub event_slug: String,
    /// Teams involved
    pub teams: (String, String),
    /// League (NBA, NFL, etc.)
    pub league: String,
    /// Structured game data from multiple sources
    pub structured_data: Option<StructuredGameData>,
    /// Market odds from Polymarket
    pub market_odds: MarketOdds,
    /// Claude's win probability prediction
    pub prediction: WinPrediction,
    /// Recommended action
    pub recommendation: TradeRecommendation,
    /// Data quality metrics
    pub data_quality: Option<DataQualityInfo>,
}

/// Data quality information
#[derive(Debug, Clone)]
pub struct DataQualityInfo {
    pub overall_score: f64,
    pub sources_used: Vec<String>,
    pub completeness: f64,
    pub freshness: f64,
}

/// Market odds from Polymarket with detailed breakdown
#[derive(Debug, Clone)]
pub struct MarketOdds {
    pub team1_yes_price: Decimal,
    pub team1_no_price: Decimal,
    pub team2_yes_price: Option<Decimal>,
    pub team2_no_price: Option<Decimal>,
    pub spread: Option<String>,
    /// Moneyline market details
    pub moneyline: Option<MoneylineMarket>,
    /// All available markets
    pub all_markets: Vec<PolymarketMarketInfo>,
}

/// Moneyline market information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoneylineMarket {
    pub question: String,
    pub team1_price: Decimal,
    pub team2_price: Decimal,
    pub team1_implied_prob: f64,
    pub team2_implied_prob: f64,
    pub volume: Option<f64>,
    pub token_ids: (String, String),
}

/// Polymarket market information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolymarketMarketInfo {
    pub question: String,
    pub market_type: MarketType,
    pub outcomes: Vec<String>,
    pub prices: Vec<Decimal>,
    pub volume: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MarketType {
    Moneyline,
    Spread,
    OverUnder,
    FirstHalfMoneyline,
    FirstHalfSpread,
    FirstHalfOverUnder,
    Other,
}

impl MarketType {
    pub fn from_question(question: &str) -> Self {
        let q = question.to_lowercase();

        if q.contains("1h moneyline") || q.contains("first half moneyline") {
            Self::FirstHalfMoneyline
        } else if q.contains("1h spread") || q.contains("first half spread") {
            Self::FirstHalfSpread
        } else if q.contains("1h o/u") || q.contains("first half o/u") {
            Self::FirstHalfOverUnder
        } else if q.contains("spread:") {
            Self::Spread
        } else if q.contains("o/u") || q.contains("over/under") {
            Self::OverUnder
        } else if q.contains(" vs. ") && !q.contains("spread") && !q.contains("o/u") {
            Self::Moneyline
        } else {
            Self::Other
        }
    }
}

/// Win probability prediction from Claude
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinPrediction {
    pub team1_win_prob: f64,
    pub team2_win_prob: f64,
    pub confidence: f64,
    pub reasoning: String,
    pub key_factors: Vec<String>,
}

/// Trade recommendation
#[derive(Debug, Clone)]
pub struct TradeRecommendation {
    pub action: TradeAction,
    pub side: String,
    pub edge: f64,
    pub suggested_size: Decimal,
    pub reasoning: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TradeAction {
    Buy,
    Sell,
    Hold,
    Avoid,
}

/// Enhanced Sports Event Analyst with multi-source aggregation
pub struct SportsAnalyst {
    aggregator: SportsDataAggregator,
    claude: ClaudeAgentClient,
    use_aggregator: bool,
}

impl SportsAnalyst {
    /// Create a new sports analyst with Grok and Claude
    pub fn new(grok: GrokClient, claude: ClaudeAgentClient) -> Self {
        let aggregator = SportsDataAggregator::new(grok);
        Self {
            aggregator,
            claude,
            use_aggregator: true,
        }
    }

    /// Create with legacy single-source mode
    pub fn new_legacy(grok: GrokClient, claude: ClaudeAgentClient) -> Self {
        let aggregator = SportsDataAggregator::new(grok);
        Self {
            aggregator,
            claude,
            use_aggregator: false,
        }
    }

    /// Create from environment with Opus model for decision making
    pub fn from_env() -> Result<Self> {
        use crate::agent::grok::GrokConfig;

        let grok = GrokClient::new(GrokConfig::from_env())?;
        if !grok.is_configured() {
            return Err(PloyError::Internal("GROK_API_KEY not configured".into()));
        }

        let aggregator = SportsDataAggregator::new(grok);

        // Use longer timeout and Opus model for complex sports analysis
        let mut config = AgentClientConfig::for_autonomous()
            .with_timeout(300); // 5 minutes for detailed analysis
        config.model = Some(std::env::var("PLOY_CLAUDE_MODEL").unwrap_or_else(|_| "opus".to_string()));
        let claude = ClaudeAgentClient::with_config(config);

        Ok(Self {
            aggregator,
            claude,
            use_aggregator: true,
        })
    }

    /// Analyze a sports event from Polymarket URL
    pub async fn analyze_event(&self, event_url: &str) -> Result<SportsAnalysis> {
        // 1. Parse event URL to extract slug, teams, and league
        let (event_slug, league, team1, team2) = self.parse_event_url(event_url)?;
        info!("Analyzing {} event: {} vs {}", league.to_uppercase(), team1, team2);

        // 2. Fetch structured data using aggregator or legacy fetcher
        info!("Fetching game data...");
        let (structured_data, data_quality) = if self.use_aggregator {
            // Use multi-source aggregator
            match self.aggregator.fetch_game_data(&team1, &team2, &league).await {
                Ok(aggregated) => {
                    info!("✓ Multi-source data aggregation successful");
                    info!("  Quality: {:.2}", aggregated.quality.overall_score);
                    info!("  Sources: {}", aggregated.source_names());

                    let quality_info = DataQualityInfo {
                        overall_score: aggregated.quality.overall_score,
                        sources_used: aggregated.sources.iter()
                            .map(|s| s.name().to_string())
                            .collect(),
                        completeness: aggregated.quality.completeness,
                        freshness: aggregated.quality.freshness,
                    };

                    (Some(aggregated.data), Some(quality_info))
                }
                Err(e) => {
                    warn!("Multi-source aggregation failed: {}, using Polymarket only", e);
                    (None, None)
                }
            }
        } else {
            // Legacy single-source mode
            let fetcher = SportsDataFetcher::new(
                self.aggregator.grok.clone()
            );
            match fetcher.fetch_game_data(&team1, &team2, &league).await {
                Ok(data) => (Some(data), None),
                Err(e) => {
                    warn!("Data fetch failed: {}, using Polymarket only", e);
                    (None, None)
                }
            }
        };

        // 3. Fetch market data from Polymarket
        let market_odds = self.fetch_market_odds_detailed(&event_slug, &team1, &team2).await?;

        // Log moneyline info if available
        if let Some(ref ml) = market_odds.moneyline {
            info!("Polymarket Moneyline:");
            info!("  {}: {:.3} ({:.1}%)", team1, ml.team1_price, ml.team1_implied_prob * 100.0);
            info!("  {}: {:.3} ({:.1}%)", team2, ml.team2_price, ml.team2_implied_prob * 100.0);
            if let Some(vol) = ml.volume {
                info!("  Volume: ${:.0}", vol);
            }
        }

        // 4. Send structured data to Claude Opus for win probability analysis
        info!("Sending to Claude Opus for analysis...");
        let prediction = self.get_claude_prediction(
            &team1, &team2,
            &market_odds,
            structured_data.as_ref(),
        ).await?;

        info!("Claude prediction: {} {:.1}% vs {} {:.1}% (confidence: {:.0}%)",
            team1, prediction.team1_win_prob * 100.0,
            team2, prediction.team2_win_prob * 100.0,
            prediction.confidence * 100.0);

        // 5. Generate trade recommendation based on edge
        let recommendation = self.generate_recommendation(
            &team1, &team2,
            &market_odds,
            &prediction
        );

        Ok(SportsAnalysis {
            event_slug,
            teams: (team1, team2),
            league,
            structured_data,
            market_odds,
            prediction,
            recommendation,
            data_quality,
        })
    }

    /// Fetch detailed market odds including moneyline
    async fn fetch_market_odds_detailed(
        &self,
        event_slug: &str,
        team1: &str,
        team2: &str
    ) -> Result<MarketOdds> {
        let gamma = GammaClient::new(GAMMA_API_URL)
            .map_err(|e| PloyError::Internal(format!("Failed to create Gamma client: {}", e)))?;

        // Try to get event by slug
        let search_slug = if event_slug.contains('/') {
            event_slug.split('/').last().unwrap_or(event_slug)
        } else {
            event_slug
        };

        debug!("Fetching Polymarket markets for: {}", search_slug);
        let req = SearchRequest::builder().q(search_slug).build();
        let results = tokio::time::timeout(Duration::from_secs(10), gamma.search(&req))
            .await
            .map_err(|_| PloyError::Internal("Gamma search timed out".to_string()))?
            .map_err(|e| PloyError::Internal(format!("Gamma search failed: {}", e)))?;

        let mut event = {
            let events = results.events.unwrap_or_default();
            let normalized = search_slug.trim_matches('/');
            events
                .iter()
                .find(|e| {
                    e.slug.as_deref().is_some_and(|slug| {
                        let s = slug.trim_matches('/');
                        s == normalized || s.ends_with(&format!("/{}", normalized))
                    })
                })
                .cloned()
                .or_else(|| events.into_iter().next())
        };

        let Some(mut event) = event.take() else {
            warn!("No event found for slug: {}", search_slug);
            return self.get_default_odds(team1, team2);
        };

        if event.markets.as_ref().map_or(true, |m| m.is_empty()) {
            let by_id_req = EventByIdRequest::builder().id(&event.id).build();
            if let Ok(full_event) = gamma.event_by_id(&by_id_req).await {
                event = full_event;
            }
        }

        self.parse_event_markets(&event, team1, team2)
    }

    /// Parse all markets from event
    fn parse_event_markets(
        &self,
        event: &GammaEvent,
        team1: &str,
        team2: &str,
    ) -> Result<MarketOdds> {
        let markets = event
            .markets
            .as_ref()
            .ok_or_else(|| PloyError::Internal("No markets found".into()))?;

        let mut all_markets = vec![];
        let mut moneyline_market = None;
        let mut main_yes_price = Decimal::new(50, 2);
        let mut main_no_price = Decimal::new(50, 2);
        let mut spread_info = None;

        for market in markets {
            let question = market.question.as_deref().unwrap_or("");

            let market_type = MarketType::from_question(question);

            // Parse prices
            let prices = self.parse_json_array_strings(market.outcome_prices.as_deref());

            // Parse outcomes
            let mut outcomes = self.parse_json_array_strings(market.outcomes.as_deref());
            if outcomes.len() < 2 {
                outcomes = vec![team1.to_string(), team2.to_string()];
            }

            // Parse volume
            let volume = market.volume
                .as_deref()
                .and_then(|s| s.parse::<f64>().ok());

            // Parse token IDs
            let token_ids = self.parse_json_array_strings(market.clob_token_ids.as_deref());

            // Convert prices to Decimal
            let decimal_prices: Vec<Decimal> = prices.iter()
                .filter_map(|p| p.parse::<f64>().ok())
                .filter_map(|f| Decimal::from_f64_retain(f))
                .collect();

            // Store market info
            all_markets.push(PolymarketMarketInfo {
                question: question.to_string(),
                market_type: market_type.clone(),
                outcomes: outcomes.clone(),
                prices: decimal_prices.clone(),
                volume,
            });

            // Extract specific market types
            match market_type {
                MarketType::Moneyline => {
                    if decimal_prices.len() >= 2 && token_ids.len() >= 2 {
                        let team1_price = decimal_prices[0];
                        let team2_price = decimal_prices[1];

                        moneyline_market = Some(MoneylineMarket {
                            question: question.to_string(),
                            team1_price,
                            team2_price,
                            team1_implied_prob: team1_price.to_string()
                                .parse::<f64>().unwrap_or(0.5),
                            team2_implied_prob: team2_price.to_string()
                                .parse::<f64>().unwrap_or(0.5),
                            volume,
                            token_ids: (token_ids[0].clone(), token_ids[1].clone()),
                        });

                        // Use moneyline as main prices
                        main_yes_price = team1_price;
                        main_no_price = team2_price;
                    }
                }
                MarketType::Spread => {
                    spread_info = Some(question.to_string());
                }
                _ => {}
            }
        }

        info!("Found {} markets for this event", all_markets.len());
        info!("Market types: {:?}",
            all_markets.iter().map(|m| &m.market_type).collect::<Vec<_>>());

        Ok(MarketOdds {
            team1_yes_price: main_yes_price,
            team1_no_price: main_no_price,
            team2_yes_price: Some(main_no_price),
            team2_no_price: Some(main_yes_price),
            spread: spread_info,
            moneyline: moneyline_market,
            all_markets,
        })
    }

    /// Get default odds when API fails
    fn get_default_odds(&self, _team1: &str, _team2: &str) -> Result<MarketOdds> {
        Ok(MarketOdds {
            team1_yes_price: Decimal::new(50, 2),
            team1_no_price: Decimal::new(50, 2),
            team2_yes_price: Some(Decimal::new(50, 2)),
            team2_no_price: Some(Decimal::new(50, 2)),
            spread: None,
            moneyline: None,
            all_markets: vec![],
        })
    }

    fn parse_json_array_strings(&self, raw: Option<&str>) -> Vec<String> {
        let Some(raw) = raw else { return vec![] };
        if let Ok(v) = serde_json::from_str::<Vec<String>>(raw) {
            return v;
        }
        if let Ok(v) = serde_json::from_str::<Vec<serde_json::Value>>(raw) {
            return v
                .into_iter()
                .map(|x| {
                    x.as_str()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| x.to_string())
                })
                .collect();
        }
        vec![]
    }

    // ... (keep existing methods: parse_event_url, get_claude_prediction, generate_recommendation, etc.)
    // These methods remain unchanged from the original implementation
}

// Re-export for compatibility
pub use crate::agent::sports_analyst_legacy::{
    SportsAnalysisWithDK,
    // ... other exports
};
