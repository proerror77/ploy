//! Live NBA In-Game Arbitrage Strategy
//!
//! Captures price inefficiencies during live games by:
//! 1. Monitoring real-time score and price changes
//! 2. Detecting extreme price deviations (e.g., 0.20 vs 0.80)
//! 3. Predicting comeback probability based on:
//!    - Team strength (historical data)
//!    - Time remaining
//!    - Score differential
//!    - Quarter/period
//! 4. Executing trades when edge > threshold

use crate::adapters::polymarket_clob::GAMMA_API_URL;
use crate::error::{PloyError, Result};
use chrono::{DateTime, Utc};
use polymarket_client_sdk::gamma::types::request::{EventByIdRequest, SeriesByIdRequest};
use polymarket_client_sdk::gamma::Client as GammaClient;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info, warn};

mod analysis;

use analysis::{analyze_game_for_opportunity, ArbitrageOpportunity, ComebackModel};

/// Live game state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveGameState {
    pub event_id: String,
    pub title: String,
    pub slug: String,
    pub team1: String,
    pub team2: String,
    pub live: bool,
    pub ended: bool,
    pub score: Option<GameScore>,
    pub period: Option<String>,
    pub elapsed: Option<String>,
    pub moneyline: MoneylinePrices,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameScore {
    pub team1_score: u32,
    pub team2_score: u32,
    pub differential: i32, // team1 - team2
}

