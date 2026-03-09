use super::*;

use crate::agents::CryptoTradingConfig;
use crate::agents::context::AgentContext;
use crate::agents::crypto_lob_ml::{
    CryptoLobMlAgent, CryptoLobMlConfig, CryptoLobMlEntrySidePolicy, CryptoLobMlExitMode,
};
#[cfg(feature = "rl")]
use crate::agents::crypto_rl_policy::{CryptoRlPolicyAgent, CryptoRlPolicyConfig};
use crate::agents::traits::TradingAgent;
use crate::collector::LobCache;
use crate::strategy::momentum::EventMatcher;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyCryptoRuntimeConfig {
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

impl Default for LegacyCryptoRuntimeConfig {
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

pub(super) fn apply_legacy_crypto_agent_env(
    crypto_cfg: &CryptoTradingConfig,
    legacy_cfg: &mut LegacyCryptoRuntimeConfig,
) {
    apply_crypto_lob_ml_env(crypto_cfg, legacy_cfg);

    #[cfg(feature = "rl")]
    apply_crypto_rl_policy_env(crypto_cfg, legacy_cfg);
}

fn apply_crypto_lob_ml_env(
    crypto_cfg: &CryptoTradingConfig,
    legacy_cfg: &mut LegacyCryptoRuntimeConfig,
) {
    legacy_cfg.lob_ml.default_shares = crypto_cfg.default_shares;
    legacy_cfg.lob_ml.exit_edge_floor = crypto_cfg.exit_edge_floor;
    legacy_cfg.lob_ml.exit_price_band = crypto_cfg.exit_price_band;
    legacy_cfg.lob_ml.risk_params = crypto_cfg.risk_params.clone();

    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__ENABLED") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => legacy_cfg.enable_lob_ml = true,
            "0" | "false" | "no" | "off" => legacy_cfg.enable_lob_ml = false,
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
            legacy_cfg.lob_ml.coins = coins;
        }
    }

    legacy_cfg.lob_ml.default_shares = env_u64(
        "PLOY_CRYPTO_LOB_ML__DEFAULT_SHARES",
        legacy_cfg.lob_ml.default_shares,
    )
    .max(1);
    legacy_cfg.lob_ml.exit_edge_floor = env_decimal(
        "PLOY_CRYPTO_LOB_ML__EXIT_EDGE_FLOOR",
        legacy_cfg.lob_ml.exit_edge_floor,
    );
    legacy_cfg.lob_ml.exit_price_band = env_decimal(
        "PLOY_CRYPTO_LOB_ML__EXIT_PRICE_BAND",
        legacy_cfg.lob_ml.exit_price_band,
    );
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__EXIT_MODE") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "settle_only" | "settle" => {
                legacy_cfg.lob_ml.exit_mode = CryptoLobMlExitMode::SettleOnly
            }
            "ev_exit" | "ev" | "model_ev" => {
                legacy_cfg.lob_ml.exit_mode = CryptoLobMlExitMode::EvExit
            }
            "signal_flip" | "flip" => {
                legacy_cfg.lob_ml.exit_mode = CryptoLobMlExitMode::SignalFlip
            }
            "trailing_exit" | "trailing" | "price_exit" | "price" | "mtm" => {
                legacy_cfg.lob_ml.exit_mode = CryptoLobMlExitMode::TrailingExit
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
    legacy_cfg.lob_ml.min_hold_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__MIN_HOLD_SECS",
        legacy_cfg.lob_ml.min_hold_secs,
    );
    legacy_cfg.lob_ml.min_edge =
        env_decimal("PLOY_CRYPTO_LOB_ML__MIN_EDGE", legacy_cfg.lob_ml.min_edge);
    legacy_cfg.lob_ml.max_entry_price = env_decimal(
        "PLOY_CRYPTO_LOB_ML__MAX_ENTRY_PRICE",
        legacy_cfg.lob_ml.max_entry_price,
    );
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__ENTRY_SIDE_POLICY") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "best_ev" | "best" => {
                legacy_cfg.lob_ml.entry_side_policy = CryptoLobMlEntrySidePolicy::BestEv
            }
            "lagging_only" | "lagging" => {
                legacy_cfg.lob_ml.entry_side_policy = CryptoLobMlEntrySidePolicy::LaggingOnly
            }
            _ => {}
        }
    }
    legacy_cfg.lob_ml.entry_late_window_secs_5m = env_u64(
        "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M",
        legacy_cfg.lob_ml.entry_late_window_secs_5m,
    )
    .min(300);
    if std::env::var_os("PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M").is_none()
        && std::env::var_os("PLOY_CRYPTO_LOB_ML__ENTRY_EARLY_WINDOW_SECS_5M").is_some()
    {
        warn!(
            "PLOY_CRYPTO_LOB_ML__ENTRY_EARLY_WINDOW_SECS_5M is deprecated; use PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M"
        );
        legacy_cfg.lob_ml.entry_late_window_secs_5m = env_u64(
            "PLOY_CRYPTO_LOB_ML__ENTRY_EARLY_WINDOW_SECS_5M",
            legacy_cfg.lob_ml.entry_late_window_secs_5m,
        )
        .min(300);
    }
    legacy_cfg.lob_ml.entry_late_window_secs_15m = env_u64(
        "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_15M",
        legacy_cfg.lob_ml.entry_late_window_secs_15m,
    )
    .min(900);
    legacy_cfg.lob_ml.taker_fee_rate = env_decimal(
        "PLOY_CRYPTO_LOB_ML__TAKER_FEE_RATE",
        legacy_cfg.lob_ml.taker_fee_rate,
    )
    .max(rust_decimal::Decimal::ZERO)
    .min(rust_decimal::Decimal::new(25, 2));
    legacy_cfg.lob_ml.entry_slippage_bps = env_decimal(
        "PLOY_CRYPTO_LOB_ML__ENTRY_SLIPPAGE_BPS",
        legacy_cfg.lob_ml.entry_slippage_bps,
    )
    .max(rust_decimal::Decimal::ZERO)
    .min(rust_decimal::Decimal::new(2500, 0));
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__USE_PRICE_TO_BEAT") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => legacy_cfg.lob_ml.use_price_to_beat = true,
            "0" | "false" | "no" | "off" => legacy_cfg.lob_ml.use_price_to_beat = false,
            _ => {}
        }
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__REQUIRE_PRICE_TO_BEAT") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => legacy_cfg.lob_ml.require_price_to_beat = true,
            "0" | "false" | "no" | "off" => legacy_cfg.lob_ml.require_price_to_beat = false,
            _ => {}
        }
    }
    legacy_cfg.lob_ml.model_blend_weight = env_decimal(
        "PLOY_CRYPTO_LOB_ML__MODEL_BLEND_WEIGHT",
        legacy_cfg.lob_ml.model_blend_weight,
    )
    .max(rust_decimal::Decimal::new(1, 2))
    .min(rust_decimal::Decimal::new(99, 2));
    legacy_cfg.lob_ml.min_direction_strength = env_decimal(
        "PLOY_CRYPTO_LOB_ML__MIN_DIRECTION_STRENGTH",
        legacy_cfg.lob_ml.min_direction_strength,
    )
    .max(rust_decimal::Decimal::ZERO)
    .min(rust_decimal::Decimal::new(49, 2));
    legacy_cfg.lob_ml.event_refresh_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__EVENT_REFRESH_SECS",
        legacy_cfg.lob_ml.event_refresh_secs,
    )
    .max(1);
    legacy_cfg.lob_ml.min_time_remaining_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__MIN_TIME_REMAINING_SECS",
        legacy_cfg.lob_ml.min_time_remaining_secs,
    );
    legacy_cfg.lob_ml.max_time_remaining_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__MAX_TIME_REMAINING_SECS",
        legacy_cfg.lob_ml.max_time_remaining_secs,
    );
    legacy_cfg.lob_ml.max_time_remaining_secs_5m = env_u64(
        "PLOY_CRYPTO_LOB_ML__MAX_TIME_REMAINING_SECS_5M",
        legacy_cfg.lob_ml.max_time_remaining_secs_5m,
    )
    .max(1);
    legacy_cfg.lob_ml.max_time_remaining_secs_15m = env_u64(
        "PLOY_CRYPTO_LOB_ML__MAX_TIME_REMAINING_SECS_15M",
        legacy_cfg.lob_ml.max_time_remaining_secs_15m,
    )
    .max(1);
    if legacy_cfg.lob_ml.max_time_remaining_secs < legacy_cfg.lob_ml.min_time_remaining_secs {
        legacy_cfg.lob_ml.max_time_remaining_secs = legacy_cfg.lob_ml.min_time_remaining_secs;
    }
    if legacy_cfg.lob_ml.max_time_remaining_secs_5m < legacy_cfg.lob_ml.min_time_remaining_secs {
        legacy_cfg.lob_ml.max_time_remaining_secs_5m = legacy_cfg.lob_ml.min_time_remaining_secs;
    }
    if legacy_cfg.lob_ml.max_time_remaining_secs_15m < legacy_cfg.lob_ml.min_time_remaining_secs {
        legacy_cfg.lob_ml.max_time_remaining_secs_15m = legacy_cfg.lob_ml.min_time_remaining_secs;
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__PREFER_CLOSE_TO_END") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => legacy_cfg.lob_ml.prefer_close_to_end = true,
            "0" | "false" | "no" | "off" => legacy_cfg.lob_ml.prefer_close_to_end = false,
            _ => {}
        }
    }
    legacy_cfg.lob_ml.cooldown_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__COOLDOWN_SECS",
        legacy_cfg.lob_ml.cooldown_secs,
    );
    legacy_cfg.lob_ml.max_lob_snapshot_age_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__MAX_LOB_SNAPSHOT_AGE_SECS",
        legacy_cfg.lob_ml.max_lob_snapshot_age_secs,
    )
    .max(1);
    legacy_cfg.lob_ml.heartbeat_interval_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__HEARTBEAT_INTERVAL_SECS",
        legacy_cfg.lob_ml.heartbeat_interval_secs,
    )
    .max(1);
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_TYPE") {
        let v = raw.trim().to_ascii_lowercase();
        if !v.is_empty() {
            legacy_cfg.lob_ml.model_type = v;
        }
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_PATH") {
        let v = raw.trim();
        legacy_cfg.lob_ml.model_path = if v.is_empty() {
            None
        } else {
            Some(v.to_string())
        };
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_VERSION") {
        let v = raw.trim();
        legacy_cfg.lob_ml.model_version = if v.is_empty() {
            None
        } else {
            Some(v.to_string())
        };
    }
    legacy_cfg.lob_ml.ev_exit_buffer = env_decimal(
        "PLOY_CRYPTO_LOB_ML__EV_EXIT_BUFFER",
        legacy_cfg.lob_ml.ev_exit_buffer,
    )
    .max(rust_decimal::Decimal::ZERO)
    .min(rust_decimal::Decimal::new(50, 2));
    legacy_cfg.lob_ml.ev_exit_vol_scale = env_decimal(
        "PLOY_CRYPTO_LOB_ML__EV_EXIT_VOL_SCALE",
        legacy_cfg.lob_ml.ev_exit_vol_scale,
    )
    .max(rust_decimal::Decimal::ZERO)
    .min(rust_decimal::Decimal::new(50, 2));
    legacy_cfg.lob_ml.oracle_lag_buffer_secs = env_u64(
        "PLOY_CRYPTO_LOB_ML__ORACLE_LAG_BUFFER_SECS",
        legacy_cfg.lob_ml.oracle_lag_buffer_secs,
    );
    legacy_cfg.lob_ml.max_spread_pct = env_decimal(
        "PLOY_CRYPTO_LOB_ML__MAX_SPREAD_PCT",
        legacy_cfg.lob_ml.max_spread_pct,
    );
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__FORCE_SETTLE_ONLY_5M") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => legacy_cfg.lob_ml.force_settle_only_5m = true,
            "0" | "false" | "no" | "off" => legacy_cfg.lob_ml.force_settle_only_5m = false,
            _ => {}
        }
    }
}

