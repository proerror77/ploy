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

use crate::error::{PloyError, Result};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info, warn};
use chrono::{DateTime, Utc};

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
            Some(Self { team1_score, team2_score, differential })
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

/// Comeback probability model
#[derive(Debug, Clone)]
pub struct ComebackModel {
    /// Historical comeback rates by score differential and time
    comeback_rates: HashMap<String, f64>,
}

impl ComebackModel {
    pub fn new() -> Self {
        let mut comeback_rates = HashMap::new();

        // Historical NBA comeback probabilities
        // Format: "period_differential" -> probability
        // Based on NBA historical data

        // Q1 (1st quarter) - 12 minutes remaining
        comeback_rates.insert("Q1_5".to_string(), 0.45);   // Down 5 in Q1
        comeback_rates.insert("Q1_10".to_string(), 0.35);  // Down 10 in Q1
        comeback_rates.insert("Q1_15".to_string(), 0.20);  // Down 15 in Q1

        // Q2 (2nd quarter) - 6-12 minutes remaining
        comeback_rates.insert("Q2_5".to_string(), 0.40);
        comeback_rates.insert("Q2_10".to_string(), 0.28);
        comeback_rates.insert("Q2_15".to_string(), 0.15);
        comeback_rates.insert("Q2_20".to_string(), 0.08);

        // Q3 (3rd quarter) - 0-6 minutes remaining
        comeback_rates.insert("Q3_5".to_string(), 0.35);
        comeback_rates.insert("Q3_10".to_string(), 0.22);
        comeback_rates.insert("Q3_15".to_string(), 0.12);
        comeback_rates.insert("Q3_20".to_string(), 0.05);

        // Q4 (4th quarter) - final 12 minutes
        comeback_rates.insert("Q4_5".to_string(), 0.30);
        comeback_rates.insert("Q4_10".to_string(), 0.15);
        comeback_rates.insert("Q4_15".to_string(), 0.08);
        comeback_rates.insert("Q4_20".to_string(), 0.03);

        Self { comeback_rates }
    }

    /// Predict comeback probability
    pub fn predict_comeback_prob(
        &self,
        period: &str,
        score_diff: i32,
        team_strength_factor: f64, // 0.8-1.2 (weak to strong)
    ) -> f64 {
        let abs_diff = score_diff.abs();

        // Round to nearest 5 for lookup
        let rounded_diff = ((abs_diff + 2) / 5) * 5;

        let key = format!("{}_{}", period, rounded_diff);

        let base_prob = self.comeback_rates.get(&key)
            .copied()
            .unwrap_or_else(|| {
                // Fallback: exponential decay based on differential
                let period_factor = match period {
                    "Q1" => 0.45,
                    "Q2" => 0.35,
                    "Q3" => 0.25,
                    "Q4" => 0.20,
                    _ => 0.15,
                };
                period_factor * (-0.05 * abs_diff as f64).exp()
            });

        // Adjust for team strength
        let adjusted_prob = base_prob * team_strength_factor;

        // Cap between 0.01 and 0.95
        adjusted_prob.max(0.01).min(0.95)
    }

    /// Calculate expected value of buying underdog
    pub fn calculate_ev(
        &self,
        comeback_prob: f64,
        market_price: f64,
    ) -> f64 {
        // EV = (win_prob * payout) - (loss_prob * stake)
        // payout = 1.0 / market_price - 1.0
        // stake = 1.0

        let payout = 1.0 / market_price - 1.0;
        let ev = (comeback_prob * payout) - ((1.0 - comeback_prob) * 1.0);

        ev
    }
}

/// Arbitrage opportunity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageOpportunity {
    pub game: LiveGameState,
    pub underdog_side: String,
    pub underdog_team: String,
    pub current_price: f64,
    pub predicted_comeback_prob: f64,
    pub expected_value: f64,
    pub edge: f64, // predicted_prob - market_price
    pub score_differential: i32,
    pub time_remaining: String,
    pub confidence: f64,
    pub reasoning: Vec<String>,
}

impl ArbitrageOpportunity {
    /// Check if opportunity meets minimum criteria
    pub fn is_valid(&self, min_edge: f64, min_ev: f64) -> bool {
        self.edge > min_edge && self.expected_value > min_ev
    }
}

/// Live arbitrage monitor
pub struct LiveArbitrageMonitor {
    client: Client,
    comeback_model: ComebackModel,
    price_history: HashMap<String, Vec<(DateTime<Utc>, MoneylinePrices)>>,
    /// Team strength factors (team name -> 0.8-1.2 multiplier)
    team_strength: HashMap<String, f64>,
}

