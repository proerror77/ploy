use super::support::env_decimal;
use super::*;

use crate::strategy::crypto_lob_ml::{
    CryptoLobMlConfig, CryptoLobMlEntrySidePolicy, CryptoLobMlExitMode,
};
#[cfg(feature = "rl")]
use crate::strategy::crypto_rl_policy::CryptoRlPolicyConfig;
use crate::strategy::CryptoTradingConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedCryptoRuntimeConfig {
    #[serde(default)]
    pub enable_lob_ml: bool,
    #[serde(default)]
    pub lob_ml: CryptoLobMlConfig,
    #[cfg(feature = "rl")]
    #[serde(default)]
    pub enable_rl_policy: bool,
    #[cfg(feature = "rl")]
    #[serde(default)]
    pub rl_policy: CryptoRlPolicyConfig,
}

impl Default for ManagedCryptoRuntimeConfig {
    fn default() -> Self {
        Self {
            enable_lob_ml: false,
            lob_ml: CryptoLobMlConfig::default(),
            #[cfg(feature = "rl")]
            enable_rl_policy: false,
            #[cfg(feature = "rl")]
            rl_policy: CryptoRlPolicyConfig::default(),
        }
    }
}

pub(super) fn apply_managed_crypto_runtime_env(
    crypto_cfg: &CryptoTradingConfig,
    managed_cfg: &mut ManagedCryptoRuntimeConfig,
) {
    apply_crypto_lob_ml_env(crypto_cfg, managed_cfg);

    #[cfg(feature = "rl")]
    apply_crypto_rl_policy_env(crypto_cfg, managed_cfg);
}