#[cfg(feature = "rl")]
fn apply_crypto_rl_policy_env(
    crypto_cfg: &CryptoTradingConfig,
    legacy_cfg: &mut LegacyCryptoRuntimeConfig,
) {
    legacy_cfg.rl_policy.default_shares = crypto_cfg.default_shares;
    legacy_cfg.rl_policy.risk_params = crypto_cfg.risk_params.clone();
    legacy_cfg.rl_policy.heartbeat_interval_secs = crypto_cfg.heartbeat_interval_secs;

    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__ENABLED") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => legacy_cfg.enable_rl_policy = true,
            "0" | "false" | "no" | "off" => legacy_cfg.enable_rl_policy = false,
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
            legacy_cfg.rl_policy.coins = coins;
        }
    }

    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__MODEL_PATH") {
        let v = raw.trim();
        if !v.is_empty() {
            legacy_cfg.rl_policy.policy_model_path = Some(v.to_string());
        }
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__POLICY_OUTPUT") {
        let v = raw.trim().to_ascii_lowercase();
        if !v.is_empty() {
            legacy_cfg.rl_policy.policy_output = v;
        }
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__MODEL_VERSION") {
        let v = raw.trim();
        if !v.is_empty() {
            legacy_cfg.rl_policy.policy_model_version = Some(v.to_string());
        }
    }

    legacy_cfg.rl_policy.default_shares = env_u64(
        "PLOY_CRYPTO_RL_POLICY__DEFAULT_SHARES",
        legacy_cfg.rl_policy.default_shares,
    )
    .max(1);
    legacy_cfg.rl_policy.max_entry_price = env_decimal(
        "PLOY_CRYPTO_RL_POLICY__MAX_ENTRY_PRICE",
        legacy_cfg.rl_policy.max_entry_price,
    );
    legacy_cfg.rl_policy.cooldown_secs = env_u64(
        "PLOY_CRYPTO_RL_POLICY__COOLDOWN_SECS",
        legacy_cfg.rl_policy.cooldown_secs,
    );
    legacy_cfg.rl_policy.max_lob_snapshot_age_secs = env_u64(
        "PLOY_CRYPTO_RL_POLICY__MAX_LOB_SNAPSHOT_AGE_SECS",
        legacy_cfg.rl_policy.max_lob_snapshot_age_secs,
    )
    .max(1);
    legacy_cfg.rl_policy.decision_interval_ms = env_u64(
        "PLOY_CRYPTO_RL_POLICY__DECISION_INTERVAL_MS",
        legacy_cfg.rl_policy.decision_interval_ms,
    )
    .max(50);
    legacy_cfg.rl_policy.observation_version = env_u64(
        "PLOY_CRYPTO_RL_POLICY__OBS_VERSION",
        legacy_cfg.rl_policy.observation_version as u64,
    ) as u32;
    legacy_cfg.rl_policy.event_refresh_secs = env_u64(
        "PLOY_CRYPTO_RL_POLICY__EVENT_REFRESH_SECS",
        legacy_cfg.rl_policy.event_refresh_secs,
    )
    .max(1);
    legacy_cfg.rl_policy.min_time_remaining_secs = env_u64(
        "PLOY_CRYPTO_RL_POLICY__MIN_TIME_REMAINING_SECS",
        legacy_cfg.rl_policy.min_time_remaining_secs,
    );
    legacy_cfg.rl_policy.max_time_remaining_secs = env_u64(
        "PLOY_CRYPTO_RL_POLICY__MAX_TIME_REMAINING_SECS",
        legacy_cfg.rl_policy.max_time_remaining_secs,
    );
    if legacy_cfg.rl_policy.max_time_remaining_secs < legacy_cfg.rl_policy.min_time_remaining_secs
    {
        legacy_cfg.rl_policy.max_time_remaining_secs = legacy_cfg.rl_policy.min_time_remaining_secs;
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__PREFER_CLOSE_TO_END") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => legacy_cfg.rl_policy.prefer_close_to_end = true,
            "0" | "false" | "no" | "off" => legacy_cfg.rl_policy.prefer_close_to_end = false,
            _ => {}
        }
    }
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__EXPLORATION_RATE") {
        if let Ok(v) = raw.trim().parse::<f32>() {
            if v.is_finite() {
                legacy_cfg.rl_policy.exploration_rate = v.clamp(0.0, 1.0);
            }
        }
    }
    legacy_cfg.rl_policy.heartbeat_interval_secs = env_u64(
        "PLOY_CRYPTO_RL_POLICY__HEARTBEAT_INTERVAL_SECS",
        legacy_cfg.rl_policy.heartbeat_interval_secs,
    )
    .max(1);
}