impl LiveArbitrageMonitor {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
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
        for name in ["Boston Celtics", "Denver Nuggets", "Oklahoma City Thunder",
                      "Milwaukee Bucks", "Phoenix Suns"] {
            m.insert(name.to_string(), 1.12);
        }
        // Above average
        for name in ["Cleveland Cavaliers", "Minnesota Timberwolves",
                      "New York Knicks", "Dallas Mavericks", "LA Clippers"] {
            m.insert(name.to_string(), 1.06);
        }
        // Below average
        for name in ["Charlotte Hornets", "Portland Trail Blazers",
                      "San Antonio Spurs", "Utah Jazz", "Detroit Pistons"] {
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
        min_edge: f64,             // e.g., 0.10 (10%)
        interval_secs: u64,        // e.g., 30 seconds
    ) -> Result<()> {
        info!("Starting live arbitrage monitor...");
        info!("Min price deviation: {:.0}%", min_price_deviation * 100.0);
        info!("Min edge: {:.0}%", min_edge * 100.0);
        info!("Update interval: {}s", interval_secs);

        loop {
            match self.scan_for_opportunities(min_price_deviation, min_edge).await {
                Ok(opportunities) => {
                    if !opportunities.is_empty() {
                        info!("\n🚨 Found {} arbitrage opportunities!", opportunities.len());

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
        let url = "https://gamma-api.polymarket.com/series/10345"; // NBA 2026
        let response = self.client.get(url).send().await
            .map_err(|e| PloyError::Internal(format!("Network error: {}", e)))?;

        if !response.status().is_success() {
            return Err(PloyError::Internal("API error".into()));
        }

        let series: serde_json::Value = response.json().await
            .map_err(|e| PloyError::Internal(format!("Parse error: {}", e)))?;

        let events = series.get("events")
            .and_then(|e| e.as_array())
            .ok_or_else(|| PloyError::Internal("No events".into()))?;

        let mut live_games = vec![];

        for event in events {
            let event_id = event.get("id")
                .and_then(|id| id.as_str())
                .unwrap_or("");

            if event_id.is_empty() {
                continue;
            }

            // Fetch event details
            match self.fetch_game_state(event_id).await {
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
        let url = format!("https://gamma-api.polymarket.com/events/{}", event_id);
        let response = self.client.get(&url).send().await
            .map_err(|e| PloyError::Internal(format!("Network error: {}", e)))?;

        if !response.status().is_success() {
            return Ok(None);
        }

        let event: serde_json::Value = response.json().await
            .map_err(|e| PloyError::Internal(format!("Parse error: {}", e)))?;

        let title = event.get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        let slug = event.get("slug")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();

        let live = event.get("live")
            .and_then(|l| l.as_bool())
            .unwrap_or(false);

        let ended = event.get("ended")
            .and_then(|e| e.as_bool())
            .unwrap_or(false);

        let score_str = event.get("score")
            .and_then(|s| s.as_str());

        let score = score_str.and_then(|s| GameScore::from_string(s));

        let period = event.get("period")
            .and_then(|p| p.as_str())
            .map(|s| s.to_string());

        let elapsed = event.get("elapsed")
            .and_then(|e| e.as_str())
            .map(|s| s.to_string());

        // Find moneyline market
        let markets = event.get("markets")
            .and_then(|m| m.as_array())
            .ok_or_else(|| PloyError::Internal("No markets".into()))?;

        let mut moneyline = None;
        let mut team1 = String::new();
        let mut team2 = String::new();

        for market in markets {
            let question = market.get("question")
                .and_then(|q| q.as_str())
                .unwrap_or("");

            // Find main moneyline (not 1H)
            if question.contains(" vs. ") && !question.contains("1H") && !question.contains("O/U") && !question.contains("Spread") {
                let prices_str = market.get("outcomePrices")
                    .and_then(|p| p.as_str())
                    .unwrap_or("[]");
                let prices: Vec<String> = serde_json::from_str(prices_str).unwrap_or_default();

                let outcomes_str = market.get("outcomes")
                    .and_then(|o| o.as_str())
                    .unwrap_or("[]");
                let outcomes: Vec<String> = serde_json::from_str(outcomes_str).unwrap_or_default();

                let volume = market.get("volume")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
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

    /// Analyze game for arbitrage opportunity
    async fn analyze_game(
        &mut self,
        game: &LiveGameState,
        min_edge: f64,
    ) -> Option<ArbitrageOpportunity> {
        // Must have score and period
        let score = game.score.as_ref()?;
        let period = game.period.as_ref()?;

        // Determine underdog
        let (underdog_side, underdog_team, underdog_price, leading_team) = if game.moneyline.team1_price < game.moneyline.team2_price {
            ("team1", &game.team1, game.moneyline.team1_price, &game.team2)
        } else {
            ("team2", &game.team2, game.moneyline.team2_price, &game.team1)
        };

        // Check if underdog is actually losing
        let is_underdog_losing = if underdog_side == "team1" {
            score.differential < 0 // team1 is behind
        } else {
            score.differential > 0 // team2 is behind
        };

        if !is_underdog_losing {
            return None; // Underdog is winning, no opportunity
        }

        let score_diff = score.differential.abs();

        // Predict comeback probability using team-specific strength factor
        let team_strength_factor = self.team_strength_factor(underdog_team);

        let predicted_prob = self.comeback_model.predict_comeback_prob(
            period,
            score_diff,
            team_strength_factor,
        );

        // Calculate EV
        let ev = self.comeback_model.calculate_ev(predicted_prob, underdog_price);

        // Calculate edge
        let edge = predicted_prob - underdog_price;

        // Generate reasoning
        let mut reasoning = vec![];

        reasoning.push(format!(
            "{} is down {} points in {}",
            underdog_team, score_diff, period
        ));

        reasoning.push(format!(
            "Market price: {:.3} ({:.1}% implied)",
            underdog_price, underdog_price * 100.0
        ));

        reasoning.push(format!(
            "Predicted comeback: {:.1}%",
            predicted_prob * 100.0
        ));

        reasoning.push(format!(
            "Edge: {:+.1}%",
            edge * 100.0
        ));

        if ev > 0.0 {
            reasoning.push(format!(
                "Positive EV: {:+.2} per $1 bet",
                ev
            ));
        }

        // Store price history
        self.price_history
            .entry(game.event_id.clone())
            .or_insert_with(Vec::new)
            .push((game.timestamp, game.moneyline.clone()));

        let confidence = if edge > 0.15 && ev > 0.20 {
            0.9
        } else if edge > 0.10 && ev > 0.10 {
            0.7
        } else {
            0.5
        };

        let opp = ArbitrageOpportunity {
            game: game.clone(),
            underdog_side: underdog_side.to_string(),
            underdog_team: underdog_team.clone(),
            current_price: underdog_price,
            predicted_comeback_prob: predicted_prob,
            expected_value: ev,
            edge,
            score_differential: score_diff,
            time_remaining: format!("{} - {}", period, game.elapsed.as_ref().unwrap_or(&"".to_string())),
            confidence,
            reasoning,
        };

        if opp.is_valid(min_edge, 0.0) {
            Some(opp)
        } else {
            None
        }
    }

    /// Print opportunity
    fn print_opportunity(&self, opp: &ArbitrageOpportunity) {
        println!("\n{}", "═".repeat(80));
        println!("🎯 ARBITRAGE OPPORTUNITY");
        println!("{}", "═".repeat(80));

        println!("\nGame: ", opp.game.title);
        println!("Score: {:?}", opp.game.score);
        println!("Period: {}", opp.time_remaining);

        println!("\n💰 Opportunity:");
        println!("  Buy: {} YES", opp.underdog_team);
        println!("  Current Price: {:.3} ({:.1}% implied)",
            opp.current_price, opp.current_price * 100.0);
        println!("  Predicted Prob: {:.1}%", opp.predicted_comeback_prob * 100.0);
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
    pub fn get_price_history(&self, event_id: &str) -> Option<&Vec<(DateTime<Utc>, MoneylinePrices)>> {
        self.price_history.get(event_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_comeback_model() {
        let model = ComebackModel::new();

        // Q3, down 15 points
        let prob = model.predict_comeback_prob("Q3", 15, 1.0);
        assert!(prob > 0.10 && prob < 0.15);

        // Q4, down 5 points
        let prob = model.predict_comeback_prob("Q4", 5, 1.0);
        assert!(prob > 0.25 && prob < 0.35);

        // Strong team factor
        let prob_strong = model.predict_comeback_prob("Q3", 15, 1.2);
        let prob_weak = model.predict_comeback_prob("Q3", 15, 0.8);
        assert!(prob_strong > prob_weak);
    }

    #[test]
    fn test_ev_calculation() {
        let model = ComebackModel::new();

        // Positive EV scenario
        let ev = model.calculate_ev(0.40, 0.20); // 40% prob, 20% price
        assert!(ev > 0.0);

        // Negative EV scenario
        let ev = model.calculate_ev(0.15, 0.20); // 15% prob, 20% price
        assert!(ev < 0.0);
    }

    #[test]
    fn test_game_score_parsing() {
        let score = GameScore::from_string("102-119").unwrap();
        assert_eq!(score.team1_score, 102);
        assert_eq!(score.team2_score, 119);
        assert_eq!(score.differential, -17);
    }
}
