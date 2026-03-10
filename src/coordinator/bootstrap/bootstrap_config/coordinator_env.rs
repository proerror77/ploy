use rust_decimal::Decimal;

use crate::config::AppConfig;
use crate::coordinator::RiskConfig;
use crate::coordinator::config::DuplicateGuardScope;

use super::PlatformBootstrapConfig;
use crate::coordinator::bootstrap::support::{env_bool, env_decimal_opt, env_u64};

fn normalize_pct(v: Decimal) -> Option<Decimal> {
    if v >= Decimal::ZERO && v <= Decimal::ONE {
        Some(v)
    } else {
        None
    }
}

pub(super) fn apply_coordinator_runtime_env(cfg: &mut PlatformBootstrapConfig, app: &AppConfig) {
    cfg.coordinator.risk = RiskConfig {
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
    let alloc_base =
        (cfg.coordinator.risk.max_platform_exposure * account_deployable_pct).max(Decimal::ZERO);

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
    if let Some(v) =
        env_decimal_opt("PLOY_COORDINATOR__CRYPTO_HORIZON_CAP_OTHER_PCT").and_then(normalize_pct)
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
}
