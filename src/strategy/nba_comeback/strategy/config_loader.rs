use super::{NBA_COMEBACK_STRATEGY_NAME, NbaComebackStrategy};
use crate::config::NbaComebackConfig;
use crate::error::Result;
use crate::strategy::nba_comeback::comeback_stats::ComebackStatsProvider;
use crate::strategy::nba_comeback::core::NbaComebackCore;
use anyhow::anyhow;
use rust_decimal::Decimal;
use sqlx::postgres::PgPoolOptions;
use toml::Value;

const DEFAULT_DATABASE_URL: &str = "postgres://localhost/unused";

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

impl NbaComebackStrategy {
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
}

fn decimal_from_toml(config: &Value, key: &str) -> Option<Decimal> {
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

fn float_from_toml(config: &Value, key: &str) -> Option<f64> {
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