fn apply_crypto_lob_ml_env(
    crypto_cfg: &CryptoTradingConfig,
    managed_cfg: &mut ManagedCryptoRuntimeConfig,
) {
    managed_cfg.lob_ml.default_shares = crypto_cfg.default_shares;
    managed_cfg.lob_ml.exit_edge_floor = crypto_cfg.exit_edge_floor;
    managed_cfg.lob_ml.exit_price_band = crypto_cfg.exit_price_band;
    managed_cfg.lob_ml.risk_params = crypto_cfg.risk_params.clone();

    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__ENABLED") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => managed_cfg.enable_lob_ml = true,
            "0" | "false" | "no" | "off" => managed_cfg.enable_lob_ml = false,
            _ => {}
        }
    }

    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__COINS") {
        let coins: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_uppercase())
            .collect();
        if !coins.is_empty() {
            managed_cfg.lob_ml.coins = coins;
        }
    }

    managed_cfg.lob_ml.default_shares = env_u64(
        "PLOY_CRYPTO_LOB_ML__DEFAULT_SHARES",
        managed_cfg.lob_ml.default_shares,
    )
    .max(1);
    managed_cfg.lob_ml.exit_edge_floor = env_decimal(
        "PLOY_CRYPTO_LOB_ML__EXIT_EDGE_FLOOR",
        managed_cfg.lob_ml.exit_edge_floor,
    );
    managed_cfg.lob_ml.exit_price_band = env_decimal(
        "PLOY_CRYPTO_LOB_ML__EXIT_PRICE_BAND",
        managed_cfg.lob_ml.exit_price_band,
    );
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__EXIT_MODE") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "settle_only" | "settle" => {
                managed_cfg.lob_ml.exit_mode = CryptoLobMlExitMode::SettleOnly
            }
            "ev_exit" | "ev" | "model_ev" => {
                managed_cfg.lob_ml.exit_mode = CryptoLobMlExitMode::EvExit
            }
            "signal_flip" | "flip" => {
                managed_cfg.lob_ml.exit_mode = CryptoLobMlExitMode::SignalFlip
            }
            "trailing_exit" | "trailing" | "price_exit" | "price" | "mtm" => {
                managed_cfg.lob_ml.exit_mode = CryptoLobMlExitMode::TrailingExit
            }
            _ => {
                warn!(
                    value = %raw,
                    "invalid PLOY_CRYPTO_LOB_ML__EXIT_MODE; keeping configured/default value"
                );
            }
        }
    }
    if std::env::var_os("PLOY_CRYPTO_LOB_ML__ENABLE_PRICE_EXITS").is_some() {
        warn!(
            "PLOY_CRYPTO_LOB_ML__ENABLE_PRICE_EXITS is deprecated and ignored; use PLOY_CRYPTO_LOB_ML__EXIT_MODE"
        );
    }
    managed_cfg.lob_ml.min_hold_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__MIN_HOLD_SECS",
        managed_cfg.lob_ml.min_hold_secs,
    );
    managed_cfg.lob_ml.min_edge =
        env_decimal("PLOY_CRYPTO_LOB_ML__MIN_EDGE", managed_cfg.lob_ml.min_edge);
    managed_cfg.lob_ml.max_entry_price = env_decimal(
        "PLOY_CRYPTO_LOB_ML__MAX_ENTRY_PRICE",
        managed_cfg.lob_ml.max_entry_price,
    );
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__ENTRY_SIDE_POLICY") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "best_ev" | "best" => {
                managed_cfg.lob_ml.entry_side_policy = CryptoLobMlEntrySidePolicy::BestEv
            }
            "lagging_only" | "lagging" => {
                managed_cfg.lob_ml.entry_side_policy = CryptoLobMlEntrySidePolicy::LaggingOnly
            }
            _ => {}
        }
    }
    managed_cfg.lob_ml.entry_late_window_secs_5m = env_u64(
        "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M",
        managed_cfg.lob_ml.entry_late_window_secs_5m,
    )
    .min(300);
    if std::env::var_os("PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M").is_none()
        && std::env::var_os("PLOY_CRYPTO_LOB_ML__ENTRY_EARLY_WINDOW_SECS_5M").is_some()
    {
        warn!(
            "PLOY_CRYPTO_LOB_ML__ENTRY_EARLY_WINDOW_SECS_5M is deprecated; use PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M"
        );
        managed_cfg.lob_ml.entry_late_window_secs_5m = env_u64(
            "PLOY_CRYPTO_LOB_ML__ENTRY_EARLY_WINDOW_SECS_5M",
            managed_cfg.lob_ml.entry_late_window_secs_5m,
        )
        .min(300);
    }
    managed_cfg.lob_ml.entry_late_window_secs_15m = env_u64(
        "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_15M",
        managed_cfg.lob_ml.entry_late_window_secs_15m,
    )
    .min(900);
    managed_cfg.lob_ml.taker_fee_rate = env_decimal(
        "PLOY_CRYPTO_LOB_ML__TAKER_FEE_RATE",
        managed_cfg.lob_ml.taker_fee_rate,
    )
    .max(rust_decimal::Decimal::ZERO)
    .min(rust_decimal::Decimal::new(25, 2));
    managed_cfg.lob_ml.entry_slippage_bps = env_decimal(
        "PLOY_CRYPTO_LOB_ML__ENTRY_SLIPPAGE_BPS",
        managed_cfg.lob_ml.entry_slippage_bps,
    )
    .max(rust_decimal::Decimal::ZERO)
    .min(rust_decimal::Decimal::new(2500, 0));
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__USE_PRICE_TO_BEAT") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => managed_cfg.lob_ml.use_price_to_beat = true,
            "0" | "false" | "no" | "off" => managed_cfg.lob_ml.use_price_to_beat = false,
            _ => {}
        }
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__REQUIRE_PRICE_TO_BEAT") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => managed_cfg.lob_ml.require_price_to_beat = true,
            "0" | "false" | "no" | "off" => managed_cfg.lob_ml.require_price_to_beat = false,
            _ => {}
        }
    }
    managed_cfg.lob_ml.model_blend_weight = env_decimal(
        "PLOY_CRYPTO_LOB_ML__MODEL_BLEND_WEIGHT",
        managed_cfg.lob_ml.model_blend_weight,
    )
    .max(rust_decimal::Decimal::new(1, 2))
    .min(rust_decimal::Decimal::new(99, 2));
    managed_cfg.lob_ml.min_direction_strength = env_decimal(
        "PLOY_CRYPTO_LOB_ML__MIN_DIRECTION_STRENGTH",
        managed_cfg.lob_ml.min_direction_strength,
    )
    .max(rust_decimal::Decimal::ZERO)
    .min(rust_decimal::Decimal::new(49, 2));
    managed_cfg.lob_ml.event_refresh_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__EVENT_REFRESH_SECS",
        managed_cfg.lob_ml.event_refresh_secs,
    )
    .max(1);
    managed_cfg.lob_ml.min_time_remaining_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__MIN_TIME_REMAINING_SECS",
        managed_cfg.lob_ml.min_time_remaining_secs,
    );
    managed_cfg.lob_ml.max_time_remaining_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__MAX_TIME_REMAINING_SECS",
        managed_cfg.lob_ml.max_time_remaining_secs,
    );
    managed_cfg.lob_ml.max_time_remaining_secs_5m = env_u64(
        "PLOY_CRYPTO_LOB_ML__MAX_TIME_REMAINING_SECS_5M",
        managed_cfg.lob_ml.max_time_remaining_secs_5m,
    )
    .max(1);
    managed_cfg.lob_ml.max_time_remaining_secs_15m = env_u64(
        "PLOY_CRYPTO_LOB_ML__MAX_TIME_REMAINING_SECS_15M",
        managed_cfg.lob_ml.max_time_remaining_secs_15m,
    )
    .max(1);
    if managed_cfg.lob_ml.max_time_remaining_secs < managed_cfg.lob_ml.min_time_remaining_secs {
        managed_cfg.lob_ml.max_time_remaining_secs = managed_cfg.lob_ml.min_time_remaining_secs;
    }
    if managed_cfg.lob_ml.max_time_remaining_secs_5m < managed_cfg.lob_ml.min_time_remaining_secs {
        managed_cfg.lob_ml.max_time_remaining_secs_5m = managed_cfg.lob_ml.min_time_remaining_secs;
    }
    if managed_cfg.lob_ml.max_time_remaining_secs_15m < managed_cfg.lob_ml.min_time_remaining_secs {
        managed_cfg.lob_ml.max_time_remaining_secs_15m = managed_cfg.lob_ml.min_time_remaining_secs;
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__PREFER_CLOSE_TO_END") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => managed_cfg.lob_ml.prefer_close_to_end = true,
            "0" | "false" | "no" | "off" => managed_cfg.lob_ml.prefer_close_to_end = false,
            _ => {}
        }
    }
    managed_cfg.lob_ml.cooldown_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__COOLDOWN_SECS",
        managed_cfg.lob_ml.cooldown_secs,
    );
    managed_cfg.lob_ml.max_lob_snapshot_age_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__MAX_LOB_SNAPSHOT_AGE_SECS",
        managed_cfg.lob_ml.max_lob_snapshot_age_secs,
    )
    .max(1);
    managed_cfg.lob_ml.heartbeat_interval_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__HEARTBEAT_INTERVAL_SECS",
        managed_cfg.lob_ml.heartbeat_interval_secs,
    )
    .max(1);
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_TYPE") {
        let v = raw.trim().to_ascii_lowercase();
        if !v.is_empty() {
            managed_cfg.lob_ml.model_type = v;
        }
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_PATH") {
        let v = raw.trim();
        managed_cfg.lob_ml.model_path = if v.is_empty() {
            None
        } else {
            Some(v.to_string())
        };
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_VERSION") {
        let v = raw.trim();
        managed_cfg.lob_ml.model_version = if v.is_empty() {
            None
        } else {
            Some(v.to_string())
        };
    }
    managed_cfg.lob_ml.ev_exit_buffer = env_decimal(
        "PLOY_CRYPTO_LOB_ML__EV_EXIT_BUFFER",
        managed_cfg.lob_ml.ev_exit_buffer,
    )
    .max(rust_decimal::Decimal::ZERO)
    .min(rust_decimal::Decimal::new(50, 2));
    managed_cfg.lob_ml.ev_exit_vol_scale = env_decimal(
        "PLOY_CRYPTO_LOB_ML__EV_EXIT_VOL_SCALE",
        managed_cfg.lob_ml.ev_exit_vol_scale,
    )
    .max(rust_decimal::Decimal::ZERO)
    .min(rust_decimal::Decimal::new(50, 2));
    managed_cfg.lob_ml.oracle_lag_buffer_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__ORACLE_LAG_BUFFER_SECS",
        managed_cfg.lob_ml.oracle_lag_buffer_secs,
    );
    managed_cfg.lob_ml.max_spread_pct = env_decimal(
        "PLOY_CRYPTO_LOB_ML__MAX_SPREAD_PCT",
        managed_cfg.lob_ml.max_spread_pct,
    );
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__FORCE_SETTLE_ONLY_5M") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => managed_cfg.lob_ml.force_settle_only_5m = true,
            "0" | "false" | "no" | "off" => managed_cfg.lob_ml.force_settle_only_5m = false,
            _ => {}
        }
    }
}

