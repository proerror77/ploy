use rust_decimal::Decimal;
use tracing::{debug, info};

use crate::strategy::nba_comeback::espn::LiveGame;

use super::{
    GrokGameIntel, GrokSignalType, GrokTradeSignal, InjuryImpact, InjuryUpdate, MomentumDirection,
};

pub struct GrokSignalEvaluator;

impl GrokSignalEvaluator {
    /// Evaluate Grok intel for tradeable signals.
    ///
    /// `trailing_abbrev`: the team currently trailing in the game
    /// `market_price`: current Polymarket price for the trailing team's YES token
    /// `min_edge`: minimum edge required to generate a signal
    /// `min_confidence`: minimum Grok confidence required to act
    pub fn evaluate(
        intel: &GrokGameIntel,
        game: &LiveGame,
        trailing_abbrev: &str,
        market_price: Decimal,
        min_edge: f64,
        min_confidence: f64,
    ) -> Option<GrokTradeSignal> {
        let market_price_f64 = market_price.to_string().parse::<f64>().unwrap_or(1.0);

        if intel.grok_confidence < min_confidence {
            debug!(
                game_id = %intel.game_id,
                confidence = intel.grok_confidence,
                min_confidence,
                "grok confidence below threshold"
            );
            return None;
        }

        let trailing_is_home = game.home_abbrev == trailing_abbrev;

        let injury_signal = Self::evaluate_injury_edge(
            intel,
            trailing_abbrev,
            trailing_is_home,
            market_price_f64,
            min_edge,
        );
        if injury_signal.is_some() {
            return injury_signal;
        }

        let momentum_signal = Self::evaluate_momentum_edge(
            intel,
            trailing_abbrev,
            trailing_is_home,
            market_price_f64,
            min_edge,
        );
        if momentum_signal.is_some() {
            return momentum_signal;
        }

        let fair_value_signal = Self::evaluate_fair_value_edge(
            intel,
            trailing_abbrev,
            trailing_is_home,
            market_price_f64,
            min_edge,
        );
        if fair_value_signal.is_some() {
            return fair_value_signal;
        }

        None
    }

    /// Signal 1: High-impact injury on the LEADING team creates edge for trailing team
    fn evaluate_injury_edge(
        intel: &GrokGameIntel,
        trailing_abbrev: &str,
        trailing_is_home: bool,
        market_price: f64,
        min_edge: f64,
    ) -> Option<GrokTradeSignal> {
        let leading_injuries: Vec<&InjuryUpdate> = intel
            .injury_updates
            .iter()
            .filter(|inj| {
                inj.team_abbrev != trailing_abbrev
                    && inj.impact == InjuryImpact::High
                    && inj.status == "OUT"
            })
            .collect();

        if leading_injuries.is_empty() {
            return None;
        }

        let injury_boost = (leading_injuries.len() as f64 * 0.06).min(0.15);
        let base_prob = if trailing_is_home {
            intel.grok_home_win_prob.unwrap_or(market_price)
        } else {
            intel
                .grok_home_win_prob
                .map(|p| 1.0 - p)
                .unwrap_or(market_price)
        };
        let fair_value = (base_prob + injury_boost).min(0.95);
        let edge = fair_value - market_price;

        if edge < min_edge {
            return None;
        }

        let player_names: Vec<&str> = leading_injuries
            .iter()
            .map(|i| i.player_name.as_str())
            .collect();
        info!(
            game_id = %intel.game_id,
            trailing = trailing_abbrev,
            edge = format!("{:.3}", edge),
            fair_value = format!("{:.3}", fair_value),
            injured = ?player_names,
            "grok injury edge signal"
        );

        Some(GrokTradeSignal {
            signal_type: GrokSignalType::InjuryEdge,
            target_team_abbrev: trailing_abbrev.to_string(),
            estimated_fair_value: fair_value,
            market_price: Decimal::from_f64_retain(market_price).unwrap_or(Decimal::ONE),
            edge,
            confidence: intel.grok_confidence,
            reasoning: format!(
                "High-impact injury on opposing team ({}). Fair value {:.1}% vs market {:.1}%",
                player_names.join(", "),
                fair_value * 100.0,
                market_price * 100.0
            ),
        })
    }

