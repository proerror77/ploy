//! Polymarket NBA Moneyline Analyzer
//!
//! Analyzes NBA moneyline markets on Polymarket to find:
//! - Best value opportunities
//! - Market inefficiencies
//! - Volume and liquidity analysis
//! - Comparison with sportsbook odds

use crate::adapters::polymarket_clob::GAMMA_API_URL;
use crate::error::{PloyError, Result};
use polymarket_client_sdk::gamma::types::request::{EventByIdRequest, SeriesByIdRequest};
use polymarket_client_sdk::gamma::Client as GammaClient;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info, warn};

/// NBA Moneyline market on Polymarket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NBAMoneylineMarket {
    pub event_id: String,
    pub event_title: String,
    pub event_slug: String,
    pub team1: String,
    pub team2: String,
    pub team1_price: Decimal,
    pub team2_price: Decimal,
    pub team1_implied_prob: f64,
    pub team2_implied_prob: f64,
    pub volume: f64,
    pub token_ids: (String, String),
    pub all_markets: Vec<MarketSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSummary {
    pub market_type: String,
    pub question: String,
    pub volume: f64,
}

/// Market analysis result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoneylineAnalysis {
    pub market: NBAMoneylineMarket,
    pub value_score: f64,
    pub liquidity_score: f64,
    pub market_efficiency: f64,
    pub recommended_side: Option<String>,
    pub edge: Option<f64>,
    pub insights: Vec<String>,
}

/// Polymarket NBA Moneyline Analyzer
pub struct NBAMoneylineAnalyzer {
    gamma_client: GammaClient,
}

impl NBAMoneylineAnalyzer {
    pub fn new() -> Result<Self> {
        Ok(Self {
            gamma_client: GammaClient::new(GAMMA_API_URL)
                .map_err(|e| PloyError::Internal(format!("Gamma client error: {}", e)))?,
        })
    }

    /// Fetch all active NBA moneyline markets
    pub async fn fetch_nba_moneylines(&self) -> Result<Vec<NBAMoneylineMarket>> {
        info!("Fetching NBA moneyline markets from Polymarket...");

        // Fetch NBA series events
        let req = SeriesByIdRequest::builder().id("10345").build(); // NBA 2026 series
        let series = self
            .gamma_client
            .series_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Gamma series fetch failed: {}", e)))?;
        let events = series.events.unwrap_or_default();

        let mut moneylines = vec![];

        // Fetch details for each event
        for event in events.iter().take(20) { // Limit to 20 events
            let event_id = event.id.as_str();
            if event_id.is_empty() {
                continue;
            }

            match self.fetch_event_moneyline(event_id).await {
                Ok(Some(ml)) => {
                    info!("✓ Found moneyline: {} vs {} (${:.0} volume)",
                        ml.team1, ml.team2, ml.volume);
                    moneylines.push(ml);
                }
                Ok(None) => {
                    debug!("No moneyline found for event {}", event_id);
                }
                Err(e) => {
                    warn!("Failed to fetch event {}: {}", event_id, e);
                }
            }
        }

        info!("Found {} NBA moneyline markets", moneylines.len());
        Ok(moneylines)
    }

