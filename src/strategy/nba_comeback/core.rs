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
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

use super::nba_winprob::{GameFeatures, LiveWinProbModel};
use crate::config::NbaComebackConfig;
use crate::strategy::nba_comeback::comeback_stats::ComebackStatsProvider;
use crate::strategy::nba_comeback::espn::{EspnClient, LiveGame};

mod positioning;

pub use positioning::{GamePosition, PositionEntry};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NbaComebackState {
    pub traded_games: HashMap<String, DateTime<Utc>>,
    pub daily_spend_usd: Decimal,
    pub daily_spend_day: NaiveDate,
    /// Realized PnL for current UTC day.
    pub daily_realized_pnl_usd: Decimal,
    /// Lifetime realized PnL from settled/early-exit positions.
    pub total_realized_pnl_usd: Decimal,
    /// Number of realized trades used for performance sizing.
    pub settled_trades: u64,
    /// Number of winning realized trades.
    pub winning_trades: u64,
    /// Current consecutive losing trades.
    pub loss_streak: u32,
    /// Per-game position tracking for Kelly scaling-in
    pub game_positions: HashMap<String, GamePosition>,
    /// Initial-entry idempotency keys (`game_id:token_id`) to prevent duplicate submits.
    pub initial_entries: HashSet<String>,
}

impl Default for NbaComebackState {
    fn default() -> Self {
        Self {
            traded_games: HashMap::new(),
            daily_spend_usd: Decimal::ZERO,
            daily_spend_day: Utc::now().date_naive(),
            daily_realized_pnl_usd: Decimal::ZERO,
            total_realized_pnl_usd: Decimal::ZERO,
            settled_trades: 0,
            winning_trades: 0,
            loss_streak: 0,
            game_positions: HashMap::new(),
            initial_entries: HashSet::new(),
        }
    }
}

impl NbaComebackState {
    fn initial_entry_key(game_id: &str, token_id: &str) -> String {
        format!("{game_id}:{token_id}")
    }

    pub fn is_initial_entry_recorded(&self, game_id: &str, token_id: &str) -> bool {
        let key = Self::initial_entry_key(game_id, token_id);
        self.initial_entries.contains(&key)
    }

