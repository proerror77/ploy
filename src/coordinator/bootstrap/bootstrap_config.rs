use rust_decimal::Decimal;
use tracing::info;

use crate::agents::OpenClawConfig;
use crate::config::AppConfig;
use crate::coordinator::config::DuplicateGuardScope;
use crate::coordinator::CoordinatorConfig;
use crate::strategy::CryptoTradingConfig;

use super::managed_crypto::{apply_managed_crypto_runtime_env, ManagedCryptoRuntimeConfig};
use super::runtime_config::{PoliticsRuntimeConfig, SportsRuntimeConfig};
use super::strategy_deployments::apply_strategy_deployments;
use super::support::{env_bool, env_decimal, env_decimal_opt, env_u64, load_strategy_deployments};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlatformBootstrapConfig {
    pub coordinator: CoordinatorConfig,
    pub enable_crypto: bool,
    #[serde(default)]
    pub enable_crypto_momentum: bool,
    #[serde(default)]
    pub enable_crypto_pattern_memory: bool,
    #[serde(default)]
    pub enable_crypto_split_arb: bool,
    pub enable_sports: bool,
    pub enable_politics: bool,
    #[serde(default)]
    pub enable_economics: bool,
    /// Enable OpenClaw meta-agent (Layer 3 orchestrator)
    #[serde(default)]
    pub enable_openclaw: bool,
    pub dry_run: bool,
    pub crypto: CryptoTradingConfig,
    #[serde(default, alias = "legacy_crypto")]
    pub managed_crypto: ManagedCryptoRuntimeConfig,
    pub sports: SportsRuntimeConfig,
    pub politics: PoliticsRuntimeConfig,
    /// OpenClaw meta-agent configuration
    #[serde(default)]
    pub openclaw: OpenClawConfig,
}

impl Default for PlatformBootstrapConfig {
    fn default() -> Self {
        Self {
            coordinator: CoordinatorConfig::default(),
            enable_crypto: true,
            enable_crypto_momentum: true,
            enable_crypto_pattern_memory: false,
            enable_crypto_split_arb: false,
            enable_sports: false,
            enable_politics: false,
            enable_economics: false,
            enable_openclaw: false,
            dry_run: true,
            crypto: CryptoTradingConfig::default(),
            managed_crypto: ManagedCryptoRuntimeConfig::default(),
            sports: SportsRuntimeConfig::default(),
            politics: PoliticsRuntimeConfig::default(),
            openclaw: OpenClawConfig::default(),
        }
    }
}

impl PlatformBootstrapConfig {
    /// Re-evaluate deployment matrix against the current runtime account + dry-run mode.
    pub fn reapply_strategy_deployments_for_runtime(&mut self, app: &AppConfig) {
        let strategy_deployments = load_strategy_deployments();
        if strategy_deployments.is_empty() {
            return;
        }

        let runtime_account_id = if app.account.id.trim().is_empty() {
            "default".to_string()
        } else {
            app.account.id.clone()
        };
        apply_strategy_deployments(
            self,
            &strategy_deployments,
            &runtime_account_id,
            self.dry_run,
        );
    }

