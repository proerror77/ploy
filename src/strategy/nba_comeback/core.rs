//! NBA Comeback Core Logic
//!
//! Implements the scan→filter→score→decide pipeline:
//! 1. Fetch live games from ESPN
//! 2. Filter to Q3 games with trailing teams
//! 3. Look up historical comeback rates
//! 4. Calculate adjusted win probability
//! 5. Compare against Polymarket price to find edge
//! 6. Emit trade decisions for opportunities above threshold

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use tracing::{debug, info, warn};

use crate::config::NbaComebackConfig;
use crate::strategy::nba_comeback::comeback_stats::ComebackStatsProvider;
use crate::strategy::nba_comeback::espn::{EspnClient, LiveGame};
use crate::strategy::nba_winprob::{GameFeatures, LiveWinProbModel};

/// A single actionable comeback opportunity
#[derive(Debug, Clone)]
pub struct ComebackOpportunity {
    pub game: LiveGame,
    pub trailing_team: String,
    pub trailing_abbrev: String,
    pub deficit: i32,
    pub comeback_rate: f64,
    pub adjusted_win_prob: f64,
    pub market_price: Decimal,
    pub edge: f64,
    pub market_slug: String,
    pub token_id: String,
}

/// Mutable state across scan cycles
#[derive(Debug, Clone)]
pub struct NbaComebackState {
    pub traded_games: HashMap<String, DateTime<Utc>>,
    pub daily_spend_usd: Decimal,
    pub daily_spend_day: NaiveDate,
}

impl Default for NbaComebackState {
    fn default() -> Self {
        Self {
            traded_games: HashMap::new(),
            daily_spend_usd: Decimal::ZERO,
            daily_spend_day: Utc::now().date_naive(),
        }
    }
}

/// Core scan→filter→decide logic for NBA comeback trading
pub struct NbaComebackCore {
    pub espn: EspnClient,
    pub stats: ComebackStatsProvider,
    pub winprob_model: LiveWinProbModel,
    pub cfg: NbaComebackConfig,
    pub state: NbaComebackState,
}

impl NbaComebackCore {
    pub fn new(espn: EspnClient, stats: ComebackStatsProvider, cfg: NbaComebackConfig) -> Self {
        Self {
            espn,
            stats,
            winprob_model: LiveWinProbModel::default_untrained(),
            cfg,
            state: NbaComebackState::default(),
        }
    }

    // ── Guards (same pattern as EventEdgeCore) ──────────────────

    pub fn reset_daily_if_needed(&mut self) {
        let today = Utc::now().date_naive();
        if today != self.state.daily_spend_day {
            self.state.daily_spend_day = today;
            self.state.daily_spend_usd = Decimal::ZERO;
            info!("NBA comeback: daily spend reset");
        }
    }

    pub fn is_on_cooldown(&self, game_id: &str) -> bool {
        if let Some(last) = self.state.traded_games.get(game_id) {
            let elapsed = (Utc::now() - *last).num_seconds();
            elapsed < self.cfg.cooldown_secs as i64
        } else {
            false
        }
    }

    pub fn can_spend(&self, amount: Decimal) -> bool {
        self.state.daily_spend_usd + amount <= self.cfg.max_daily_spend_usd
    }

    pub fn record_trade(&mut self, game_id: &str, spend: Decimal) {
        self.state
            .traded_games
            .insert(game_id.to_string(), Utc::now());
        self.state.daily_spend_usd += spend;
    }

    // ── Scan cycle ──────────────────────────────────────────────

    fn scan_games_inner(&mut self, games: &[LiveGame]) -> Vec<ComebackCandidate> {
        let q3_games = EspnClient::games_in_quarter(games, self.cfg.target_quarter);
        debug!(
            "ESPN: {} total games, {} in Q{}",
            games.len(),
            q3_games.len(),
            self.cfg.target_quarter
        );

        let mut candidates = Vec::new();

        for game in q3_games {
            // Skip if on cooldown
            if self.is_on_cooldown(&game.espn_game_id) {
                debug!("Skipping {} (cooldown)", game.espn_game_id);
                continue;
            }

            // Identify trailing team
            let (trail_name, trail_abbrev, deficit) = match game.trailing_team() {
                Some(t) => t,
                None => continue, // Tied — skip
            };

            // Check deficit bounds
            if deficit < self.cfg.min_deficit || deficit > self.cfg.max_deficit {
                debug!(
                    "Skipping {} deficit={} (bounds {}-{})",
                    trail_abbrev, deficit, self.cfg.min_deficit, self.cfg.max_deficit
                );
                continue;
            }

            // Look up comeback rate
            let comeback_rate = match self.stats.comeback_rate_for_deficit(&trail_abbrev, deficit) {
                Some(r) => r,
                None => continue,
            };

            // Check minimum comeback rate
            if comeback_rate < self.cfg.min_comeback_rate {
                debug!(
                    "Skipping {} comeback_rate={:.3} < min {:.3}",
                    trail_abbrev, comeback_rate, self.cfg.min_comeback_rate
                );
                continue;
            }

            // Calculate adjusted win probability using the model
            let elo_diff = self
                .stats
                .get_profile(&trail_abbrev)
                .map(|p| p.elo_rating - 1500.0)
                .unwrap_or(0.0);

            let features = GameFeatures {
                point_diff: -(deficit as f64), // trailing = negative
                time_remaining: game.time_remaining_mins,
                quarter: game.quarter,
                possession: 0.5, // unknown from ESPN
                pregame_spread: 0.0,
                elo_diff,
                comeback_rate: Some(comeback_rate),
            };

            let prediction = self.winprob_model.predict(&features);

            // Blend model win_prob with historical comeback rate
            // Weight: 60% model, 40% historical (comeback rate is a strong signal)
            let adjusted_win_prob = prediction.win_prob * 0.6 + comeback_rate * 0.4;

            info!(
                "Candidate: {} trailing {} by {} | comeback_rate={:.3} model_wp={:.3} adjusted={:.3}",
                trail_abbrev,
                if game.home_score > game.away_score {
                    &game.home_abbrev
                } else {
                    &game.away_abbrev
                },
                deficit,
                comeback_rate,
                prediction.win_prob,
                adjusted_win_prob,
            );

            candidates.push(ComebackCandidate {
                game: game.clone(),
                trailing_team: trail_name,
                trailing_abbrev: trail_abbrev,
                deficit,
                comeback_rate,
                adjusted_win_prob,
            });
        }

        candidates
    }