impl GameScore {
    pub fn from_string(score_str: &str) -> Option<Self> {
        let parts: Vec<&str> = score_str.split('-').collect();
        if parts.len() == 2 {
            let team1_score = parts[0].trim().parse().ok()?;
            let team2_score = parts[1].trim().parse().ok()?;
            let differential = team1_score as i32 - team2_score as i32;
            Some(Self {
                team1_score,
                team2_score,
                differential,
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoneylinePrices {
    pub team1_price: f64,
    pub team2_price: f64,
    pub price_ratio: f64, // team1 / team2
    pub volume: f64,
}

impl MoneylinePrices {
    /// Check if prices show extreme deviation
    pub fn is_extreme_deviation(&self, threshold: f64) -> bool {
        // Check if one side is < threshold (e.g., 0.20)
        self.team1_price < threshold || self.team2_price < threshold
    }

    /// Get the underdog side
    pub fn underdog_side(&self) -> &str {
        if self.team1_price < self.team2_price {
            "team1"
        } else {
            "team2"
        }
    }

    /// Get underdog price
    pub fn underdog_price(&self) -> f64 {
        self.team1_price.min(self.team2_price)
    }
}

/// Live arbitrage monitor
pub struct LiveArbitrageMonitor {
    gamma_client: GammaClient,
    comeback_model: ComebackModel,
    price_history: HashMap<String, Vec<(DateTime<Utc>, MoneylinePrices)>>,
    /// Team strength factors (team name -> 0.8-1.2 multiplier)
    team_strength: HashMap<String, f64>,
}

impl LiveArbitrageMonitor {
    pub fn new() -> Self {
        Self {
            gamma_client: GammaClient::new(GAMMA_API_URL).unwrap(),
            comeback_model: ComebackModel::new(),
            price_history: HashMap::new(),
            team_strength: Self::default_team_strength(),
        }
    }

    /// Default NBA team strength factors based on historical comeback ability.
    /// Values range from 0.85 (weaker) to 1.15 (stronger).
    /// Neutral = 1.0 for unknown or average teams.
    fn default_team_strength() -> HashMap<String, f64> {
        let mut m = HashMap::new();
        // Elite tier (strong comeback teams)
        for name in [
            "Boston Celtics",
            "Denver Nuggets",
            "Oklahoma City Thunder",
            "Milwaukee Bucks",
            "Phoenix Suns",
        ] {
            m.insert(name.to_string(), 1.12);
        }
        // Above average
        for name in [
            "Cleveland Cavaliers",
            "Minnesota Timberwolves",
            "New York Knicks",
            "Dallas Mavericks",
            "LA Clippers",
        ] {
            m.insert(name.to_string(), 1.06);
        }
        // Below average
        for name in [
            "Charlotte Hornets",
            "Portland Trail Blazers",
            "San Antonio Spurs",
            "Utah Jazz",
            "Detroit Pistons",
        ] {
            m.insert(name.to_string(), 0.92);
        }
        // Weak tier
        for name in ["Washington Wizards", "Brooklyn Nets"] {
            m.insert(name.to_string(), 0.87);
        }
        // All other teams default to 1.0 via lookup
        m
    }

    /// Look up team strength factor (defaults to 1.0 for unknown teams)
    fn team_strength_factor(&self, team_name: &str) -> f64 {
        self.team_strength.get(team_name).copied().unwrap_or(1.0)
    }

    /// Monitor live NBA games for arbitrage opportunities
    pub async fn monitor_live_games(
        &mut self,
        min_price_deviation: f64, // e.g., 0.20 (20%)
        min_edge: f64,            // e.g., 0.10 (10%)
        interval_secs: u64,       // e.g., 30 seconds
    ) -> Result<()> {
        info!("Starting live arbitrage monitor...");
        info!("Min price deviation: {:.0}%", min_price_deviation * 100.0);
        info!("Min edge: {:.0}%", min_edge * 100.0);
        info!("Update interval: {}s", interval_secs);

        loop {
            match self
                .scan_for_opportunities(min_price_deviation, min_edge)
                .await
            {
                Ok(opportunities) => {
                    if !opportunities.is_empty() {
                        info!(
                            "\n🚨 Found {} arbitrage opportunities!",
                            opportunities.len()
                        );

                        for opp in opportunities {
                            self.print_opportunity(&opp);
                        }
                    } else {
                        debug!("No opportunities found in this scan");
                    }
                }
                Err(e) => {
                    warn!("Scan failed: {}", e);
                }
            }

            sleep(Duration::from_secs(interval_secs)).await;
        }
    }

    /// Scan for arbitrage opportunities
    pub async fn scan_for_opportunities(
        &mut self,
        min_price_deviation: f64,
        min_edge: f64,
    ) -> Result<Vec<ArbitrageOpportunity>> {
        // Fetch live NBA games
        let live_games = self.fetch_live_games().await?;

        info!("Scanning {} live games...", live_games.len());

        let mut opportunities = vec![];

        for game in live_games {
            // Check if prices show extreme deviation
            if !game.moneyline.is_extreme_deviation(min_price_deviation) {
                continue;
            }

            // Analyze opportunity
            if let Some(opp) = self.analyze_game(&game, min_edge).await {
                opportunities.push(opp);
            }
        }

        Ok(opportunities)
    }

    /// Fetch all live NBA games
    async fn fetch_live_games(&self) -> Result<Vec<LiveGameState>> {
        let req = SeriesByIdRequest::builder().id("10345").build(); // NBA 2026
        let series = self
            .gamma_client
            .series_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Gamma series fetch failed: {}", e)))?;

        let events = series.events.unwrap_or_default();

        let mut live_games = vec![];

        for event in events {
            let event_id = event.id;
            if event_id.is_empty() {
                continue;
            }

            // Fetch event details
            match self.fetch_game_state(&event_id).await {
                Ok(Some(game)) => {
                    if game.live && !game.ended {
                        live_games.push(game);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    debug!("Failed to fetch game {}: {}", event_id, e);
                }
            }
        }

        Ok(live_games)
    }

    /// Fetch game state
    async fn fetch_game_state(&self, event_id: &str) -> Result<Option<LiveGameState>> {
        let req = EventByIdRequest::builder().id(event_id).build();
        let event = match self.gamma_client.event_by_id(&req).await {
            Ok(event) => event,
            Err(_) => return Ok(None),
        };

        let title = event.title.clone().unwrap_or_default();
        let slug = event.slug.clone().unwrap_or_default();
        let live = event.live.unwrap_or(false);
        let ended = event.ended.unwrap_or(false);

        let score_str = event.score.as_deref();

        let score = score_str.and_then(|s| GameScore::from_string(s));
        let period = event.period.clone();
        let elapsed = event.elapsed.clone();

        // Find moneyline market
        let Some(markets) = event.markets.as_ref() else {
            return Ok(None);
        };

        let mut moneyline = None;
        let mut team1 = String::new();
        let mut team2 = String::new();

        for market in markets {
            let question = market.question.as_deref().unwrap_or("");

            // Find main moneyline (not 1H)
            if question.contains(" vs. ")
                && !question.contains("1H")
                && !question.contains("O/U")
                && !question.contains("Spread")
            {
                let prices: Vec<String> = market
                    .outcome_prices
                    .as_ref()
                    .map(|ps| ps.iter().map(|d| d.to_string()).collect())
                    .unwrap_or_default();
                let outcomes: Vec<String> = market.outcomes.clone().unwrap_or_default();
                let volume: f64 = market
                    .volume
                    .and_then(|d| d.to_string().parse::<f64>().ok())
                    .unwrap_or(0.0);

                if prices.len() >= 2 && outcomes.len() >= 2 {
                    let team1_price = prices[0].parse::<f64>().unwrap_or(0.5);
                    let team2_price = prices[1].parse::<f64>().unwrap_or(0.5);

                    team1 = outcomes[0].clone();
                    team2 = outcomes[1].clone();

                    let price_ratio = if team2_price > 0.0 {
                        team1_price / team2_price
                    } else {
                        1.0
                    };

                    moneyline = Some(MoneylinePrices {
                        team1_price,
                        team2_price,
                        price_ratio,
                        volume,
                    });

                    break;
                }
            }
        }

        if let Some(ml) = moneyline {
            Ok(Some(LiveGameState {
                event_id: event_id.to_string(),
                title,
                slug,
                team1,
                team2,
                live,
                ended,
                score,
                period,
                elapsed,
                moneyline: ml,
                timestamp: Utc::now(),
            }))
        } else {
            Ok(None)
        }
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

    /// Analyze game for arbitrage opportunity
    async fn analyze_game(
        &mut self,
        game: &LiveGameState,
        min_edge: f64,
    ) -> Option<ArbitrageOpportunity> {
        let underdog_team = if game.moneyline.team1_price < game.moneyline.team2_price {
            &game.team1
        } else {
            &game.team2
        };

        analyze_game_for_opportunity(
            &self.comeback_model,
            game,
            min_edge,
            self.team_strength_factor(underdog_team),
            &mut self.price_history,
        )
    }

    /// Print opportunity
    fn print_opportunity(&self, opp: &ArbitrageOpportunity) {
        println!("\n{}", "═".repeat(80));
        println!("🎯 ARBITRAGE OPPORTUNITY");
        println!("{}", "═".repeat(80));

        println!("\nGame: {}", opp.game.title);
        println!("Score: {:?}", opp.game.score);
        println!("Period: {}", opp.time_remaining);

        println!("\n💰 Opportunity:");
        println!("  Buy: {} YES", opp.underdog_team);
        println!(
            "  Current Price: {:.3} ({:.1}% implied)",
            opp.current_price,
            opp.current_price * 100.0
        );
        println!(
            "  Predicted Prob: {:.1}%",
            opp.predicted_comeback_prob * 100.0
        );
        println!("  Edge: {:+.1}%", opp.edge * 100.0);
        println!("  Expected Value: {:+.2} per $1", opp.expected_value);
        println!("  Confidence: {:.0}%", opp.confidence * 100.0);

        println!("\n📊 Analysis:");
        for reason in &opp.reasoning {
            println!("  • {}", reason);
        }

        println!("\n{}", "═".repeat(80));
    }

    /// Get price history for a game
    pub fn get_price_history(
        &self,
        event_id: &str,
    ) -> Option<&Vec<(DateTime<Utc>, MoneylinePrices)>> {
        self.price_history.get(event_id)
    }
}

#[cfg(test)]
mod tests {
    use super::GameScore;

    #[test]
    fn test_game_score_parsing() {
        let score = GameScore::from_string("102-119").unwrap();
        assert_eq!(score.team1_score, 102);
        assert_eq!(score.team2_score, 119);
        assert_eq!(score.differential, -17);
    }
}