    /// Build from AppConfig, enabling agents based on their config sections
    pub fn from_app_config(app: &AppConfig) -> Self {
        let mut cfg = Self::default();
        cfg.dry_run = app.dry_run.enabled;
        cfg.sports.account_id = app.account.id.clone();

        cfg.coordinator.risk = crate::platform::RiskConfig {
            max_platform_exposure: app.risk.max_single_exposure_usd,
            max_consecutive_failures: app.risk.max_consecutive_failures,
            daily_loss_limit: app.risk.daily_loss_limit_usd,
            max_spread_bps: 500,
            critical_bypass_exposure: false,
            ..Default::default()
        };
        cfg.coordinator.risk.max_drawdown_limit =
            env_decimal_opt("PLOY_RISK__MAX_DRAWDOWN_USD").map(|v| v.max(Decimal::ZERO));
        cfg.coordinator.risk.circuit_breaker_auto_recover = env_bool(
            "PLOY_RISK__CIRCUIT_BREAKER_AUTO_RECOVER",
            cfg.coordinator.risk.circuit_breaker_auto_recover,
        );
        cfg.coordinator.risk.circuit_breaker_cooldown_secs = env_u64(
            "PLOY_RISK__CIRCUIT_BREAKER_COOLDOWN_SECS",
            cfg.coordinator.risk.circuit_breaker_cooldown_secs,
        );

        let normalize_pct = |v: Decimal| {
            if v >= Decimal::ZERO && v <= Decimal::ONE {
                Some(v)
            } else {
                None
            }
        };

        let crypto_alloc_pct =
            env_decimal_opt("PLOY_RISK__CRYPTO_ALLOCATION_PCT").and_then(normalize_pct);
        let sports_alloc_pct =
            env_decimal_opt("PLOY_RISK__SPORTS_ALLOCATION_PCT").and_then(normalize_pct);
        let politics_alloc_pct =
            env_decimal_opt("PLOY_RISK__POLITICS_ALLOCATION_PCT").and_then(normalize_pct);
        let economics_alloc_pct =
            env_decimal_opt("PLOY_RISK__ECONOMICS_ALLOCATION_PCT").and_then(normalize_pct);

        let account_reserve_pct = env_decimal_opt("PLOY_RISK__ACCOUNT_RESERVE_PCT")
            .and_then(normalize_pct)
            .unwrap_or(Decimal::ZERO);
        let account_deployable_pct = env_decimal_opt("PLOY_RISK__ACCOUNT_DEPLOYABLE_PCT")
            .and_then(normalize_pct)
            .unwrap_or_else(|| Decimal::ONE - account_reserve_pct);
        let alloc_base = (cfg.coordinator.risk.max_platform_exposure * account_deployable_pct)
            .max(Decimal::ZERO);

        cfg.coordinator.risk.crypto_max_exposure =
            env_decimal_opt("PLOY_RISK__CRYPTO_MAX_EXPOSURE_USD")
                .or_else(|| crypto_alloc_pct.map(|p| alloc_base * p));
        cfg.coordinator.risk.sports_max_exposure =
            env_decimal_opt("PLOY_RISK__SPORTS_MAX_EXPOSURE_USD")
                .or_else(|| sports_alloc_pct.map(|p| alloc_base * p));
        cfg.coordinator.risk.politics_max_exposure =
            env_decimal_opt("PLOY_RISK__POLITICS_MAX_EXPOSURE_USD")
                .or_else(|| politics_alloc_pct.map(|p| alloc_base * p));
        cfg.coordinator.risk.economics_max_exposure =
            env_decimal_opt("PLOY_RISK__ECONOMICS_MAX_EXPOSURE_USD")
                .or_else(|| economics_alloc_pct.map(|p| alloc_base * p));

        cfg.coordinator.risk.crypto_daily_loss_limit =
            env_decimal_opt("PLOY_RISK__CRYPTO_DAILY_LOSS_LIMIT_USD");
        cfg.coordinator.risk.sports_daily_loss_limit =
            env_decimal_opt("PLOY_RISK__SPORTS_DAILY_LOSS_LIMIT_USD");
        cfg.coordinator.risk.politics_daily_loss_limit =
            env_decimal_opt("PLOY_RISK__POLITICS_DAILY_LOSS_LIMIT_USD");
        cfg.coordinator.risk.economics_daily_loss_limit =
            env_decimal_opt("PLOY_RISK__ECONOMICS_DAILY_LOSS_LIMIT_USD");

        cfg.coordinator.duplicate_guard_enabled = env_bool(
            "PLOY_COORDINATOR__DUPLICATE_GUARD_ENABLED",
            cfg.coordinator.duplicate_guard_enabled,
        );
        cfg.coordinator.duplicate_guard_window_ms = env_u64(
            "PLOY_COORDINATOR__DUPLICATE_GUARD_WINDOW_MS",
            cfg.coordinator.duplicate_guard_window_ms,
        )
        .max(100);
        if let Ok(raw) = std::env::var("PLOY_COORDINATOR__DUPLICATE_GUARD_SCOPE") {
            let v = raw.trim().to_ascii_lowercase();
            cfg.coordinator.duplicate_guard_scope = match v.as_str() {
                "deployment" | "dep" => DuplicateGuardScope::Deployment,
                "market" | "global" => DuplicateGuardScope::Market,
                _ => cfg.coordinator.duplicate_guard_scope,
            };
        }
        cfg.coordinator.heartbeat_stale_warn_cooldown_secs = env_u64(
            "PLOY_COORDINATOR__HEARTBEAT_STALE_WARN_COOLDOWN_SECS",
            cfg.coordinator.heartbeat_stale_warn_cooldown_secs,
        )
        .max(10);

        cfg.coordinator.crypto_allocator_enabled = env_bool(
            "PLOY_COORDINATOR__CRYPTO_ALLOCATOR_ENABLED",
            cfg.coordinator.crypto_allocator_enabled,
        );
        cfg.coordinator.crypto_allocator_total_cap_usd =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_ALLOCATOR_TOTAL_CAP_USD")
                .or(cfg.coordinator.crypto_allocator_total_cap_usd);

        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_COIN_CAP_BTC_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_coin_cap_btc_pct = v;
        }
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_COIN_CAP_ETH_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_coin_cap_eth_pct = v;
        }
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_COIN_CAP_SOL_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_coin_cap_sol_pct = v;
        }
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_COIN_CAP_XRP_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_coin_cap_xrp_pct = v;
        }
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_COIN_CAP_OTHER_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_coin_cap_other_pct = v;
        }

        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_HORIZON_CAP_5M_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_horizon_cap_5m_pct = v;
        }
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__CRYPTO_HORIZON_CAP_15M_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.crypto_horizon_cap_15m_pct = v;
        }
        if let Some(v) = env_decimal_opt("PLOY_COORDINATOR__CRYPTO_HORIZON_CAP_OTHER_PCT")
            .and_then(normalize_pct)
        {
            cfg.coordinator.crypto_horizon_cap_other_pct = v;
        }

        cfg.coordinator.sports_allocator_enabled = env_bool(
            "PLOY_COORDINATOR__SPORTS_ALLOCATOR_ENABLED",
            cfg.coordinator.sports_allocator_enabled,
        );
        cfg.coordinator.sports_allocator_total_cap_usd =
            env_decimal_opt("PLOY_COORDINATOR__SPORTS_ALLOCATOR_TOTAL_CAP_USD")
                .or(cfg.coordinator.sports_allocator_total_cap_usd);
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__SPORTS_MARKET_CAP_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.sports_market_cap_pct = v;
        }
        cfg.coordinator.sports_auto_split_by_active_markets = env_bool(
            "PLOY_COORDINATOR__SPORTS_AUTO_SPLIT_BY_ACTIVE_MARKETS",
            cfg.coordinator.sports_auto_split_by_active_markets,
        );

        cfg.coordinator.politics_allocator_enabled = env_bool(
            "PLOY_COORDINATOR__POLITICS_ALLOCATOR_ENABLED",
            cfg.coordinator.politics_allocator_enabled,
        );
        cfg.coordinator.politics_allocator_total_cap_usd =
            env_decimal_opt("PLOY_COORDINATOR__POLITICS_ALLOCATOR_TOTAL_CAP_USD")
                .or(cfg.coordinator.politics_allocator_total_cap_usd);
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__POLITICS_MARKET_CAP_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.politics_market_cap_pct = v;
        }
        cfg.coordinator.politics_auto_split_by_active_markets = env_bool(
            "PLOY_COORDINATOR__POLITICS_AUTO_SPLIT_BY_ACTIVE_MARKETS",
            cfg.coordinator.politics_auto_split_by_active_markets,
        );

        cfg.coordinator.economics_allocator_enabled = env_bool(
            "PLOY_COORDINATOR__ECONOMICS_ALLOCATOR_ENABLED",
            cfg.coordinator.economics_allocator_enabled,
        );
        cfg.coordinator.economics_allocator_total_cap_usd =
            env_decimal_opt("PLOY_COORDINATOR__ECONOMICS_ALLOCATOR_TOTAL_CAP_USD")
                .or(cfg.coordinator.economics_allocator_total_cap_usd);
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__ECONOMICS_MARKET_CAP_PCT").and_then(normalize_pct)
        {
            cfg.coordinator.economics_market_cap_pct = v;
        }
        cfg.coordinator.economics_auto_split_by_active_markets = env_bool(
            "PLOY_COORDINATOR__ECONOMICS_AUTO_SPLIT_BY_ACTIVE_MARKETS",
            cfg.coordinator.economics_auto_split_by_active_markets,
        );

        cfg.coordinator.governance_block_new_intents =
            std::env::var("PLOY_COORDINATOR__GOVERNANCE_BLOCK_NEW_INTENTS")
                .or_else(|_| std::env::var("PLOY_GOVERNANCE__BLOCK_NEW_INTENTS"))
                .ok()
                .map(|raw| match raw.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => true,
                    "0" | "false" | "no" | "off" => false,
                    _ => cfg.coordinator.governance_block_new_intents,
                })
                .unwrap_or(cfg.coordinator.governance_block_new_intents);
        cfg.coordinator.governance_max_intent_notional_usd =
            env_decimal_opt("PLOY_COORDINATOR__GOVERNANCE_MAX_INTENT_NOTIONAL_USD")
                .or_else(|| env_decimal_opt("PLOY_GOVERNANCE__MAX_INTENT_NOTIONAL_USD"))
                .or(cfg.coordinator.governance_max_intent_notional_usd);
        cfg.coordinator.governance_max_total_notional_usd =
            env_decimal_opt("PLOY_COORDINATOR__GOVERNANCE_MAX_TOTAL_NOTIONAL_USD")
                .or_else(|| env_decimal_opt("PLOY_GOVERNANCE__MAX_TOTAL_NOTIONAL_USD"))
                .or(cfg.coordinator.governance_max_total_notional_usd);

        if let Ok(raw) = std::env::var("PLOY_COORDINATOR__GOVERNANCE_BLOCKED_DOMAINS")
            .or_else(|_| std::env::var("PLOY_GOVERNANCE__BLOCKED_DOMAINS"))
        {
            let domains = raw
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.to_ascii_lowercase())
                .collect::<Vec<_>>();
            if !domains.is_empty() {
                cfg.coordinator.governance_blocked_domains = domains;
            }
        }
        cfg.coordinator.kelly_sizing_enabled = env_bool(
            "PLOY_COORDINATOR__KELLY_SIZING_ENABLED",
            cfg.coordinator.kelly_sizing_enabled,
        );
        if let Some(v) =
            env_decimal_opt("PLOY_COORDINATOR__KELLY_FRACTION_MULTIPLIER").and_then(normalize_pct)
        {
            cfg.coordinator.kelly_fraction_multiplier = v;
        }
        if let Some(v) = env_decimal_opt("PLOY_COORDINATOR__KELLY_MIN_EDGE") {
            cfg.coordinator.kelly_min_edge = v.max(Decimal::ZERO).min(Decimal::ONE);
        }
        cfg.coordinator.kelly_min_shares = env_u64(
            "PLOY_COORDINATOR__KELLY_MIN_SHARES",
            cfg.coordinator.kelly_min_shares,
        );

        cfg.coordinator.min_order_shares = env_u64(
            "PLOY_COORDINATOR__MIN_ORDER_SHARES",
            cfg.coordinator.min_order_shares,
        );
        if let Some(v) = env_decimal_opt("PLOY_COORDINATOR__MIN_ORDER_NOTIONAL_USD") {
            cfg.coordinator.min_order_notional_usd = v.max(Decimal::ZERO);
        }

        cfg.crypto.default_shares = app.strategy.shares.max(1);
        let effective_threshold = app.strategy.effective_sum_target();
        if effective_threshold > Decimal::ZERO {
            cfg.crypto.sum_threshold = effective_threshold;
        } else if app.strategy.sum_target > Decimal::ZERO {
            cfg.crypto.sum_threshold = app.strategy.sum_target;
        }
        cfg.crypto.exit_edge_floor = app.strategy.profit_buffer.max(Decimal::ZERO);
        cfg.crypto.risk_params.max_order_value = app.risk.max_single_exposure_usd;
        let max_positions = if app.risk.max_positions > 0 {
            app.risk.max_positions
        } else {
            3
        };
        cfg.crypto.risk_params.max_total_exposure =
            app.risk.max_single_exposure_usd * Decimal::from(max_positions);
        cfg.crypto.risk_params.max_daily_loss = app.risk.daily_loss_limit_usd;
        cfg.crypto.risk_params.max_unhedged_positions = app.risk.max_positions_per_symbol.max(1);

        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__ENABLED") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.enable_crypto_momentum = true,
                "0" | "false" | "no" | "off" => cfg.enable_crypto_momentum = false,
                _ => {}
            }
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__COINS") {
            let coins: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_uppercase())
                .collect();
            if !coins.is_empty() {
                cfg.crypto.coins = coins;
            }
        }
        cfg.crypto.sum_threshold =
            env_decimal("PLOY_CRYPTO_AGENT__SUM_THRESHOLD", cfg.crypto.sum_threshold);
        cfg.crypto.default_shares = env_u64(
            "PLOY_CRYPTO_AGENT__DEFAULT_SHARES",
            cfg.crypto.default_shares,
        )
        .max(1);
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__MIN_MOMENTUM_1S") {
            if let Ok(v) = raw.parse::<f64>() {
                if v.is_finite() && v >= 0.0 {
                    cfg.crypto.min_momentum_1s = v;
                }
            }
        }
        cfg.crypto.min_window_move_pct = env_decimal(
            "PLOY_CRYPTO_AGENT__MIN_WINDOW_MOVE_PCT",
            cfg.crypto.min_window_move_pct,
        );
        cfg.crypto.min_edge = env_decimal("PLOY_CRYPTO_AGENT__MIN_EDGE", cfg.crypto.min_edge);
        cfg.crypto.event_refresh_secs = env_u64(
            "PLOY_CRYPTO_AGENT__EVENT_REFRESH_SECS",
            cfg.crypto.event_refresh_secs,
        )
        .max(1);
        cfg.crypto.min_time_remaining_secs = env_u64(
            "PLOY_CRYPTO_AGENT__MIN_TIME_REMAINING_SECS",
            cfg.crypto.min_time_remaining_secs,
        );
        cfg.crypto.max_time_remaining_secs = env_u64(
            "PLOY_CRYPTO_AGENT__MAX_TIME_REMAINING_SECS",
            cfg.crypto.max_time_remaining_secs,
        );
        if cfg.crypto.max_time_remaining_secs < cfg.crypto.min_time_remaining_secs {
            cfg.crypto.max_time_remaining_secs = cfg.crypto.min_time_remaining_secs;
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__PREFER_CLOSE_TO_END") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto.prefer_close_to_end = true,
                "0" | "false" | "no" | "off" => cfg.crypto.prefer_close_to_end = false,
                _ => {}
            }
        }
        cfg.crypto.entry_cooldown_secs = env_u64(
            "PLOY_CRYPTO_AGENT__ENTRY_COOLDOWN_SECS",
            cfg.crypto.entry_cooldown_secs,
        );
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__REQUIRE_MTF_AGREEMENT") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto.require_mtf_agreement = true,
                "0" | "false" | "no" | "off" => cfg.crypto.require_mtf_agreement = false,
                _ => {}
            }
        }
        cfg.crypto.exit_edge_floor = env_decimal(
            "PLOY_CRYPTO_AGENT__EXIT_EDGE_FLOOR",
            cfg.crypto.exit_edge_floor,
        );
        cfg.crypto.exit_price_band = env_decimal(
            "PLOY_CRYPTO_AGENT__EXIT_PRICE_BAND",
            cfg.crypto.exit_price_band,
        );
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__ENABLE_PRICE_EXITS") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto.enable_price_exits = true,
                "0" | "false" | "no" | "off" => cfg.crypto.enable_price_exits = false,
                _ => {}
            }
        }
        cfg.crypto.min_hold_secs =
            env_u64("PLOY_CRYPTO_AGENT__MIN_HOLD_SECS", cfg.crypto.min_hold_secs);
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__ENTRY_MODE") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "arb_only" | "arb" => {
                    cfg.crypto.entry_mode = crate::strategy::CryptoEntryMode::ArbOnly
                }
                "directional" | "dir" => {
                    cfg.crypto.entry_mode = crate::strategy::CryptoEntryMode::Directional
                }
                "vol_straddle" | "straddle" => {
                    cfg.crypto.entry_mode = crate::strategy::CryptoEntryMode::VolStraddle
                }
                _ => {}
            }
        }
        cfg.crypto.oracle_lag_buffer_secs = env_u64(
            "PLOY_CRYPTO_AGENT__ORACLE_LAG_BUFFER_SECS",
            cfg.crypto.oracle_lag_buffer_secs,
        );
        cfg.crypto.max_spread_pct = env_decimal(
            "PLOY_CRYPTO_AGENT__MAX_SPREAD_PCT",
            cfg.crypto.max_spread_pct,
        );
        cfg.crypto.straddle_threshold = env_decimal(
            "PLOY_CRYPTO_AGENT__STRADDLE_THRESHOLD",
            cfg.crypto.straddle_threshold,
        );
        cfg.crypto.straddle_min_vol = env_decimal(
            "PLOY_CRYPTO_AGENT__STRADDLE_MIN_VOL",
            cfg.crypto.straddle_min_vol,
        );
        cfg.crypto.min_signal_score = env_decimal(
            "PLOY_CRYPTO_AGENT__MIN_SIGNAL_SCORE",
            cfg.crypto.min_signal_score,
        )
        .max(Decimal::ZERO)
        .min(Decimal::ONE);
        cfg.crypto.heartbeat_interval_secs = env_u64(
            "PLOY_CRYPTO_AGENT__HEARTBEAT_INTERVAL_SECS",
            cfg.crypto.heartbeat_interval_secs,
        )
        .max(1);
        cfg.crypto.risk_params.max_order_value = env_decimal(
            "PLOY_CRYPTO_AGENT__MAX_ORDER_VALUE_USD",
            cfg.crypto.risk_params.max_order_value,
        );
        cfg.crypto.risk_params.max_total_exposure = env_decimal(
            "PLOY_CRYPTO_AGENT__MAX_TOTAL_EXPOSURE_USD",
            cfg.crypto.risk_params.max_total_exposure,
        );
        cfg.crypto.risk_params.max_daily_loss = env_decimal(
            "PLOY_CRYPTO_AGENT__MAX_DAILY_LOSS_USD",
            cfg.crypto.risk_params.max_daily_loss,
        );
        cfg.crypto.risk_params.max_unhedged_positions = env_u64(
            "PLOY_CRYPTO_AGENT__MAX_UNHEDGED_POSITIONS",
            cfg.crypto.risk_params.max_unhedged_positions as u64,
        )
        .max(1) as u32;

        apply_managed_crypto_runtime_env(&cfg.crypto, &mut cfg.managed_crypto);

        if let Some(ref nba) = app.nba_comeback {
            if nba.enabled {
                cfg.enable_sports = true;
                cfg.sports.poll_interval_secs = nba.espn_poll_interval_secs;
            }
        }

        if let Some(ref ee) = app.event_edge_agent {
            if ee.enabled {
                cfg.enable_politics = true;
            }
        }

        cfg.reapply_strategy_deployments_for_runtime(app);

        if app.openclaw_runtime_lockdown() {
            cfg.enable_crypto = false;
            cfg.enable_crypto_momentum = false;
            cfg.enable_crypto_pattern_memory = false;
            cfg.enable_crypto_split_arb = false;
            cfg.managed_crypto.enable_lob_ml = false;
            #[cfg(feature = "rl")]
            {
                cfg.managed_crypto.enable_rl_policy = false;
            }
            cfg.enable_sports = false;
            cfg.enable_politics = false;
            cfg.enable_economics = false;
            info!(
                "agent framework lockdown active (mode=openclaw): built-in managed/legacy runtime loops are disabled"
            );
        }

        cfg
    }
}
