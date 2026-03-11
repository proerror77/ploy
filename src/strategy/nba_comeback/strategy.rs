use crate::ai_clients::PolymarketSportsClient;
use crate::config::NbaComebackConfig;
use crate::domain::{OrderStatus, Side};
use crate::error::Result;
use crate::strategy::nba_comeback::comeback_stats::ComebackStatsProvider;
use crate::strategy::nba_comeback::core::{ComebackOpportunity, NbaComebackCore, NbaComebackState};
#[cfg(test)]
use crate::strategy::nba_comeback::espn::LiveGame;
use crate::strategy::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyEvent, StrategyEventType, StrategyStateInfo,
};
use anyhow::anyhow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::postgres::PgPoolOptions;
use std::collections::HashMap;

mod opportunity_flow;

const NBA_COMEBACK_STRATEGY_NAME: &str = "nba_comeback";
const DEFAULT_DATABASE_URL: &str = "postgres://localhost/unused";
const NBA_COMEBACK_PRIORITY: u8 = 8;

pub(crate) fn default_nba_comeback_config() -> NbaComebackConfig {
    NbaComebackConfig {
        enabled: true,
        min_edge: Decimal::new(5, 2),
        max_entry_price: Decimal::new(75, 2),
        shares: 50,
        cooldown_secs: 300,
        max_daily_spend_usd: Decimal::new(100, 0),
        min_deficit: 1,
        max_deficit: 15,
        target_quarter: 3,
        espn_poll_interval_secs: 30,
        min_comeback_rate: 0.15,
        season: "2025-26".to_string(),
        grok_enabled: false,
        grok_interval_secs: 300,
        grok_min_edge: Decimal::new(8, 2),
        grok_min_confidence: 0.6,
        grok_decision_cooldown_secs: 60,
        grok_fallback_enabled: true,
        min_reward_risk_ratio: 4.0,
        min_expected_value: 0.05,
        kelly_fraction_cap: 0.25,
        performance_daily_loss_limit_usd: Decimal::new(30, 0),
        performance_min_settled_trades: 10,
        performance_min_win_rate: 0.45,
        performance_low_winrate_multiplier: 0.60,
        performance_loss_streak_threshold: 3,
        performance_loss_streak_multiplier: 0.50,
        scaling_enabled: false,
        scaling_max_adds: 3,
        scaling_min_price_drop_pct: 5.0,
        scaling_max_game_exposure_usd: Decimal::new(50, 0),
        scaling_min_comeback_retention: 0.70,
        scaling_min_time_remaining_mins: 8.0,
        early_exit_enabled: true,
        early_exit_take_profit_pct: 15.0,
        early_exit_stop_loss_pct: 20.0,
    }
}

#[derive(Debug, Clone)]
pub struct NbaComebackMarketRegistration {
    pub game_id: Option<String>,
    pub market_slug: String,
    pub condition_id: Option<String>,
    pub home_team: String,
    pub away_team: String,
    pub home_abbrev: String,
    pub away_abbrev: String,
    pub home_token_id: String,
    pub away_token_id: String,
    pub home_price: Decimal,
    pub away_price: Decimal,
}

#[derive(Debug, Clone)]
struct PendingNbaComebackOrder {
    client_order_id: String,
    game_id: String,
    trailing_abbrev: String,
    token_id: String,
    market_slug: String,
    condition_id: Option<String>,
    requested_shares: u64,
    accounted_filled_shares: u64,
    limit_price: Decimal,
    reserved_notional_usd: Decimal,
}

pub type NbaComebackAdapter = NbaComebackStrategy;

pub struct NbaComebackStrategy {
    id: String,
    dry_run: bool,
    enabled: bool,
    core: NbaComebackCore,
    pm_sports: Option<PolymarketSportsClient>,
    market_registrations: Vec<NbaComebackMarketRegistration>,
    pending_orders: HashMap<String, PendingNbaComebackOrder>,
    positions: HashMap<String, PositionInfo>,
    reserved_notional_usd: Decimal,
    last_scan_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    stats_loaded: bool,
}

