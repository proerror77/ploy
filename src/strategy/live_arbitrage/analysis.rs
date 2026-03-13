use super::{LiveGameState, MoneylinePrices};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Comeback probability model.
#[derive(Debug, Clone)]
pub struct ComebackModel {
    /// Historical comeback rates by score differential and time.
    comeback_rates: HashMap<String, f64>,
}

impl ComebackModel {
    pub fn new() -> Self {
        let mut comeback_rates = HashMap::new();

        comeback_rates.insert("Q1_5".to_string(), 0.45);
        comeback_rates.insert("Q1_10".to_string(), 0.35);
        comeback_rates.insert("Q1_15".to_string(), 0.20);

        comeback_rates.insert("Q2_5".to_string(), 0.40);
        comeback_rates.insert("Q2_10".to_string(), 0.28);
        comeback_rates.insert("Q2_15".to_string(), 0.15);
        comeback_rates.insert("Q2_20".to_string(), 0.08);

        comeback_rates.insert("Q3_5".to_string(), 0.35);
        comeback_rates.insert("Q3_10".to_string(), 0.22);
        comeback_rates.insert("Q3_15".to_string(), 0.12);
        comeback_rates.insert("Q3_20".to_string(), 0.05);

        comeback_rates.insert("Q4_5".to_string(), 0.30);
        comeback_rates.insert("Q4_10".to_string(), 0.15);
        comeback_rates.insert("Q4_15".to_string(), 0.08);
        comeback_rates.insert("Q4_20".to_string(), 0.03);

        Self { comeback_rates }
    }

    /// Predict comeback probability.
    pub fn predict_comeback_prob(
        &self,
        period: &str,
        score_diff: i32,
        team_strength_factor: f64,
    ) -> f64 {
        let abs_diff = score_diff.abs();
        let rounded_diff = ((abs_diff + 2) / 5) * 5;
        let key = format!("{}_{}", period, rounded_diff);

        let base_prob = self.comeback_rates.get(&key).copied().unwrap_or_else(|| {
            let period_factor = match period {
                "Q1" => 0.45,
                "Q2" => 0.35,
                "Q3" => 0.25,
                "Q4" => 0.20,
                _ => 0.15,
            };
            period_factor * (-0.05 * abs_diff as f64).exp()
        });

        (base_prob * team_strength_factor).clamp(0.01, 0.95)
    }

    /// Calculate expected value of buying underdog.
    pub fn calculate_ev(&self, comeback_prob: f64, market_price: f64) -> f64 {
        let payout = 1.0 / market_price - 1.0;
        (comeback_prob * payout) - ((1.0 - comeback_prob) * 1.0)
    }
}

/// Arbitrage opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageOpportunity {
    pub game: LiveGameState,
    pub underdog_side: String,
    pub underdog_team: String,
    pub current_price: f64,
    pub predicted_comeback_prob: f64,
    pub expected_value: f64,
    pub edge: f64,
    pub score_differential: i32,
    pub time_remaining: String,
    pub confidence: f64,
    pub reasoning: Vec<String>,
}

impl ArbitrageOpportunity {
    /// Check if opportunity meets minimum criteria.
    pub fn is_valid(&self, min_edge: f64, min_ev: f64) -> bool {
        self.edge > min_edge && self.expected_value > min_ev
    }
}

pub fn analyze_game_for_opportunity(
    comeback_model: &ComebackModel,
    game: &LiveGameState,
    min_edge: f64,
    team_strength_factor: f64,
    price_history: &mut HashMap<String, Vec<(DateTime<Utc>, MoneylinePrices)>>,
) -> Option<ArbitrageOpportunity> {
    let score = game.score.as_ref()?;
    let period = game.period.as_ref()?;

    let (underdog_side, underdog_team, underdog_price) =
        if game.moneyline.team1_price < game.moneyline.team2_price {
            ("team1", &game.team1, game.moneyline.team1_price)
        } else {
            ("team2", &game.team2, game.moneyline.team2_price)
        };

    let is_underdog_losing = if underdog_side == "team1" {
        score.differential < 0
    } else {
        score.differential > 0
    };

    if !is_underdog_losing {
        return None;
    }

    let score_diff = score.differential.abs();
    let predicted_prob =
        comeback_model.predict_comeback_prob(period, score_diff, team_strength_factor);
    let ev = comeback_model.calculate_ev(predicted_prob, underdog_price);
    let edge = predicted_prob - underdog_price;

    let mut reasoning = vec![
        format!(
            "{} is down {} points in {}",
            underdog_team, score_diff, period
        ),
        format!(
            "Market price: {:.3} ({:.1}% implied)",
            underdog_price,
            underdog_price * 100.0
        ),
        format!("Predicted comeback: {:.1}%", predicted_prob * 100.0),
        format!("Edge: {:+.1}%", edge * 100.0),
    ];

    if ev > 0.0 {
        reasoning.push(format!("Positive EV: {:+.2} per $1 bet", ev));
    }

    price_history
        .entry(game.event_id.clone())
        .or_default()
        .push((game.timestamp, game.moneyline.clone()));

    let confidence = if edge > 0.15 && ev > 0.20 {
        0.9
    } else if edge > 0.10 && ev > 0.10 {
        0.7
    } else {
        0.5
    };

    let opportunity = ArbitrageOpportunity {
        game: game.clone(),
        underdog_side: underdog_side.to_string(),
        underdog_team: underdog_team.clone(),
        current_price: underdog_price,
        predicted_comeback_prob: predicted_prob,
        expected_value: ev,
        edge,
        score_differential: score_diff,
        time_remaining: format!(
            "{} - {}",
            period,
            game.elapsed.as_ref().unwrap_or(&String::new())
        ),
        confidence,
        reasoning,
    };

    opportunity.is_valid(min_edge, 0.0).then_some(opportunity)
}

#[cfg(test)]
mod tests {
    use super::ComebackModel;

    #[test]
    fn test_comeback_model() {
        let model = ComebackModel::new();

        let prob = model.predict_comeback_prob("Q3", 15, 1.0);
        assert!(prob > 0.10 && prob < 0.15);

        let prob = model.predict_comeback_prob("Q4", 5, 1.0);
        assert!(prob > 0.25 && prob < 0.35);

        let prob_strong = model.predict_comeback_prob("Q3", 15, 1.2);
        let prob_weak = model.predict_comeback_prob("Q3", 15, 0.8);
        assert!(prob_strong > prob_weak);
    }

    #[test]
    fn test_ev_calculation() {
        let model = ComebackModel::new();

        let ev = model.calculate_ev(0.40, 0.20);
        assert!(ev > 0.0);

        let ev = model.calculate_ev(0.15, 0.20);
        assert!(ev < 0.0);
    }
}