    /// Returns true when the key was inserted, false when it already existed.
    pub fn record_initial_entry(&mut self, game_id: &str, token_id: &str) -> bool {
        let key = Self::initial_entry_key(game_id, token_id);
        self.initial_entries.insert(key)
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
            self.state.daily_realized_pnl_usd = Decimal::ZERO;
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

    pub fn has_hit_daily_loss_limit(&self) -> bool {
        self.state.daily_realized_pnl_usd <= -self.cfg.performance_daily_loss_limit_usd
    }

    pub fn can_open_new_risk(&self) -> bool {
        !self.has_hit_daily_loss_limit()
    }

    pub fn settled_win_rate(&self) -> Option<f64> {
        if self.state.settled_trades == 0 {
            return None;
        }
        Some(self.state.winning_trades as f64 / self.state.settled_trades as f64)
    }

    /// Dynamic size multiplier from realized performance.
    ///
    /// - No adjustment until `performance_min_settled_trades` reached.
    /// - Low win rate and loss streak multipliers are multiplicative.
    pub fn risk_size_multiplier(&self) -> f64 {
        if self.state.settled_trades < self.cfg.performance_min_settled_trades {
            return 1.0;
        }

        let mut multiplier = 1.0_f64;
        if let Some(win_rate) = self.settled_win_rate() {
            if win_rate < self.cfg.performance_min_win_rate {
                multiplier *= self.cfg.performance_low_winrate_multiplier;
            }
        }
        if self.state.loss_streak >= self.cfg.performance_loss_streak_threshold {
            multiplier *= self.cfg.performance_loss_streak_multiplier;
        }

        multiplier.clamp(0.1, 1.0)
    }

    /// Base shares scaled by performance-aware risk multiplier.
    pub fn adjusted_shares(&self, base_shares: u64) -> u64 {
        if base_shares == 0 {
            return 0;
        }
        let adjusted = (base_shares as f64 * self.risk_size_multiplier()).floor() as u64;
        adjusted.max(1)
    }

    /// Record realized trade outcome (early exit or final settlement).
    pub fn record_realized_pnl(&mut self, pnl: Decimal) {
        self.state.total_realized_pnl_usd += pnl;
        self.state.daily_realized_pnl_usd += pnl;
        self.state.settled_trades = self.state.settled_trades.saturating_add(1);
        if pnl > Decimal::ZERO {
            self.state.winning_trades = self.state.winning_trades.saturating_add(1);
            self.state.loss_streak = 0;
        } else if pnl < Decimal::ZERO {
            self.state.loss_streak = self.state.loss_streak.saturating_add(1);
        } else {
            self.state.loss_streak = 0;
        }
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

    fn test_cfg() -> crate::config::NbaComebackConfig {
        crate::config::NbaComebackConfig {
            enabled: true,
            min_edge: dec!(0.05),
            max_entry_price: dec!(0.75),
            shares: 50,
            cooldown_secs: 300,
            max_daily_spend_usd: dec!(100),
            min_deficit: 1,
            max_deficit: 15,
            target_quarter: 3,
            espn_poll_interval_secs: 30,
            min_comeback_rate: 0.15,
            season: "2025-26".to_string(),
            grok_enabled: false,
            grok_interval_secs: 300,
            grok_min_edge: dec!(0.08),
            grok_min_confidence: 0.6,
            grok_decision_cooldown_secs: 60,
            grok_fallback_enabled: true,
            min_reward_risk_ratio: 4.0,
            min_expected_value: 0.05,
            kelly_fraction_cap: 0.25,
            performance_daily_loss_limit_usd: dec!(30),
            performance_min_settled_trades: 10,
            performance_min_win_rate: 0.45,
            performance_low_winrate_multiplier: 0.60,
            performance_loss_streak_threshold: 3,
            performance_loss_streak_multiplier: 0.50,
            scaling_enabled: true,
            scaling_max_adds: 3,
            scaling_min_price_drop_pct: 5.0,
            scaling_max_game_exposure_usd: dec!(50),
            scaling_min_comeback_retention: 0.70,
            scaling_min_time_remaining_mins: 8.0,
            early_exit_enabled: true,
            early_exit_take_profit_pct: 15.0,
            early_exit_stop_loss_pct: 20.0,
        }
    }

    #[tokio::test]
    async fn test_daily_loss_limit_blocks_new_risk() {
        let cfg = test_cfg();
        let mut core = NbaComebackCore {
            espn: EspnClient::new(),
            stats: ComebackStatsProvider::new(
                sqlx::postgres::PgPoolOptions::new()
                    .connect_lazy("postgres://localhost/unused")
                    .expect("lazy pool"),
                cfg.season.clone(),
            ),
            winprob_model: LiveWinProbModel::default_untrained(),
            cfg,
            state: NbaComebackState::default(),
        };

        core.record_realized_pnl(dec!(-20));
        assert!(core.can_open_new_risk());
        core.record_realized_pnl(dec!(-11));
        assert!(core.has_hit_daily_loss_limit());
        assert!(!core.can_open_new_risk());
    }

    #[tokio::test]
    async fn test_adjusted_shares_reduces_after_poor_performance() {
        let cfg = test_cfg();
        let mut core = NbaComebackCore {
            espn: EspnClient::new(),
            stats: ComebackStatsProvider::new(
                sqlx::postgres::PgPoolOptions::new()
                    .connect_lazy("postgres://localhost/unused")
                    .expect("lazy pool"),
                cfg.season.clone(),
            ),
            winprob_model: LiveWinProbModel::default_untrained(),
            cfg,
            state: NbaComebackState::default(),
        };

        core.state.settled_trades = 10;
        core.state.winning_trades = 3; // 30% < 45%
        core.state.loss_streak = 3; // >= threshold

        let multiplier = core.risk_size_multiplier();
        assert!((multiplier - 0.30).abs() < f64::EPSILON); // 0.60 * 0.50
        assert_eq!(core.adjusted_shares(50), 15);
    }
}