#[cfg(feature = "rl")]
fn apply_crypto_rl_policy_env(
    crypto_cfg: &CryptoTradingConfig,
    managed_cfg: &mut ManagedCryptoRuntimeConfig,
) {
    managed_cfg.rl_policy.default_shares = crypto_cfg.default_shares;
    managed_cfg.rl_policy.risk_params = crypto_cfg.risk_params.clone();
    managed_cfg.rl_policy.heartbeat_interval_secs = crypto_cfg.heartbeat_interval_secs;

    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__ENABLED") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => managed_cfg.enable_rl_policy = true,
            "0" | "false" | "no" | "off" => managed_cfg.enable_rl_policy = false,
            _ => {}
        }
    }

    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__COINS") {
        let coins: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_uppercase())
            .collect();
        if !coins.is_empty() {
            managed_cfg.rl_policy.coins = coins;
        }
    }

    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__MODEL_PATH") {
        let v = raw.trim();
        if !v.is_empty() {
            managed_cfg.rl_policy.policy_model_path = Some(v.to_string());
        }
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__POLICY_OUTPUT") {
        let v = raw.trim().to_ascii_lowercase();
        if !v.is_empty() {
            managed_cfg.rl_policy.policy_output = v;
        }
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__MODEL_VERSION") {
        let v = raw.trim();
        if !v.is_empty() {
            managed_cfg.rl_policy.policy_model_version = Some(v.to_string());
        }
    }

    managed_cfg.rl_policy.default_shares = env_u64(
        "PLOY_CRYPTO_RL_POLICY__DEFAULT_SHARES",
        managed_cfg.rl_policy.default_shares,
    )
    .max(1);
    managed_cfg.rl_policy.max_entry_price = env_decimal(
        "PLOY_CRYPTO_RL_POLICY__MAX_ENTRY_PRICE",
        managed_cfg.rl_policy.max_entry_price,
    );
    managed_cfg.rl_policy.cooldown_secs = env_u64(
        "PLOY_CRYPTO_RL_POLICY__COOLDOWN_SECS",
        managed_cfg.rl_policy.cooldown_secs,
    );
    managed_cfg.rl_policy.max_lob_snapshot_age_secs = env_u64(
        "PLOY_CRYPTO_RL_POLICY__MAX_LOB_SNAPSHOT_AGE_SECS",
        managed_cfg.rl_policy.max_lob_snapshot_age_secs,
    )
    .max(1);
    managed_cfg.rl_policy.decision_interval_ms = env_u64(
        "PLOY_CRYPTO_RL_POLICY__DECISION_INTERVAL_MS",
        managed_cfg.rl_policy.decision_interval_ms,
    )
    .max(50);
    managed_cfg.rl_policy.observation_version = env_u64(
        "PLOY_CRYPTO_RL_POLICY__OBS_VERSION",
        managed_cfg.rl_policy.observation_version as u64,
    ) as u32;
    managed_cfg.rl_policy.event_refresh_secs = env_u64(
        "PLOY_CRYPTO_RL_POLICY__EVENT_REFRESH_SECS",
        managed_cfg.rl_policy.event_refresh_secs,
    )
    .max(1);
    managed_cfg.rl_policy.min_time_remaining_secs = env_u64(
        "PLOY_CRYPTO_RL_POLICY__MIN_TIME_REMAINING_SECS",
        managed_cfg.rl_policy.min_time_remaining_secs,
    );
    managed_cfg.rl_policy.max_time_remaining_secs = env_u64(
        "PLOY_CRYPTO_RL_POLICY__MAX_TIME_REMAINING_SECS",
        managed_cfg.rl_policy.max_time_remaining_secs,
    );
    if managed_cfg.rl_policy.max_time_remaining_secs < managed_cfg.rl_policy.min_time_remaining_secs
    {
        managed_cfg.rl_policy.max_time_remaining_secs =
            managed_cfg.rl_policy.min_time_remaining_secs;
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__PREFER_CLOSE_TO_END") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => managed_cfg.rl_policy.prefer_close_to_end = true,
            "0" | "false" | "no" | "off" => managed_cfg.rl_policy.prefer_close_to_end = false,
            _ => {}
        }
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__EXPLORATION_RATE") {
        if let Ok(v) = raw.trim().parse::<f32>() {
            if v.is_finite() {
                managed_cfg.rl_policy.exploration_rate = v.clamp(0.0, 1.0);
            }
        }
    }
    managed_cfg.rl_policy.heartbeat_interval_secs = env_u64(
        "PLOY_CRYPTO_RL_POLICY__HEARTBEAT_INTERVAL_SECS",
        managed_cfg.rl_policy.heartbeat_interval_secs,
    )
    .max(1);
}