    /// Fetch moneyline for a specific event
    async fn fetch_event_moneyline(&self, event_id: &str) -> Result<Option<NBAMoneylineMarket>> {
        let req = EventByIdRequest::builder().id(event_id).build();
        let event = match self.gamma_client.event_by_id(&req).await {
            Ok(event) => event,
            Err(_) => return Ok(None),
        };

        let title = event.title.unwrap_or_default();
        let slug = event.slug.unwrap_or_default();

        let Some(markets) = event.markets.as_ref() else {
            return Ok(None);
        };

        // Find moneyline market
        let mut moneyline_market = None;
        let mut all_markets = vec![];

        for market in markets {
            let question = market.question.as_deref().unwrap_or("");

            let volume = market
                .volume
                .as_deref()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);

            // Classify market type
            let market_type = if question.contains("1H") || question.contains("First Half") {
                if question.contains("Moneyline") {
                    "1H Moneyline"
                } else if question.contains("Spread") {
                    "1H Spread"
                } else {
                    "1H O/U"
                }
            } else if question.contains("Spread:") {
                "Spread"
            } else if question.contains("O/U") {
                "O/U"
            } else if question.contains(" vs. ") {
                "Moneyline"
            } else {
                "Other"
            };

            all_markets.push(MarketSummary {
                market_type: market_type.to_string(),
                question: question.to_string(),
                volume,
            });

            // Extract moneyline
            if market_type == "Moneyline" {
                let prices = self.parse_json_array_strings(market.outcome_prices.as_deref());
                let outcomes = self.parse_json_array_strings(market.outcomes.as_deref());
                let token_ids = self.parse_json_array_strings(market.clob_token_ids.as_deref());

                if prices.len() >= 2 && outcomes.len() >= 2 && token_ids.len() >= 2 {
                    let team1_price = prices[0].parse::<f64>().unwrap_or(0.5);
                    let team2_price = prices[1].parse::<f64>().unwrap_or(0.5);

                    moneyline_market = Some(NBAMoneylineMarket {
                        event_id: event_id.to_string(),
                        event_title: title.clone(),
                        event_slug: slug.clone(),
                        team1: outcomes[0].clone(),
                        team2: outcomes[1].clone(),
                        team1_price: Decimal::from_f64_retain(team1_price)
                            .unwrap_or(Decimal::new(50, 2)),
                        team2_price: Decimal::from_f64_retain(team2_price)
                            .unwrap_or(Decimal::new(50, 2)),
                        team1_implied_prob: team1_price,
                        team2_implied_prob: team2_price,
                        volume,
                        token_ids: (token_ids[0].clone(), token_ids[1].clone()),
                        all_markets: all_markets.clone(),
                    });
                }
            }
        }

        Ok(moneyline_market)
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

    /// Analyze a moneyline market
    pub fn analyze_market(&self, market: &NBAMoneylineMarket) -> MoneylineAnalysis {
        let mut insights = vec![];

        // Calculate value score (0-1)
        // Higher score = better value (prices closer to 50/50)
        let price_diff = (market.team1_implied_prob - 0.5).abs();
        let value_score = 1.0 - (price_diff * 2.0).min(1.0);

        // Calculate liquidity score (0-1)
        // Based on volume (log scale)
        let liquidity_score = if market.volume > 0.0 {
            (market.volume.ln() / 15.0).min(1.0).max(0.0)
        } else {
            0.0
        };

        // Calculate market efficiency (0-1)
        // Check if prices sum to ~1.0 (efficient market)
        let price_sum = market.team1_implied_prob + market.team2_implied_prob;
        let market_efficiency = 1.0 - (price_sum - 1.0).abs();

        // Generate insights
        if market.volume > 100000.0 {
            insights.push(format!("High volume market (${:.0})", market.volume));
        } else if market.volume < 10000.0 {
            insights.push(format!("Low volume market (${:.0}) - be cautious", market.volume));
        }

        if price_diff < 0.1 {
            insights.push("Competitive matchup (close odds)".to_string());
        } else if price_diff > 0.3 {
            insights.push("Heavy favorite detected".to_string());
        }

        if market_efficiency < 0.95 {
            insights.push(format!(
                "Market inefficiency detected (prices sum to {:.3})",
                price_sum
            ));
        }

        // Check for value
        let (recommended_side, edge) = if value_score > 0.7 && liquidity_score > 0.5 {
            // Recommend underdog if close odds
            if market.team1_implied_prob < market.team2_implied_prob {
                (Some(market.team1.clone()), Some((0.5 - market.team1_implied_prob) * 100.0))
            } else {
                (Some(market.team2.clone()), Some((0.5 - market.team2_implied_prob) * 100.0))
            }
        } else {
            (None, None)
        };

        // Market composition insights
        let total_markets = market.all_markets.len();
        let total_volume: f64 = market.all_markets.iter().map(|m| m.volume).sum();
        insights.push(format!(
            "{} total markets available (${:.0} combined volume)",
            total_markets, total_volume
        ));

        MoneylineAnalysis {
            market: market.clone(),
            value_score,
            liquidity_score,
            market_efficiency,
            recommended_side,
            edge,
            insights,
        }
    }