impl std::fmt::Debug for NbaComebackStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NbaComebackStrategy")
            .field("id", &self.id)
            .field("dry_run", &self.dry_run)
            .field("enabled", &self.enabled)
            .field("market_registrations", &self.market_registrations.len())
            .field("pending_orders", &self.pending_orders.len())
            .field("positions", &self.positions.len())
            .field("stats_loaded", &self.stats_loaded)
            .finish()
    }
}

impl NbaComebackStrategy {
    pub fn new(id: String, core: NbaComebackCore, dry_run: bool) -> Self {
        let enabled = core.cfg.enabled;
        Self {
            id,
            dry_run,
            enabled,
            core,
            pm_sports: PolymarketSportsClient::new().ok(),
            market_registrations: Vec::new(),
            pending_orders: HashMap::new(),
            positions: HashMap::new(),
            reserved_notional_usd: Decimal::ZERO,
            last_scan_at: None,
            last_error: None,
            stats_loaded: false,
        }
    }

    pub fn from_config(
        id: String,
        cfg: NbaComebackConfig,
        dry_run: bool,
        database_url: Option<&str>,
    ) -> Result<Self> {
        let database_url = database_url
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .unwrap_or_else(|| DEFAULT_DATABASE_URL.to_string());

        let pool = PgPoolOptions::new()
            .min_connections(0)
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_lazy(&database_url)
            .map_err(|e| anyhow!("invalid nba_comeback database_url: {}", e))?;
        let stats = ComebackStatsProvider::new(pool, cfg.season.clone());
        let espn = crate::strategy::nba_comeback::EspnClient::new();

        Ok(Self::new(
            id,
            NbaComebackCore::new(espn, stats, cfg),
            dry_run,
        ))
    }

    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        use toml::Value;

        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow!("Invalid TOML: {}", e))?;

        let strategy_section = config
            .get("strategy")
            .ok_or_else(|| anyhow!("Missing [strategy] section"))?;
        let strategy_name = strategy_section
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing strategy.name"))?;
        if strategy_name != NBA_COMEBACK_STRATEGY_NAME {
            return Err(anyhow!(
                "strategy.name must be \"{}\", got \"{}\"",
                NBA_COMEBACK_STRATEGY_NAME,
                strategy_name
            )
            .into());
        }

        let nba = config
            .get("nba_comeback")
            .ok_or_else(|| anyhow!("Missing [nba_comeback] section"))?;

        let mut cfg = default_nba_comeback_config();
        cfg.enabled = strategy_section
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        if let Some(enabled) = nba.get("enabled").and_then(|v| v.as_bool()) {
            cfg.enabled = enabled;
        }
        if let Some(min_edge) = decimal_from_toml(nba, "min_edge") {
            cfg.min_edge = min_edge;
        }
        if let Some(max_entry_price) = decimal_from_toml(nba, "max_entry_price") {
            cfg.max_entry_price = max_entry_price;
        }
        if let Some(shares) = nba.get("shares").and_then(|v| v.as_integer()) {
            cfg.shares = shares.max(0) as u64;
        }
        if let Some(cooldown_secs) = nba.get("cooldown_secs").and_then(|v| v.as_integer()) {
            cfg.cooldown_secs = cooldown_secs.max(0) as u64;
        }
        if let Some(max_daily_spend_usd) = decimal_from_toml(nba, "max_daily_spend_usd") {
            cfg.max_daily_spend_usd = max_daily_spend_usd;
        }
        if let Some(min_deficit) = nba.get("min_deficit").and_then(|v| v.as_integer()) {
            cfg.min_deficit = min_deficit as i32;
        }
        if let Some(max_deficit) = nba.get("max_deficit").and_then(|v| v.as_integer()) {
            cfg.max_deficit = max_deficit as i32;
        }
        if let Some(target_quarter) = nba.get("target_quarter").and_then(|v| v.as_integer()) {
            cfg.target_quarter = target_quarter.max(0) as u8;
        }
        if let Some(interval_secs) = nba
            .get("espn_poll_interval_secs")
            .and_then(|v| v.as_integer())
        {
            cfg.espn_poll_interval_secs = interval_secs.max(1) as u64;
        }
        if let Some(min_comeback_rate) = float_from_toml(nba, "min_comeback_rate") {
            cfg.min_comeback_rate = min_comeback_rate;
        }
        if let Some(season) = nba.get("season").and_then(|v| v.as_str()) {
            cfg.season = season.trim().to_string();
        }
        if let Some(grok_enabled) = nba.get("grok_enabled").and_then(|v| v.as_bool()) {
            cfg.grok_enabled = grok_enabled;
        }
        if let Some(grok_interval_secs) = nba.get("grok_interval_secs").and_then(|v| v.as_integer())
        {
            cfg.grok_interval_secs = grok_interval_secs.max(1) as u64;
        }
        if let Some(grok_min_edge) = decimal_from_toml(nba, "grok_min_edge") {
            cfg.grok_min_edge = grok_min_edge;
        }
        if let Some(grok_min_confidence) = float_from_toml(nba, "grok_min_confidence") {
            cfg.grok_min_confidence = grok_min_confidence;
        }
        if let Some(grok_decision_cooldown_secs) = nba
            .get("grok_decision_cooldown_secs")
            .and_then(|v| v.as_integer())
        {
            cfg.grok_decision_cooldown_secs = grok_decision_cooldown_secs.max(0) as u64;
        }
        if let Some(grok_fallback_enabled) =
            nba.get("grok_fallback_enabled").and_then(|v| v.as_bool())
        {
            cfg.grok_fallback_enabled = grok_fallback_enabled;
        }
        if let Some(min_reward_risk_ratio) = float_from_toml(nba, "min_reward_risk_ratio") {
            cfg.min_reward_risk_ratio = min_reward_risk_ratio;
        }
        if let Some(min_expected_value) = float_from_toml(nba, "min_expected_value") {
            cfg.min_expected_value = min_expected_value;
        }
        if let Some(kelly_fraction_cap) = float_from_toml(nba, "kelly_fraction_cap") {
            cfg.kelly_fraction_cap = kelly_fraction_cap;
        }
        if let Some(limit) = decimal_from_toml(nba, "performance_daily_loss_limit_usd") {
            cfg.performance_daily_loss_limit_usd = limit;
        }
        if let Some(value) = nba
            .get("performance_min_settled_trades")
            .and_then(|v| v.as_integer())
        {
            cfg.performance_min_settled_trades = value.max(0) as u64;
        }
        if let Some(value) = float_from_toml(nba, "performance_min_win_rate") {
            cfg.performance_min_win_rate = value;
        }
        if let Some(value) = float_from_toml(nba, "performance_low_winrate_multiplier") {
            cfg.performance_low_winrate_multiplier = value;
        }
        if let Some(value) = nba
            .get("performance_loss_streak_threshold")
            .and_then(|v| v.as_integer())
        {
            cfg.performance_loss_streak_threshold = value.max(0) as u32;
        }
        if let Some(value) = float_from_toml(nba, "performance_loss_streak_multiplier") {
            cfg.performance_loss_streak_multiplier = value;
        }
        if let Some(value) = nba.get("scaling_enabled").and_then(|v| v.as_bool()) {
            cfg.scaling_enabled = value;
        }
        if let Some(value) = nba.get("scaling_max_adds").and_then(|v| v.as_integer()) {
            cfg.scaling_max_adds = value.max(0) as u32;
        }
        if let Some(value) = float_from_toml(nba, "scaling_min_price_drop_pct") {
            cfg.scaling_min_price_drop_pct = value;
        }
        if let Some(value) = decimal_from_toml(nba, "scaling_max_game_exposure_usd") {
            cfg.scaling_max_game_exposure_usd = value;
        }
        if let Some(value) = float_from_toml(nba, "scaling_min_comeback_retention") {
            cfg.scaling_min_comeback_retention = value;
        }
        if let Some(value) = float_from_toml(nba, "scaling_min_time_remaining_mins") {
            cfg.scaling_min_time_remaining_mins = value;
        }
        if let Some(value) = nba.get("early_exit_enabled").and_then(|v| v.as_bool()) {
            cfg.early_exit_enabled = value;
        }
        if let Some(value) = float_from_toml(nba, "early_exit_take_profit_pct") {
            cfg.early_exit_take_profit_pct = value;
        }
        if let Some(value) = float_from_toml(nba, "early_exit_stop_loss_pct") {
            cfg.early_exit_stop_loss_pct = value;
        }

        let database_url = nba.get("database_url").and_then(|v| v.as_str());

        Self::from_config(id, cfg, dry_run, database_url)
    }

    pub fn with_market_registration(mut self, registration: NbaComebackMarketRegistration) -> Self {
        self.market_registrations.push(registration);
        self
    }

    pub fn build_actions_for_opportunity_for_test(
        &mut self,
        opp: &ComebackOpportunity,
        condition_id: Option<String>,
        now: DateTime<Utc>,
    ) -> Vec<StrategyAction> {
        self.build_actions_for_opportunity_inner(opp, condition_id, now)
    }
}