pub(super) fn spawn_legacy_crypto_agent_runtimes(
    legacy_cfg: &LegacyCryptoRuntimeConfig,
    has_shared_pool: bool,
    crypto_market_data: CryptoDataPlaneHandle,
    event_matcher: Arc<EventMatcher>,
    lob_cache_opt: Option<LobCache>,
    coordinator: &mut Coordinator,
    handle: &CoordinatorHandle,
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
) -> Result<()> {
    if legacy_cfg.enable_lob_ml {
        spawn_crypto_lob_ml_agent(
            &legacy_cfg.lob_ml,
            has_shared_pool,
            crypto_market_data.clone(),
            event_matcher.clone(),
            lob_cache_opt.clone(),
            coordinator,
            handle,
            agent_handles,
        )?;
    }

    #[cfg(feature = "rl")]
    if legacy_cfg.enable_rl_policy {
        spawn_crypto_rl_policy_agent(
            &legacy_cfg.rl_policy,
            crypto_market_data,
            event_matcher,
            lob_cache_opt,
            coordinator,
            handle,
            agent_handles,
        );
    }

    Ok(())
}

fn spawn_crypto_lob_ml_agent(
    cfg: &CryptoLobMlConfig,
    has_shared_pool: bool,
    crypto_market_data: CryptoDataPlaneHandle,
    event_matcher: Arc<EventMatcher>,
    lob_cache_opt: Option<LobCache>,
    coordinator: &mut Coordinator,
    handle: &CoordinatorHandle,
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
) -> Result<()> {
    let model_type = cfg.model_type.trim().to_ascii_lowercase();
    let model_is_tcn = matches!(
        model_type.as_str(),
        "onnx_tcn" | "tcn" | "tcn_onnx" | "tcn-onnx"
    );

    if model_is_tcn && !cfg!(feature = "onnx") {
        warn!(
            agent = cfg.agent_id,
            model_type = %model_type,
            "crypto lob-ml agent model_type=onnx_tcn requires --features onnx; skipping agent spawn"
        );
        return Ok(());
    }

    if model_is_tcn && !has_shared_pool {
        warn!(
            agent = cfg.agent_id,
            model_type = %model_type,
            "crypto lob-ml agent model_type=onnx_tcn requires DB for feature parity with training; skipping agent spawn"
        );
        return Ok(());
    }

    let Some(lob_cache) = lob_cache_opt else {
        warn!(
            agent = cfg.agent_id,
            model_type = %model_type,
            "crypto lob-ml agent requires binance depth stream but it is disabled; skipping agent spawn"
        );
        return Ok(());
    };

    let agent = CryptoLobMlAgent::new(
        cfg.clone(),
        crypto_market_data,
        event_matcher,
        lob_cache,
    )?;
    spawn_trading_agent_task(
        agent,
        coordinator,
        handle,
        agent_handles,
        "crypto_lob_ml",
    );
    Ok(())
}

