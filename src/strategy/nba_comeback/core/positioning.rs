use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::NbaComebackCore;

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

impl NbaComebackCore {
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
            None => return false,
        };

        let add_count = pos.entries.len().saturating_sub(1) as u32;
        if add_count >= self.cfg.scaling_max_adds {
            debug!(game_id, adds = add_count, "scaling: max adds reached");
            return false;
        }

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

        if time_remaining_mins < self.cfg.scaling_min_time_remaining_mins {
            debug!(
                game_id,
                time_remaining = format!("{:.1}m", time_remaining_mins),
                min = format!("{:.1}m", self.cfg.scaling_min_time_remaining_mins),
                "scaling: not enough time remaining"
            );
            return false;
        }

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
            return None;
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
            return None;
        }

        let delta_shares = (delta / price_f64).floor() as u64;
        if delta_shares == 0 {
            return None;
        }

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NbaComebackConfig;
    use crate::strategy::nba_comeback::NbaComebackState;
    use crate::strategy::nba_comeback::comeback_stats::ComebackStatsProvider;
    use crate::strategy::nba_comeback::espn::EspnClient;
    use crate::strategy::nba_comeback::nba_winprob::LiveWinProbModel;
    use rust_decimal_macros::dec;

    fn scaling_cfg() -> NbaComebackConfig {
        NbaComebackConfig {
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

    fn test_core(cfg: NbaComebackConfig) -> NbaComebackCore {
        NbaComebackCore {
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
        }
    }

    #[test]
    fn test_record_position_entry() {
        let mut state = NbaComebackState::default();
        let game_id = "game1";

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

        let add_count = pos.entries.len().saturating_sub(1) as u32;
        assert_eq!(add_count, 3);
        assert!(add_count >= cfg.scaling_max_adds);
    }

    #[test]
    fn test_scaling_guards_price_drop() {
        let cfg = scaling_cfg();

        let drop_pct = ((dec!(0.20) - dec!(0.18)) * dec!(100) / dec!(0.20))
            .to_string()
            .parse::<f64>()
            .unwrap();
        assert!(drop_pct >= cfg.scaling_min_price_drop_pct);

        let drop_pct2 = ((dec!(0.20) - dec!(0.195)) * dec!(100) / dec!(0.20))
            .to_string()
            .parse::<f64>()
            .unwrap();
        assert!(drop_pct2 < cfg.scaling_min_price_drop_pct);
    }

    #[test]
    fn test_scaling_guards_comeback_retention() {
        let cfg = scaling_cfg();

        let retention = 0.20 / 0.25;
        assert!(retention >= cfg.scaling_min_comeback_retention);

        let retention2 = 0.15 / 0.25;
        assert!(retention2 < cfg.scaling_min_comeback_retention);
    }

    #[test]
    fn test_kelly_scaling_shares_basic() {
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

        assert_eq!(capped, 0.25);
        assert!((target - 12.5).abs() < 0.01);
        assert_eq!(shares, 54);
    }

    #[test]
    fn test_kelly_scaling_no_edge() {
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

        assert!(!state.record_initial_entry("game-1", "token-a"));
        assert!(state.record_initial_entry("game-1", "token-b"));
        assert!(state.record_initial_entry("game-2", "token-a"));
    }

    #[tokio::test]
    async fn test_record_position_entry_with_market_metadata() {
        let cfg = scaling_cfg();
        let mut core = test_core(cfg);

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
        let restored: super::super::NbaComebackState =
            serde_json::from_value(json).expect("deserialize state");

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
}
