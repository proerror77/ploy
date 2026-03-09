use rust_decimal::Decimal;

use crate::config::AppConfig;
use crate::strategy::{CryptoEntryMode, CryptoTradingConfig};

use super::PlatformBootstrapConfig;
use crate::coordinator::bootstrap::support::{env_decimal, env_u64};

pub(super) fn apply_crypto_runtime_env(cfg: &mut PlatformBootstrapConfig, app: &AppConfig) {
    apply_crypto_defaults(&mut cfg.crypto, app);

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
    cfg.crypto.sum_threshold = env_decimal("PLOY_CRYPTO_AGENT__SUM_THRESHOLD", cfg.crypto.sum_threshold);
    cfg.crypto.default_shares =
        env_u64("PLOY_CRYPTO_AGENT__DEFAULT_SHARES", cfg.crypto.default_shares).max(1);
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
    cfg.crypto.event_refresh_secs =
        env_u64("PLOY_CRYPTO_AGENT__EVENT_REFRESH_SECS", cfg.crypto.event_refresh_secs).max(1);
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
    cfg.crypto.entry_cooldown_secs =
        env_u64("PLOY_CRYPTO_AGENT__ENTRY_COOLDOWN_SECS", cfg.crypto.entry_cooldown_secs);
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
    cfg.crypto.min_hold_secs = env_u64("PLOY_CRYPTO_AGENT__MIN_HOLD_SECS", cfg.crypto.min_hold_secs);
    if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__ENTRY_MODE") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "arb_only" | "arb" => cfg.crypto.entry_mode = CryptoEntryMode::ArbOnly,
            "directional" | "dir" => cfg.crypto.entry_mode = CryptoEntryMode::Directional,
            "vol_straddle" | "straddle" => cfg.crypto.entry_mode = CryptoEntryMode::VolStraddle,
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
}

fn apply_crypto_defaults(crypto: &mut CryptoTradingConfig, app: &AppConfig) {
    crypto.default_shares = app.strategy.shares.max(1);
    let effective_threshold = app.strategy.effective_sum_target();
    if effective_threshold > Decimal::ZERO {
        crypto.sum_threshold = effective_threshold;
    } else if app.strategy.sum_target > Decimal::ZERO {
        crypto.sum_threshold = app.strategy.sum_target;
    }
    crypto.exit_edge_floor = app.strategy.profit_buffer.max(Decimal::ZERO);
    crypto.risk_params.max_order_value = app.risk.max_single_exposure_usd;
    let max_positions = if app.risk.max_positions > 0 {
        app.risk.max_positions
    } else {
        3
    };
    crypto.risk_params.max_total_exposure =
        app.risk.max_single_exposure_usd * Decimal::from(max_positions);
    crypto.risk_params.max_daily_loss = app.risk.daily_loss_limit_usd;
    crypto.risk_params.max_unhedged_positions = app.risk.max_positions_per_symbol.max(1);
}