#[cfg(feature = "rl")]
fn spawn_crypto_rl_policy_agent(
    cfg: &CryptoRlPolicyConfig,
    crypto_market_data: CryptoDataPlaneHandle,
    event_matcher: Arc<EventMatcher>,
    lob_cache_opt: Option<LobCache>,
    coordinator: &mut Coordinator,
    handle: &CoordinatorHandle,
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
) {
    let Some(lob_cache) = lob_cache_opt else {
        warn!(
            agent = cfg.agent_id,
            "RL policy agent enabled but binance depth stream is disabled; skipping agent spawn"
        );
        return;
    };

    let agent = CryptoRlPolicyAgent::new(
        cfg.clone(),
        crypto_market_data,
        event_matcher,
        lob_cache,
    );
    spawn_trading_agent_task(
        agent,
        coordinator,
        handle,
        agent_handles,
        "crypto_rl_policy",
    );
}

fn spawn_trading_agent_task<A>(
    agent: A,
    coordinator: &mut Coordinator,
    handle: &CoordinatorHandle,
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    runtime_label: &'static str,
) where
    A: TradingAgent,
{
    let agent_id = agent.id().to_string();
    let domain = agent.domain();
    let risk_params = agent.risk_params();
    let cmd_rx = coordinator.register_agent(agent_id.clone(), domain, risk_params);
    let ctx = AgentContext::new(agent_id.clone(), domain, handle.clone(), cmd_rx);
    let runtime_agent_id = agent_id.clone();

    let jh = tokio::spawn(async move {
        if let Err(e) = agent.run(ctx).await {
            error!(
                agent = runtime_label,
                runtime_agent_id = %runtime_agent_id,
                error = %e,
                "legacy trading agent exited with error"
            );
        }
    });
    agent_handles.push(jh);
    info!(
        agent = %agent_id,
        runtime = runtime_label,
        "legacy trading agent spawned"
    );
}