    /// Signal 2: Momentum surge toward the trailing team + positive sentiment
    fn evaluate_momentum_edge(
        intel: &GrokGameIntel,
        trailing_abbrev: &str,
        trailing_is_home: bool,
        market_price: f64,
        min_edge: f64,
    ) -> Option<GrokTradeSignal> {
        let momentum_favors_trailing = match intel.momentum_direction {
            MomentumDirection::HomeTeamSurge => trailing_is_home,
            MomentumDirection::AwayTeamSurge => !trailing_is_home,
            MomentumDirection::Neutral => return None,
        };

        if !momentum_favors_trailing {
            return None;
        }

        let trailing_sentiment = if trailing_is_home {
            intel.home_sentiment_score
        } else {
            intel.away_sentiment_score
        };

        if trailing_sentiment < 0.2 {
            return None;
        }

        let sentiment_boost = trailing_sentiment * 0.04;
        let momentum_boost = 0.03;
        let base_prob = if trailing_is_home {
            intel.grok_home_win_prob.unwrap_or(market_price)
        } else {
            intel
                .grok_home_win_prob
                .map(|p| 1.0 - p)
                .unwrap_or(market_price)
        };
        let fair_value = (base_prob + sentiment_boost + momentum_boost).min(0.95);
        let edge = fair_value - market_price;

        if edge < min_edge {
            return None;
        }

        info!(
            game_id = %intel.game_id,
            trailing = trailing_abbrev,
            edge = format!("{:.3}", edge),
            sentiment = format!("{:.2}", trailing_sentiment),
            "grok momentum edge signal"
        );

        Some(GrokTradeSignal {
            signal_type: GrokSignalType::MomentumEdge,
            target_team_abbrev: trailing_abbrev.to_string(),
            estimated_fair_value: fair_value,
            market_price: Decimal::from_f64_retain(market_price).unwrap_or(Decimal::ONE),
            edge,
            confidence: intel.grok_confidence * (0.5 + trailing_sentiment * 0.5),
            reasoning: format!(
                "Momentum surge + positive sentiment ({:.0}%) for {}. Fair value {:.1}% vs market {:.1}%",
                trailing_sentiment * 100.0,
                trailing_abbrev,
                fair_value * 100.0,
                market_price * 100.0
            ),
        })
    }

    /// Signal 3: Grok's fair probability significantly diverges from market price
    fn evaluate_fair_value_edge(
        intel: &GrokGameIntel,
        trailing_abbrev: &str,
        trailing_is_home: bool,
        market_price: f64,
        min_edge: f64,
    ) -> Option<GrokTradeSignal> {
        let grok_home_prob = intel.grok_home_win_prob?;

        let grok_trailing_prob = if trailing_is_home {
            grok_home_prob
        } else {
            1.0 - grok_home_prob
        };

        let edge = grok_trailing_prob - market_price;

        if edge < min_edge {
            return None;
        }

        info!(
            game_id = %intel.game_id,
            trailing = trailing_abbrev,
            grok_prob = format!("{:.3}", grok_trailing_prob),
            market_price = format!("{:.3}", market_price),
            edge = format!("{:.3}", edge),
            "grok fair value edge signal"
        );

        Some(GrokTradeSignal {
            signal_type: GrokSignalType::FairValueEdge,
            target_team_abbrev: trailing_abbrev.to_string(),
            estimated_fair_value: grok_trailing_prob,
            market_price: Decimal::from_f64_retain(market_price).unwrap_or(Decimal::ONE),
            edge,
            confidence: intel.grok_confidence,
            reasoning: format!(
                "Grok estimates {:.1}% fair value for {} vs market {:.1}%",
                grok_trailing_prob * 100.0,
                trailing_abbrev,
                market_price * 100.0
            ),
        })
    }
}
