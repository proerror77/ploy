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
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

use crate::config::NbaComebackConfig;
use crate::strategy::nba_comeback::comeback_stats::ComebackStatsProvider;
use crate::strategy::nba_comeback::espn::{EspnClient, LiveGame};
use super::nba_winprob::{GameFeatures, LiveWinProbModel};

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

/// A single entry in a game position (for Kelly scaling-in tracking)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionEntry {
    pub entry_price: Decimal,
    pub shares: u64,
    pub timestamp: DateTime<Utc>,
}

/// Tracks all entries for a single game (for Kelly scaling-in)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamePosition {
    pub entries: Vec<PositionEntry>,
    pub initial_comeback_rate: f64,
    pub total_shares: u64,
    pub total_cost: Decimal,
    /// Team abbreviation this YES position represents (for final settlement PnL).
    #[serde(default)]
    pub trailing_abbrev: Option<String>,
    /// Market slug for exits; populated on initial entry.
    #[serde(default)]
    pub market_slug: Option<String>,
    /// Token id for exits; populated on initial entry.
    #[serde(default)]
    pub token_id: Option<String>,
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

    pub fn has_position(&self, game_id: &str) -> bool {
        self.state.game_positions.contains_key(game_id)
    }

    pub fn is_duplicate_initial_entry(&self, game_id: &str, token_id: &str) -> bool {
        self.has_position(game_id) || self.state.is_initial_entry_recorded(game_id, token_id)
    }

    pub fn record_initial_entry_submission(
        &mut self,
        game_id: &str,
        token_id: &str,
        spend: Decimal,
    ) {
        self.record_trade(game_id, spend);
        self.state.record_initial_entry(game_id, token_id);
    }

    pub fn record_trade(&mut self, game_id: &str, spend: Decimal) {
        self.state
            .traded_games
            .insert(game_id.to_string(), Utc::now());
        self.state.daily_spend_usd += spend;
    }

    // ── Kelly scaling-in ──────────────────────────────────────────

    /// Record a new position entry for scaling-in tracking.
    /// Call this after a successful order submission (initial or scale-in).
    pub fn record_position_entry(
        &mut self,
        game_id: &str,
        entry_price: Decimal,
        shares: u64,
        comeback_rate: f64,
    ) {
        self.record_position_entry_internal(
            game_id,
            None,
            None,
            None,
            entry_price,
            shares,
            comeback_rate,
        );
    }

    /// Record a position entry and persist market metadata needed for exit orders.
    pub fn record_position_entry_with_market(
        &mut self,
        game_id: &str,
        market_slug: &str,
        token_id: &str,
        entry_price: Decimal,
        shares: u64,
        comeback_rate: f64,
    ) {
        self.record_position_entry_internal(
            game_id,
            None,
            Some(market_slug),
            Some(token_id),
            entry_price,
            shares,
            comeback_rate,
        );
    }

    /// Record a position entry with market metadata and trailing team abbreviation.
    pub fn record_position_entry_with_market_and_team(
        &mut self,
        game_id: &str,
        trailing_abbrev: &str,
        market_slug: &str,
        token_id: &str,
        entry_price: Decimal,
        shares: u64,
        comeback_rate: f64,
    ) {
        self.record_position_entry_internal(
            game_id,
            Some(trailing_abbrev),
            Some(market_slug),
            Some(token_id),
            entry_price,
            shares,
            comeback_rate,
        );
    }

    fn record_position_entry_internal(
        &mut self,
        game_id: &str,
        trailing_abbrev: Option<&str>,
        market_slug: Option<&str>,
        token_id: Option<&str>,
        entry_price: Decimal,
        shares: u64,
        comeback_rate: f64,
    ) {
        let cost = entry_price * Decimal::from(shares);
        let entry = PositionEntry {
            entry_price,
            shares,
            timestamp: Utc::now(),
        };

        let pos = self
            .state
            .game_positions
            .entry(game_id.to_string())
            .or_insert_with(|| GamePosition {
                entries: Vec::new(),
                initial_comeback_rate: comeback_rate,
                total_shares: 0,
                total_cost: Decimal::ZERO,
                trailing_abbrev: trailing_abbrev.map(ToString::to_string),
                market_slug: market_slug.map(ToString::to_string),
                token_id: token_id.map(ToString::to_string),
            });

        if pos.trailing_abbrev.is_none() {
            pos.trailing_abbrev = trailing_abbrev.map(ToString::to_string);
        }
        if pos.market_slug.is_none() {
            pos.market_slug = market_slug.map(ToString::to_string);
        }
        if pos.token_id.is_none() {
            pos.token_id = token_id.map(ToString::to_string);
        }

        pos.entries.push(entry);
        pos.total_shares += shares;
        pos.total_cost += cost;
    }

    /// Remove a tracked game position after full exit/settlement.
    pub fn close_position(&mut self, game_id: &str) -> Option<GamePosition> {
        self.state.game_positions.remove(game_id)
    }

    /// Check whether scaling-in guards pass for a game.
    ///
    /// Guards:
    /// 1. Existing position exists and hasn't exceeded max adds
    /// 2. Price dropped >= min_price_drop_pct from last entry
    /// 3. Comeback rate retained >= min_comeback_retention of initial
    /// 4. Enough game time remaining
    /// 5. Total exposure under max_game_exposure_usd
    pub fn can_scale_in(
        &self,
        game_id: &str,
        current_price: Decimal,
        current_comeback_rate: f64,
        time_remaining_mins: f64,
    ) -> bool {
        let pos = match self.state.game_positions.get(game_id) {
            Some(p) => p,
            None => return false, // no existing position → use initial entry path
        };

        // Guard 1: max adds not exceeded
        // entries includes the initial entry, so add count = entries.len() - 1
        let add_count = pos.entries.len().saturating_sub(1) as u32;
        if add_count >= self.cfg.scaling_max_adds {
            debug!(game_id, adds = add_count, "scaling: max adds reached");
            return false;
        }

        // Guard 2: price drop from last entry
        if let Some(last_entry) = pos.entries.last() {
            let drop_pct = if last_entry.entry_price > Decimal::ZERO {
                let drop = last_entry.entry_price - current_price;
                (drop * dec!(100) / last_entry.entry_price)
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            if drop_pct < self.cfg.scaling_min_price_drop_pct {
                debug!(
                    game_id,
                    drop_pct = format!("{:.1}%", drop_pct),
                    min = format!("{:.1}%", self.cfg.scaling_min_price_drop_pct),
                    "scaling: insufficient price drop"
                );
                return false;
            }
        }

        // Guard 3: comeback rate retention
        let retention = if pos.initial_comeback_rate > 0.0 {
            current_comeback_rate / pos.initial_comeback_rate
        } else {
            0.0
        };
        if retention < self.cfg.scaling_min_comeback_retention {
            debug!(
                game_id,
                retention = format!("{:.0}%", retention * 100.0),
                min = format!("{:.0}%", self.cfg.scaling_min_comeback_retention * 100.0),
                "scaling: comeback rate degraded too much"
            );
            return false;
        }

        // Guard 4: time remaining
        if time_remaining_mins < self.cfg.scaling_min_time_remaining_mins {
            debug!(
                game_id,
                time_remaining = format!("{:.1}m", time_remaining_mins),
                min = format!("{:.1}m", self.cfg.scaling_min_time_remaining_mins),
                "scaling: not enough time remaining"
            );
            return false;
        }

        // Guard 5: total exposure cap
        if pos.total_cost >= self.cfg.scaling_max_game_exposure_usd {
            debug!(
                game_id,
                exposure = %pos.total_cost,
                max = %self.cfg.scaling_max_game_exposure_usd,
                "scaling: max game exposure reached"
            );
            return false;
        }

        true
    }

    /// Calculate the number of shares to add for Kelly-proportional scaling.
    ///
    /// Returns `Some(shares)` if we should add, `None` if Kelly says hold/reduce.
    ///
    /// Formula:
    ///   kelly_fraction = edge / (1 - price)
    ///   capped_fraction = min(kelly_fraction, kelly_fraction_cap)
    ///   target_exposure = capped_fraction * max_game_exposure_usd
    ///   delta = target_exposure - current_exposure
    ///   shares = floor(delta / current_price)
    pub fn kelly_scaling_shares(
        &self,
        game_id: &str,
        current_price: Decimal,
        fair_value: f64,
    ) -> Option<u64> {
        let pos = self.state.game_positions.get(game_id)?;

        let price_f64 = current_price
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0)
            .clamp(0.001, 0.999);

        let edge = fair_value - price_f64;
        if edge <= 0.0 {
            return None; // no edge → don't add
        }

        let kelly_fraction = edge / (1.0 - price_f64);
        let capped = kelly_fraction.min(self.cfg.kelly_fraction_cap);

        let max_exposure_f64 = self
            .cfg
            .scaling_max_game_exposure_usd
            .to_string()
            .parse::<f64>()
            .unwrap_or(50.0);

        let target_exposure = capped * max_exposure_f64;
        let current_exposure = pos.total_cost.to_string().parse::<f64>().unwrap_or(0.0);

        let delta = target_exposure - current_exposure;
        if delta <= 0.0 {
            return None; // already at or above optimal
        }

        let delta_shares = (delta / price_f64).floor() as u64;
        if delta_shares == 0 {
            return None;
        }

        // Don't exceed max game exposure
        let add_cost = price_f64 * delta_shares as f64;
        if current_exposure + add_cost > max_exposure_f64 {
            let clamped = ((max_exposure_f64 - current_exposure) / price_f64).floor() as u64;
            if clamped == 0 {
                return None;
            }
            return Some(clamped);
        }

        Some(delta_shares)
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

    fn scaling_cfg() -> crate::config::NbaComebackConfig {
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

    #[test]
    fn test_record_position_entry() {
        let mut state = NbaComebackState::default();
        // Can't construct full NbaComebackCore without DB, so test state directly
        let game_id = "game1";

        // Record initial entry
        let entry = PositionEntry {
            entry_price: dec!(0.15),
            shares: 50,
            timestamp: Utc::now(),
        };
        let pos = state
            .game_positions
            .entry(game_id.to_string())
            .or_insert_with(|| GamePosition {
                entries: Vec::new(),
                initial_comeback_rate: 0.22,
                total_shares: 0,
                total_cost: Decimal::ZERO,
                trailing_abbrev: None,
                market_slug: None,
                token_id: None,
            });
        pos.entries.push(entry);
        pos.total_shares += 50;
        pos.total_cost += dec!(0.15) * dec!(50);

        let pos = state.game_positions.get(game_id).unwrap();
        assert_eq!(pos.entries.len(), 1);
        assert_eq!(pos.total_shares, 50);
        assert_eq!(pos.total_cost, dec!(7.5));
        assert!((pos.initial_comeback_rate - 0.22).abs() < f64::EPSILON);
    }

    #[test]
    fn test_scaling_guards_max_adds() {
        let cfg = scaling_cfg();
        let mut state = NbaComebackState::default();
        let game_id = "game1";

        // Create a position with 4 entries (initial + 3 adds = max_adds reached)
        let pos = state
            .game_positions
            .entry(game_id.to_string())
            .or_insert_with(|| GamePosition {
                entries: Vec::new(),
                initial_comeback_rate: 0.25,
                total_shares: 0,
                total_cost: Decimal::ZERO,
                trailing_abbrev: None,
                market_slug: None,
                token_id: None,
            });
        for i in 0..4 {
            pos.entries.push(PositionEntry {
                entry_price: dec!(0.15) - Decimal::from(i) * dec!(0.01),
                shares: 50,
                timestamp: Utc::now(),
            });
            pos.total_shares += 50;
            pos.total_cost += dec!(7.5);
        }

        // add_count = 4 - 1 = 3, which equals scaling_max_adds → should fail
        let add_count = pos.entries.len().saturating_sub(1) as u32;
        assert_eq!(add_count, 3);
        assert!(add_count >= cfg.scaling_max_adds);
    }

    #[test]
    fn test_scaling_guards_price_drop() {
        let cfg = scaling_cfg();

        // Last entry at $0.20, current at $0.18 → 10% drop → passes 5% threshold
        let drop_pct = ((dec!(0.20) - dec!(0.18)) * dec!(100) / dec!(0.20))
            .to_string()
            .parse::<f64>()
            .unwrap();
        assert!(drop_pct >= cfg.scaling_min_price_drop_pct);

        // Last entry at $0.20, current at $0.195 → 2.5% drop → fails 5% threshold
        let drop_pct2 = ((dec!(0.20) - dec!(0.195)) * dec!(100) / dec!(0.20))
            .to_string()
            .parse::<f64>()
            .unwrap();
        assert!(drop_pct2 < cfg.scaling_min_price_drop_pct);
    }

    #[test]
    fn test_scaling_guards_comeback_retention() {
        let cfg = scaling_cfg();

        // Initial comeback rate 0.25, current 0.20 → retention 80% → passes 70%
        let retention = 0.20 / 0.25;
        assert!(retention >= cfg.scaling_min_comeback_retention);

        // Current 0.15 → retention 60% → fails 70%
        let retention2 = 0.15 / 0.25;
        assert!(retention2 < cfg.scaling_min_comeback_retention);
    }

    #[test]
    fn test_kelly_scaling_shares_basic() {
        // Manually test the Kelly math:
        // fair_value=0.35, price=0.12, max_exposure=$50, cap=0.25
        // edge = 0.35 - 0.12 = 0.23
        // kelly = 0.23 / (1 - 0.12) = 0.2614
        // capped = min(0.2614, 0.25) = 0.25
        // target = 0.25 * 50 = $12.50
        // current = $6.00 (50 shares at $0.12)
        // delta = $6.50
        // shares = floor(6.50 / 0.12) = 54

        let mut state = NbaComebackState::default();
        state
            .game_positions
            .entry("game1".to_string())
            .or_insert_with(|| GamePosition {
                entries: vec![PositionEntry {
                    entry_price: dec!(0.12),
                    shares: 50,
                    timestamp: Utc::now(),
                }],
                initial_comeback_rate: 0.22,
                total_shares: 50,
                total_cost: dec!(6),
                trailing_abbrev: None,
                market_slug: None,
                token_id: None,
            });

        let cfg = scaling_cfg();
        let price_f64 = 0.12_f64;
        let fair_value = 0.35_f64;
        let edge = fair_value - price_f64;
        let kelly = edge / (1.0 - price_f64);
        let capped = kelly.min(cfg.kelly_fraction_cap);
        let max_exp = 50.0_f64;
        let target = capped * max_exp;
        let current = 6.0_f64;
        let delta = target - current;
        let shares = (delta / price_f64).floor() as u64;

        assert_eq!(capped, 0.25); // kelly 0.2614 capped at 0.25
        assert!((target - 12.5).abs() < 0.01);
        assert_eq!(shares, 54);
    }

    #[test]
    fn test_kelly_scaling_no_edge() {
        // fair_value=0.10, price=0.15 → negative edge → no scaling
        let price_f64 = 0.15_f64;
        let fair_value = 0.10_f64;
        let edge = fair_value - price_f64;
        assert!(edge <= 0.0);
    }

    #[test]
    fn test_state_prevents_duplicate_initial_entries() {
        let mut state = NbaComebackState::default();

        assert!(!state.is_initial_entry_recorded("game-1", "token-a"));
        assert!(state.record_initial_entry("game-1", "token-a"));
        assert!(state.is_initial_entry_recorded("game-1", "token-a"));

        // Same game+token should be treated as duplicate.
        assert!(!state.record_initial_entry("game-1", "token-a"));

        // Different token or game should still be allowed.
        assert!(state.record_initial_entry("game-1", "token-b"));
        assert!(state.record_initial_entry("game-2", "token-a"));
    }

    #[tokio::test]
    async fn test_record_position_entry_with_market_metadata() {
        let cfg = scaling_cfg();
        let mut core = NbaComebackCore {
            espn: EspnClient::new(),
            stats: ComebackStatsProvider::new(
                // Test doesn't touch DB; use lazy connection options via a local pool.
                sqlx::postgres::PgPoolOptions::new()
                    .connect_lazy("postgres://localhost/unused")
                    .expect("lazy pool"),
                cfg.season.clone(),
            ),
            winprob_model: LiveWinProbModel::default_untrained(),
            cfg,
            state: NbaComebackState::default(),
        };

        core.record_position_entry_with_market(
            "game-1",
            "market-1",
            "token-1",
            dec!(0.20),
            50,
            0.25,
        );

        let pos = core.state.game_positions.get("game-1").expect("position");
        assert_eq!(pos.market_slug.as_deref(), Some("market-1"));
        assert_eq!(pos.token_id.as_deref(), Some("token-1"));
    }

    #[test]
    fn test_state_json_roundtrip_preserves_positions_and_idempotency() {
        let mut state = NbaComebackState::default();
        state.daily_spend_usd = dec!(12.5);
        state.record_initial_entry("game-1", "token-a");
        state.traded_games.insert(
            "game-1".to_string(),
            Utc::now() - chrono::Duration::seconds(15),
        );
        state.game_positions.insert(
            "game-1".to_string(),
            GamePosition {
                entries: vec![PositionEntry {
                    entry_price: dec!(0.31),
                    shares: 40,
                    timestamp: Utc::now(),
                }],
                initial_comeback_rate: 0.22,
                total_shares: 40,
                total_cost: dec!(12.4),
                trailing_abbrev: Some("HOU".to_string()),
                market_slug: Some("market-1".to_string()),
                token_id: Some("token-a".to_string()),
            },
        );

        let json = serde_json::to_value(&state).expect("serialize state");
        let restored: NbaComebackState = serde_json::from_value(json).expect("deserialize state");

        assert!(restored.is_initial_entry_recorded("game-1", "token-a"));
        let pos = restored
            .game_positions
            .get("game-1")
            .expect("restored position");
        assert_eq!(pos.total_shares, 40);
        assert_eq!(pos.market_slug.as_deref(), Some("market-1"));
        assert_eq!(restored.daily_spend_usd, dec!(12.5));
        assert_eq!(restored.daily_realized_pnl_usd, Decimal::ZERO);
    }

    #[tokio::test]
    async fn test_daily_loss_limit_blocks_new_risk() {
        let cfg = scaling_cfg();
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
        let cfg = scaling_cfg();
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
