// DraftKings / The Odds API Integration
// Fetches live sports betting odds from multiple sportsbooks

use crate::error::{PloyError, Result};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

const THE_ODDS_API_BASE: &str = "https://api.the-odds-api.com/v4";

/// Supported sports
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sport {
    #[serde(rename = "basketball_nba")]
    NBA,
    #[serde(rename = "americanfootball_nfl")]
    NFL,
    #[serde(rename = "icehockey_nhl")]
    NHL,
    #[serde(rename = "baseball_mlb")]
    MLB,
    #[serde(rename = "basketball_ncaab")]
    NCAAB,
    #[serde(rename = "americanfootball_ncaaf")]
    NCAAF,
}

impl Sport {
    pub fn api_key(&self) -> &'static str {
        match self {
            Sport::NBA => "basketball_nba",
            Sport::NFL => "americanfootball_nfl",
            Sport::NHL => "icehockey_nhl",
            Sport::MLB => "baseball_mlb",
            Sport::NCAAB => "basketball_ncaab",
            Sport::NCAAF => "americanfootball_ncaaf",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Sport::NBA => "NBA",
            Sport::NFL => "NFL",
            Sport::NHL => "NHL",
            Sport::MLB => "MLB",
            Sport::NCAAB => "College Basketball",
            Sport::NCAAF => "College Football",
        }
    }
}

/// Sportsbook identifiers
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sportsbook {
    DraftKings,
    FanDuel,
    BetMGM,
    Caesars,
    PointsBet,
    Bovada,
}

impl Sportsbook {
    pub fn key(&self) -> &str {
        match self {
            Sportsbook::DraftKings => "draftkings",
            Sportsbook::FanDuel => "fanduel",
            Sportsbook::BetMGM => "betmgm",
            Sportsbook::Caesars => "williamhill_us",
            Sportsbook::PointsBet => "pointsbetus",
            Sportsbook::Bovada => "bovada",
        }
    }
}

/// Betting market types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Market {
    #[serde(rename = "h2h")]
    Moneyline,
    #[serde(rename = "spreads")]
    Spread,
    #[serde(rename = "totals")]
    Total,
}

impl Market {
    pub fn api_key(&self) -> &'static str {
        match self {
            Market::Moneyline => "h2h",
            Market::Spread => "spreads",
            Market::Total => "totals",
        }
    }
}

/// Odds from a single outcome
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub name: String,
    pub price: f64,
    #[serde(default)]
    pub point: Option<f64>,
}

impl Outcome {
    /// Convert American odds to decimal odds
    pub fn decimal_odds(&self) -> Decimal {
        let price = self.price;
        if price > 0.0 {
            Decimal::from_f64_retain((price / 100.0) + 1.0).unwrap_or(Decimal::ONE)
        } else {
            Decimal::from_f64_retain((100.0 / price.abs()) + 1.0).unwrap_or(Decimal::ONE)
        }
    }

    /// Convert to implied probability
    pub fn implied_probability(&self) -> Decimal {
        let decimal = self.decimal_odds();
        if decimal > Decimal::ZERO {
            Decimal::ONE / decimal
        } else {
            Decimal::ZERO
        }
    }
}

/// Bookmaker odds for a game
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookmakerOdds {
    pub key: String,
    pub title: String,
    pub markets: Vec<MarketOdds>,
}

/// Market odds (h2h, spreads, totals)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketOdds {
    pub key: String,
    pub outcomes: Vec<Outcome>,
}

/// Game event with odds from multiple bookmakers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameEvent {
    pub id: String,
    pub sport_key: String,
    pub sport_title: String,
    pub commence_time: String,
    pub home_team: String,
    pub away_team: String,
    pub bookmakers: Vec<BookmakerOdds>,
}

impl GameEvent {
    /// Get moneyline odds from a specific bookmaker
    pub fn get_moneyline(&self, bookmaker: &str) -> Option<(Decimal, Decimal)> {
        let bookie = self.bookmakers.iter().find(|b| b.key == bookmaker)?;
        let market = bookie.markets.iter().find(|m| m.key == "h2h")?;

        if market.outcomes.len() >= 2 {
            let home_odds = market
                .outcomes
                .iter()
                .find(|o| o.name == self.home_team)?
                .implied_probability();
            let away_odds = market
                .outcomes
                .iter()
                .find(|o| o.name == self.away_team)?
                .implied_probability();
            Some((home_odds, away_odds))
        } else {
            None
        }
    }