#[async_trait]
impl Strategy for NbaComebackStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        NBA_COMEBACK_STRATEGY_NAME
    }

    fn description(&self) -> &str {
        "NBA comeback strategy using ESPN Q3/Q4 game state"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![DataFeed::Tick {
            interval_ms: self.core.cfg.espn_poll_interval_secs.saturating_mul(1000),
        }]
    }

    async fn on_market_update(&mut self, _update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        Ok(Vec::new())
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        let Some(client_order_id) = update.client_order_id.as_deref() else {
            return Ok(Vec::new());
        };
        let Some(pending) = self.pending_orders.get_mut(client_order_id) else {
            return Ok(Vec::new());
        };

        let mut actions = Vec::new();
        if update.filled_qty > pending.accounted_filled_shares {
            let delta = update.filled_qty - pending.accounted_filled_shares;
            pending.accounted_filled_shares = update.filled_qty;
            let fill_price = update.avg_fill_price.unwrap_or(pending.limit_price);

            self.core.record_initial_entry_submission(
                &pending.game_id,
                &pending.token_id,
                fill_price * Decimal::from(delta),
            );
            self.core.record_position_entry_with_market_and_team(
                &pending.game_id,
                &pending.trailing_abbrev,
                &pending.market_slug,
                &pending.token_id,
                fill_price,
                delta,
                0.0,
            );

            let position = self
                .positions
                .entry(pending.token_id.clone())
                .or_insert_with(|| {
                    let mut info = PositionInfo::new(
                        pending.token_id.clone(),
                        Side::Up,
                        0,
                        fill_price,
                        self.id.clone(),
                    );
                    info.metadata
                        .insert("game_id".to_string(), pending.game_id.clone());
                    info.metadata
                        .insert("trailing_team".to_string(), pending.trailing_abbrev.clone());
                    info.metadata
                        .insert("market_slug".to_string(), pending.market_slug.clone());
                    if let Some(condition_id) = pending.condition_id.clone() {
                        info.metadata
                            .insert("condition_id".to_string(), condition_id);
                    }
                    info
                });
            let total_cost = position.entry_price * Decimal::from(position.shares)
                + fill_price * Decimal::from(delta);
            position.shares += delta;
            if position.shares > 0 {
                position.entry_price = total_cost / Decimal::from(position.shares);
            }
            position.current_price = Some(fill_price);

            actions.push(StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::OrderFilled,
                    format!(
                        "nba_comeback fill game={} token={} shares={} price={}",
                        pending.game_id, pending.token_id, delta, fill_price
                    ),
                )
                .with_data("game_id", &pending.game_id)
                .with_data("token_id", &pending.token_id)
                .with_data("filled_qty", delta.to_string())
                .with_data("fill_price", fill_price.to_string()),
            });
        }

        if matches!(
            update.status,
            OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Expired
                | OrderStatus::Failed
        ) {
            self.release_pending_order(client_order_id);
        }

        Ok(actions)
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        if !self.enabled {
            return Ok(Vec::new());
        }

        match self.collect_opportunities().await {
            Ok(opps) => {
                self.last_error = None;
                let Some((best, condition_id)) = opps
                    .iter()
                    .max_by(|(a, _), (b, _)| {
                        a.edge
                            .partial_cmp(&b.edge)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .cloned()
                else {
                    self.last_scan_at = Some(now);
                    return Ok(Vec::new());
                };

                Ok(self.build_actions_for_opportunity_inner(&best, condition_id, now))
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                Ok(vec![StrategyAction::Alert {
                    level: AlertLevel::Warning,
                    message: format!("{} scan failed: {}", self.id, error),
                }])
            }
        }
    }

    fn state(&self) -> StrategyStateInfo {
        let mut metrics = HashMap::new();
        metrics.insert(
            "tracked_markets".to_string(),
            self.market_registrations.len().to_string(),
        );
        metrics.insert(
            "reserved_notional_usd".to_string(),
            self.reserved_notional_usd.to_string(),
        );
        metrics.insert("stats_loaded".to_string(), self.stats_loaded.to_string());
        if let Some(last_scan_at) = self.last_scan_at {
            metrics.insert("last_scan_at".to_string(), last_scan_at.to_rfc3339());
        }
        if let Some(last_error) = self.last_error.as_ref() {
            metrics.insert("last_error".to_string(), last_error.clone());
        }

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: if self.enabled {
                "running".to_string()
            } else {
                "disabled".to_string()
            },
            enabled: self.enabled,
            active: !self.positions.is_empty() || !self.pending_orders.is_empty(),
            position_count: self.positions.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure: self
                .positions
                .values()
                .map(|position| position.entry_price * Decimal::from(position.shares))
                .sum(),
            unrealized_pnl: self
                .positions
                .values()
                .map(|position| position.unrealized_pnl)
                .sum(),
            realized_pnl_today: self.core.state.daily_realized_pnl_usd,
            last_update: self.last_scan_at.unwrap_or_else(Utc::now),
            metrics,
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.positions.values().cloned().collect()
    }

    fn is_active(&self) -> bool {
        self.enabled && (!self.positions.is_empty() || !self.pending_orders.is_empty())
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.enabled = false;
        Ok(vec![StrategyAction::Alert {
            level: AlertLevel::Info,
            message: format!("{} shutdown (dry_run={})", self.id, self.dry_run),
        }])
    }

    fn reset(&mut self) {
        self.pending_orders.clear();
        self.positions.clear();
        self.reserved_notional_usd = Decimal::ZERO;
        self.last_scan_at = None;
        self.last_error = None;
        self.stats_loaded = false;
        self.core.state = NbaComebackState::default();
    }
}