    /// Find best value opportunities
    pub fn find_best_opportunities(
        &self,
        markets: &[NBAMoneylineMarket],
        min_volume: f64,
    ) -> Vec<MoneylineAnalysis> {
        let mut analyses: Vec<MoneylineAnalysis> = markets.iter()
            .filter(|m| m.volume >= min_volume)
            .map(|m| self.analyze_market(m))
            .collect();

        // Sort by combined score
        analyses.sort_by(|a, b| {
            let score_a = a.value_score * 0.5 + a.liquidity_score * 0.5;
            let score_b = b.value_score * 0.5 + b.liquidity_score * 0.5;
            score_b.partial_cmp(&score_a).unwrap_or(std::cmp::Ordering::Equal)
        });

        analyses
    }

    /// Generate market report
    pub fn generate_report(&self, analyses: &[MoneylineAnalysis]) -> String {
        let mut report = String::new();

        report.push_str("\n");
        report.push_str(&"═".repeat(80));
        report.push_str("\n");
        report.push_str("  POLYMARKET NBA MONEYLINE ANALYSIS\n");
        report.push_str(&"═".repeat(80));
        report.push_str("\n\n");

        report.push_str(&format!("Total Markets Analyzed: {}\n\n", analyses.len()));

        for (i, analysis) in analyses.iter().enumerate() {
            let market = &analysis.market;

            report.push_str(&format!("{}. {} vs \n", i + 1, market.team1, market.team2));
            report.push_str(&format!("   Event: {}\n", market.event_title));
            report.push_str(&format!("   Slug: {}\n", market.event_slug));
            report.push_str("\n");

            report.push_str("   Moneyline Odds:\n");
            report.push_str(&format!("   • {}: {:.3} ({:.1}%)\n",
                market.team1, market.team1_price, market.team1_implied_prob * 100.0));
            report.push_str(&format!("   • {}: {:.3} ({:.1}%)\n",
                market.team2, market.team2_price, market.team2_implied_prob * 100.0));
            report.push_str(&format!("   • Volume: ${:.0}\n", market.volume));
            report.push_str("\n");

            report.push_str("   Scores:\n");
            report.push_str(&format!("   • Value: {:.2}/1.0\n", analysis.value_score));
            report.push_str(&format!("   • Liquidity: {:.2}/1.0\n", analysis.liquidity_score));
            report.push_str(&format!("   • Efficiency: {:.2}/1.0\n", analysis.market_efficiency));
            report.push_str("\n");

            if let Some(ref side) = analysis.recommended_side {
                report.push_str(&format!("   ✓ Recommended: {} ", side));
                if let Some(edge) = analysis.edge {
                    report.push_str(&format!("(Edge: {:+.1}%)", edge));
                }
                report.push_str("\n\n");
            }

            report.push_str("   Insights:\n");
            for insight in &analysis.insights {
                report.push_str(&format!("   • {}\n", insight));
            }

            report.push_str("\n");
            report.push_str(&"-".repeat(80));
            report.push_str("\n\n");
        }

        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fetch_nba_moneylines() {
        let analyzer = NBAMoneylineAnalyzer::new().unwrap();
        let markets = analyzer.fetch_nba_moneylines().await;

        match markets {
            Ok(markets) => {
                println!("Found {} markets", markets.len());
                for market in markets.iter().take(3) {
                    println!("{} vs {}: {:.3} / {:.3}",
                        market.team1, market.team2,
                        market.team1_price, market.team2_price);
                }
            }
            Err(e) => {
                println!("Error: {}", e);
            }
        }
    }

    #[test]
    fn test_market_analysis() {
        let market = NBAMoneylineMarket {
            event_id: "test".to_string(),
            event_title: "Test Game".to_string(),
            event_slug: "test-game".to_string(),
            team1: "Team A".to_string(),
            team2: "Team B".to_string(),
            team1_price: Decimal::new(45, 2),
            team2_price: Decimal::new(55, 2),
            team1_implied_prob: 0.45,
            team2_implied_prob: 0.55,
            volume: 50000.0,
            token_ids: ("token1".to_string(), "token2".to_string()),
            all_markets: vec![],
        };

        let analyzer = NBAMoneylineAnalyzer::new().unwrap();
        let analysis = analyzer.analyze_market(&market);

        assert!(analysis.value_score > 0.0);
        assert!(analysis.liquidity_score > 0.0);
        assert!(analysis.market_efficiency > 0.0);
    }
}