    /// Find best odds across all bookmakers
    pub fn best_odds(&self) -> Option<BestOdds> {
        let mut best_home: Option<(String, Decimal, f64)> = None;
        let mut best_away: Option<(String, Decimal, f64)> = None;

        for bookie in &self.bookmakers {
            if let Some(market) = bookie.markets.iter().find(|m| m.key == "h2h") {
                for outcome in &market.outcomes {
                    let prob = outcome.implied_probability();

                    if outcome.name == self.home_team {
                        if best_home.is_none() || prob < best_home.as_ref().unwrap().1 {
                            best_home = Some((bookie.key.clone(), prob, outcome.price));
                        }
                    } else if outcome.name == self.away_team {
                        if best_away.is_none() || prob < best_away.as_ref().unwrap().1 {
                            best_away = Some((bookie.key.clone(), prob, outcome.price));
                        }
                    }
                }
            }
        }

        match (best_home, best_away) {
            (
                Some((home_book, home_prob, home_american)),
                Some((away_book, away_prob, away_american)),
            ) => Some(BestOdds {
                home_team: self.home_team.clone(),
                away_team: self.away_team.clone(),
                home_bookmaker: home_book,
                away_bookmaker: away_book,
                home_implied_prob: home_prob,
                away_implied_prob: away_prob,
                home_american_odds: home_american,
                away_american_odds: away_american,
                total_implied: home_prob + away_prob,
            }),
            _ => None,
        }
    }
}

/// Best odds comparison across bookmakers
#[derive(Debug, Clone)]
pub struct BestOdds {
    pub home_team: String,
    pub away_team: String,
    pub home_bookmaker: String,
    pub away_bookmaker: String,
    pub home_implied_prob: Decimal,
    pub away_implied_prob: Decimal,
    pub home_american_odds: f64,
    pub away_american_odds: f64,
    pub total_implied: Decimal,
}

impl BestOdds {
    /// Check if arbitrage opportunity exists (total implied < 100%)
    pub fn has_arbitrage(&self) -> bool {
        self.total_implied < Decimal::ONE
    }

    /// Calculate arbitrage profit percentage
    pub fn arbitrage_profit(&self) -> Decimal {
        if self.has_arbitrage() {
            (Decimal::ONE - self.total_implied) * Decimal::from(100)
        } else {
            Decimal::ZERO
        }
    }
}

/// Odds provider configuration
#[derive(Debug, Clone)]
pub struct OddsProviderConfig {
    pub api_key: String,
    pub bookmakers: Vec<String>,
    pub region: String,
}

impl Default for OddsProviderConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            bookmakers: vec![
                "draftkings".to_string(),
                "fanduel".to_string(),
                "betmgm".to_string(),
            ],
            region: "us".to_string(),
        }
    }
}

/// The Odds API client for fetching sports betting odds
pub struct OddsProvider {
    client: Client,
    config: OddsProviderConfig,
}

impl OddsProvider {
    /// Create new odds provider
    pub fn new(config: OddsProviderConfig) -> Result<Self> {
        if config.api_key.is_empty() {
            return Err(PloyError::Internal(
                "THE_ODDS_API_KEY not configured".into(),
            ));
        }

        Ok(Self {
            client: Client::new(),
            config,
        })
    }

    /// Create from environment
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("THE_ODDS_API_KEY")
            .map_err(|_| PloyError::Internal("THE_ODDS_API_KEY not set".into()))?;

        let bookmakers = std::env::var("ODDS_BOOKMAKERS")
            .unwrap_or_else(|_| "draftkings,fanduel,betmgm".to_string())
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();

        let config = OddsProviderConfig {
            api_key,
            bookmakers,
            region: "us".to_string(),
        };