fn decimal_from_toml(config: &toml::Value, key: &str) -> Option<Decimal> {
    let value = config.get(key)?;
    if let Some(raw) = value.as_float() {
        Decimal::try_from(raw).ok()
    } else if let Some(raw) = value.as_integer() {
        Some(Decimal::from(raw))
    } else if let Some(raw) = value.as_str() {
        raw.parse::<Decimal>().ok()
    } else {
        None
    }
}

fn float_from_toml(config: &toml::Value, key: &str) -> Option<f64> {
    let value = config.get(key)?;
    if let Some(raw) = value.as_float() {
        Some(raw)
    } else if let Some(raw) = value.as_integer() {
        Some(raw as f64)
    } else if let Some(raw) = value.as_str() {
        raw.parse::<f64>().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::nba_comeback::espn::GameStatus;
    use rust_decimal_macros::dec;

    fn sample_game() -> LiveGame {
        LiveGame {
            espn_game_id: "401584701".to_string(),
            home_team: "Boston Celtics".to_string(),
            away_team: "Los Angeles Lakers".to_string(),
            home_abbrev: "BOS".to_string(),
            away_abbrev: "LAL".to_string(),
            home_score: 90,
            away_score: 80,
            quarter: 3,
            clock: "05:00".to_string(),
            time_remaining_mins: 17.0,
            status: GameStatus::InProgress,
            home_quarter_scores: Vec::new(),
            away_quarter_scores: Vec::new(),
        }
    }

    fn sample_opportunity() -> ComebackOpportunity {
        ComebackOpportunity {
            game: sample_game(),
            trailing_team: "Los Angeles Lakers".to_string(),
            trailing_abbrev: "LAL".to_string(),
            deficit: 10,
            comeback_rate: 0.20,
            adjusted_win_prob: 0.35,
            market_price: dec!(0.30),
            edge: 0.05,
            market_slug: "nba-lal-vs-bos".to_string(),
            token_id: "lal-win-yes".to_string(),
        }
    }

    fn strategy_from_toml(toml: &str) -> NbaComebackStrategy {
        NbaComebackStrategy::from_toml("nba-test".to_string(), toml, true).expect("strategy")
    }

    #[tokio::test]
    async fn strategy_from_config_builds_strategy() {
        let strategy = NbaComebackStrategy::from_config(
            "nba-test".to_string(),
            default_nba_comeback_config(),
            true,
            Some("postgres://localhost/unused"),
        )
        .expect("strategy");

        assert_eq!(strategy.name(), "nba_comeback");
        assert_eq!(strategy.id(), "nba-test");
    }

    #[tokio::test]
    async fn from_toml_builds_nba_strategy_and_overrides_config() {
        let toml = r#"
[strategy]
name = "nba_comeback"

[nba_comeback]
min_edge = 0.12
max_entry_price = 0.63
shares = 55
cooldown_secs = 900
max_daily_spend_usd = 125
min_deficit = 4
max_deficit = 12
target_quarter = 3
espn_poll_interval_secs = 45
min_comeback_rate = 0.18
season = "2025-26"
database_url = "postgres://localhost/unused"
"#;

        let strategy = strategy_from_toml(toml);

        assert_eq!(strategy.name(), "nba_comeback");
        assert!(matches!(
            strategy.required_feeds().as_slice(),
            [DataFeed::Tick {
                interval_ms: 45_000
            }]
        ));
        assert_eq!(strategy.core.cfg.min_edge, dec!(0.12));
        assert_eq!(strategy.core.cfg.max_entry_price, dec!(0.63));
        assert_eq!(strategy.core.cfg.shares, 55);
        assert_eq!(strategy.core.cfg.cooldown_secs, 900);
        assert_eq!(strategy.core.cfg.max_daily_spend_usd, dec!(125));
        assert_eq!(strategy.core.cfg.min_deficit, 4);
        assert_eq!(strategy.core.cfg.max_deficit, 12);
        assert_eq!(strategy.core.cfg.target_quarter, 3);
        assert!((strategy.core.cfg.min_comeback_rate - 0.18).abs() < f64::EPSILON);
        assert_eq!(strategy.core.cfg.season, "2025-26");
    }

    #[test]
    fn from_toml_rejects_non_nba_strategy_name() {
        let toml = r#"
[strategy]
name = "event_edge"

[nba_comeback]
database_url = "postgres://localhost/unused"
"#;

        let err = NbaComebackStrategy::from_toml("nba-test".to_string(), toml, true)
            .err()
            .expect("wrong strategy name should fail");
        assert!(err.to_string().contains("nba_comeback"));
    }

    #[tokio::test]
    async fn emits_canonical_submit_order_and_tracks_fill_into_position() {
        let toml = r#"
[strategy]
name = "nba_comeback"

[nba_comeback]
shares = 25
database_url = "postgres://localhost/unused"
"#;
        let mut strategy = strategy_from_toml(toml);
        let opp = sample_opportunity();
        let now = Utc::now();

        let actions =
            strategy.build_actions_for_opportunity_for_test(&opp, Some("cond-1".into()), now);

        let client_order_id = actions
            .iter()
            .find_map(|action| match action {
                StrategyAction::SubmitIntent { intent } => {
                    let order = crate::domain::order_request_from_strategy_intent(&intent);
                    assert_eq!(order.client_order_id, intent.client_order_id);
                    assert_eq!(
                        order.idempotency_key.as_deref(),
                        Some(intent.client_order_id.as_str())
                    );
                    Some(intent.client_order_id.clone())
                }
                _ => None,
            })
            .expect("submit order action");

        assert_eq!(strategy.pending_orders.len(), 1);

        strategy
            .on_order_update(&OrderUpdate {
                order_id: "exchange-1".to_string(),
                client_order_id: Some(client_order_id),
                status: OrderStatus::Filled,
                filled_qty: 25,
                avg_fill_price: Some(dec!(0.31)),
                timestamp: now,
                error: None,
            })
            .await
            .expect("fill update");

        assert!(strategy.pending_orders.is_empty());
        let positions = strategy.positions();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].token_id, "lal-win-yes");
        assert_eq!(positions[0].entry_price, dec!(0.31));
        assert_eq!(positions[0].shares, 25);
        assert_eq!(
            positions[0].metadata.get("game_id"),
            Some(&"401584701".to_string())
        );
        assert_eq!(
            positions[0].metadata.get("condition_id"),
            Some(&"cond-1".to_string())
        );
    }
}