    /// Run the candidate scan pipeline using already-fetched ESPN games.
    pub fn scan_games(&mut self, games: &[LiveGame]) -> Vec<ComebackCandidate> {
        self.reset_daily_if_needed();
        self.scan_games_inner(games)
    }

    /// Main scan: ESPN → filter Q3 → check comeback rates → calculate edge
    ///
    /// This does NOT look up Polymarket markets — that's the agent's job.
    /// Instead it returns opportunities with `market_slug` and `token_id`
    /// left empty, to be filled by the agent layer that has access to
    /// the Polymarket client.
    pub async fn scan_espn(&mut self) -> Vec<ComebackCandidate> {
        self.reset_daily_if_needed();

        let games = match self.espn.fetch_live_games().await {
            Ok(g) => g,
            Err(e) => {
                warn!("ESPN fetch failed: {}", e);
                return vec![];
            }
        };
        self.scan_games_inner(&games)
    }

    /// Given a candidate and a market price, determine if there's a tradeable edge
    pub fn evaluate_opportunity(
        &self,
        candidate: &ComebackCandidate,
        market_price: Decimal,
        market_slug: String,
        token_id: String,
    ) -> Option<ComebackOpportunity> {
        // Edge = our estimated probability - market price
        let edge =
            candidate.adjusted_win_prob - market_price.to_string().parse::<f64>().unwrap_or(1.0);

        if edge < self.cfg.min_edge.to_string().parse::<f64>().unwrap_or(0.05) {
            debug!(
                "{} edge={:.3} < min_edge, skipping",
                candidate.trailing_abbrev, edge
            );
            return None;
        }

        // Check max entry price
        if market_price > self.cfg.max_entry_price {
            debug!(
                "{} market_price={} > max_entry_price={}, skipping",
                candidate.trailing_abbrev, market_price, self.cfg.max_entry_price
            );
            return None;
        }

        // Check daily spend
        let cost = market_price * Decimal::from(self.cfg.shares);
        if !self.can_spend(cost) {
            warn!(
                "Daily spend limit reached ({}/{})",
                self.state.daily_spend_usd, self.cfg.max_daily_spend_usd
            );
            return None;
        }

        info!(
            "OPPORTUNITY: {} deficit={} edge={:.3} price={} shares={}",
            candidate.trailing_abbrev, candidate.deficit, edge, market_price, self.cfg.shares
        );

        Some(ComebackOpportunity {
            game: candidate.game.clone(),
            trailing_team: candidate.trailing_team.clone(),
            trailing_abbrev: candidate.trailing_abbrev.clone(),
            deficit: candidate.deficit,
            comeback_rate: candidate.comeback_rate,
            adjusted_win_prob: candidate.adjusted_win_prob,
            market_price,
            edge,
            market_slug,
            token_id,
        })
    }

    /// Pick the best opportunity from a list (highest edge)
    pub fn pick_best<'a>(
        &self,
        opps: &'a [ComebackOpportunity],
    ) -> Option<&'a ComebackOpportunity> {
        opps.iter().max_by(|a, b| {
            a.edge
                .partial_cmp(&b.edge)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

/// Intermediate candidate before Polymarket price lookup
#[derive(Debug, Clone)]
pub struct ComebackCandidate {
    pub game: LiveGame,
    pub trailing_team: String,
    pub trailing_abbrev: String,
    pub deficit: i32,
    pub comeback_rate: f64,
    pub adjusted_win_prob: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_state_daily_spend() {
        let mut state = NbaComebackState::default();

        // Fresh state should have zero spend
        assert_eq!(state.daily_spend_usd, Decimal::ZERO);

        // Add some spend
        state.daily_spend_usd += dec!(50);
        let max = dec!(100);
        assert!(state.daily_spend_usd + dec!(40) <= max);
        assert!(!(state.daily_spend_usd + dec!(60) <= max));
    }

    #[test]
    fn test_state_cooldown() {
        let mut state = NbaComebackState::default();

        // No cooldown for unknown game
        assert!(!state.traded_games.contains_key("game1"));

        // Record a trade
        state.traded_games.insert("game1".to_string(), Utc::now());

        // Should be on cooldown (just traded)
        let elapsed = (Utc::now() - *state.traded_games.get("game1").unwrap()).num_seconds();
        assert!(elapsed < 300); // 5 min cooldown
    }
}