        Self::new(config)
    }

    /// Fetch odds for a specific sport
    pub async fn get_odds(&self, sport: Sport, market: Market) -> Result<Vec<GameEvent>> {
        let bookmakers = self.config.bookmakers.join(",");

        let url = format!("{}/sports/{}/odds", THE_ODDS_API_BASE, sport.api_key());

        debug!("Fetching odds from: {}", url);

        let response = self
            .client
            .get(&url)
            .query(&[
                ("apiKey", self.config.api_key.as_str()),
                ("regions", self.config.region.as_str()),
                ("markets", market.api_key()),
                ("bookmakers", &bookmakers),
                ("oddsFormat", "american"),
            ])
            .send()
            .await
            .map_err(|e| PloyError::Internal(format!("Network error: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(PloyError::Internal(format!(
                "Odds API error {}: {}",
                status, text
            )));
        }

        let events: Vec<GameEvent> = response
            .json()
            .await
            .map_err(|e| PloyError::Internal(format!("Parse error: {}", e)))?;

        info!(
            "Fetched {} {} games with odds",
            events.len(),
            sport.display_name()
        );
        Ok(events)
    }

    /// Get NBA moneyline odds
    pub async fn get_nba_odds(&self) -> Result<Vec<GameEvent>> {
        self.get_odds(Sport::NBA, Market::Moneyline).await
    }

    /// Get NFL moneyline odds
    pub async fn get_nfl_odds(&self) -> Result<Vec<GameEvent>> {
        self.get_odds(Sport::NFL, Market::Moneyline).await
    }

    /// Find arbitrage opportunities
    pub async fn find_arbitrage(&self, sport: Sport) -> Result<Vec<(GameEvent, BestOdds)>> {
        let events = self.get_odds(sport, Market::Moneyline).await?;

        let mut opportunities = Vec::new();

        for event in events {
            if let Some(best) = event.best_odds() {
                if best.has_arbitrage() {
                    info!(
                        "Arbitrage found: {} vs {} - {:.2}% profit",
                        best.home_team,
                        best.away_team,
                        best.arbitrage_profit()
                    );
                    opportunities.push((event, best));
                }
            }
        }

        Ok(opportunities)
    }

    /// Compare DraftKings odds with Polymarket predictions
    pub async fn compare_with_prediction(
        &self,
        sport: Sport,
        home_team: &str,
        away_team: &str,
        predicted_home_prob: Decimal,
    ) -> Result<Option<EdgeAnalysis>> {
        let events = self.get_odds(sport, Market::Moneyline).await?;

        // Find matching game
        let event = events.iter().find(|e| {
            (e.home_team
                .to_lowercase()
                .contains(&home_team.to_lowercase())
                || home_team
                    .to_lowercase()
                    .contains(&e.home_team.to_lowercase()))
                && (e
                    .away_team
                    .to_lowercase()
                    .contains(&away_team.to_lowercase())
                    || away_team
                        .to_lowercase()
                        .contains(&e.away_team.to_lowercase()))
        });

        let event = match event {
            Some(e) => e,
            None => {
                warn!("No matching game found for {} vs {}", home_team, away_team);
                return Ok(None);
            }
        };

        // Get DraftKings odds
        let dk_odds = event.get_moneyline("draftkings");

        if let Some((dk_home_prob, dk_away_prob)) = dk_odds {
            let predicted_away_prob = Decimal::ONE - predicted_home_prob;

            let home_edge = predicted_home_prob - dk_home_prob;
            let away_edge = predicted_away_prob - dk_away_prob;

            let analysis = EdgeAnalysis {
                game: format!("{} vs {}", event.home_team, event.away_team),
                home_team: event.home_team.clone(),
                away_team: event.away_team.clone(),
                dk_home_prob,
                dk_away_prob,
                predicted_home_prob,
                predicted_away_prob,
                home_edge,
                away_edge,
                recommended_side: if home_edge > away_edge {
                    event.home_team.clone()
                } else {
                    event.away_team.clone()
                },
                edge: home_edge.max(away_edge),
            };

            Ok(Some(analysis))
        } else {
            warn!("DraftKings odds not available for this game");
            Ok(None)
        }
    }
}

/// Edge analysis comparing prediction vs sportsbook odds
#[derive(Debug, Clone)]
pub struct EdgeAnalysis {
    pub game: String,
    pub home_team: String,
    pub away_team: String,
    pub dk_home_prob: Decimal,
    pub dk_away_prob: Decimal,
    pub predicted_home_prob: Decimal,
    pub predicted_away_prob: Decimal,
    pub home_edge: Decimal,
    pub away_edge: Decimal,
    pub recommended_side: String,
    pub edge: Decimal,
}

impl EdgeAnalysis {
    /// Check if edge is significant (> 5%)
    pub fn is_significant(&self) -> bool {
        self.edge > Decimal::from_str_exact("0.05").unwrap()
    }

    /// Calculate Kelly criterion bet size
    pub fn kelly_fraction(&self) -> Decimal {
        if self.edge <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        // Simplified Kelly: edge / odds
        let odds = if self.home_edge > self.away_edge {
            Decimal::ONE / self.dk_home_prob - Decimal::ONE
        } else {
            Decimal::ONE / self.dk_away_prob - Decimal::ONE
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
    fn test_american_to_decimal_positive() {
        let outcome = Outcome {
            name: "Lakers".to_string(),
            price: 150.0,
            point: None,
        };
        let decimal = outcome.decimal_odds();
        assert!(decimal > Decimal::from(2));
        assert!(decimal < Decimal::from(3));
    }

    #[test]
    fn test_american_to_decimal_negative() {
        let outcome = Outcome {
            name: "Celtics".to_string(),
            price: -150.0,
            point: None,
        };
        let decimal = outcome.decimal_odds();
        assert!(decimal > Decimal::ONE);
        assert!(decimal < Decimal::from(2));
    }

    #[test]
    fn test_implied_probability() {
        let outcome = Outcome {
            name: "Team".to_string(),
            price: -200.0, // 66.67% implied
            point: None,
        };
        let prob = outcome.implied_probability();
        assert!(prob > Decimal::from_str_exact("0.6").unwrap());
        assert!(prob < Decimal::from_str_exact("0.7").unwrap());
    }
}
