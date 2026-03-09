//! Platform Bootstrap — wires up Coordinator + Agents from config
//!
//! Entry point for `ploy platform start`. Creates shared infrastructure,
//! registers agents based on config flags, and runs the coordinator loop.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, trace, warn};

use crate::adapters::polymarket_clob::POLYGON_CHAIN_ID;
use crate::adapters::{BinanceWebSocket, PolymarketClient, PolymarketWebSocket, PostgresStore};
use crate::agents::{
    AgentContext, CryptoEntryMode, CryptoLobMlAgent, CryptoLobMlConfig, CryptoLobMlEntrySidePolicy,
    CryptoLobMlExitMode, CryptoTradingAgent, CryptoTradingConfig, OpenClawAgent, OpenClawConfig,
    GovernanceAgent, GovernanceContext, PoliticsTradingAgent, PoliticsTradingConfig,
    SportsTradingAgent, SportsTradingConfig, TradingAgent,
};
#[cfg(feature = "rl")]
use crate::agents::{CryptoRlPolicyAgent, CryptoRlPolicyConfig};
use crate::ai_clients::PolymarketSportsClient;
use crate::config::AppConfig;
use crate::coordinator::config::DuplicateGuardScope;
use crate::coordinator::{Coordinator, CoordinatorConfig, CoordinatorHandle, GlobalState};
use crate::domain::Side;
use crate::error::Result;
use crate::exchange::{build_exchange_client, parse_exchange_kind, ExchangeKind};
use crate::platform::{
    AgentRiskParams, BinanceDataPlaneHandle, CryptoDataPlaneHandle, DataPlaneConfig,
    Domain, MarketSelector, PlatformDataPlane, StrategyDeployment,
};
use crate::signing::Wallet;
use crate::strategy::event_edge::core::EventEdgeCore;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::idempotency::IdempotencyManager;
use crate::strategy::momentum::EventMatcher;
use crate::strategy::DataFeed;
use chrono::Utc;
use futures_util::StreamExt;
use polymarket_client_sdk::data::types::request::TradesRequest as DataTradesRequest;
use polymarket_client_sdk::data::types::MarketFilter as DataMarketFilter;
use polymarket_client_sdk::data::Client as DataApiClient;

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::instrument;

use super::strategy_runtime::run_managed_strategy_runtime;

mod runtime_spawns;
mod market_persistence;
mod schema;
mod strategy_deployments;

use self::market_persistence::{
    ensure_clob_trade_alerts_table, spawn_pm_token_settlement_persistence,
    spawn_polymarket_trade_persistence,
    spawn_polymarket_trade_persistence_from_collector_targets,
};
use self::runtime_spawns::{
    spawn_managed_strategy_runtime_task, spawn_openclaw_governance_agent,
    spawn_politics_trading_agent, spawn_sports_trading_agent, spawn_trading_agent_task,
    ManagedStrategyRuntimeSpawn,
};
pub(crate) use self::schema::{
    ensure_agent_order_executions_table, ensure_clob_orderbook_snapshots_table,
    ensure_coordinator_governance_policies_table,
    ensure_coordinator_governance_policy_history_table, ensure_pm_market_metadata_table,
    ensure_pm_token_settlements_table, ensure_risk_runtime_state_table,
    ensure_strategy_observability_tables,
};
use self::schema::{
    ensure_accounts_table, ensure_binance_lob_ticks_table, ensure_binance_price_ticks_table,
    ensure_clob_quote_ticks_table, ensure_schema_repairs, upsert_account_from_config,
};
use self::strategy_deployments::{
    apply_strategy_deployments, build_momentum_runtime_config,
    build_pattern_memory_runtime_config, build_split_arb_runtime_config, coin_symbol_for,
    collect_runtime_crypto_strategy_targets, crypto_series_id_for, symbol_for_crypto_series_id,
};

const CLOB_PERSIST_MIN_INTERVAL_SECS: i64 = 2;
const BINANCE_PERSIST_MIN_INTERVAL_SECS: i64 = 1;
const PM_COLLECTOR_REFRESH_SECS: u64 = 300;

fn lob_levels_json(
    state: &crate::collector::OrderBookState,
    is_bids: bool,
    max_levels: usize,
) -> Vec<(String, String)> {
    let max_levels = max_levels.max(1);

    if is_bids {
        state
            .bids
            .iter()
            .rev()
            .take(max_levels)
            .map(|(price_cents, qty)| {
                let price =
                    rust_decimal::Decimal::from(*price_cents) / rust_decimal::Decimal::from(100);
                (price.to_string(), qty.to_string())
            })
            .collect()
    } else {
        state
            .asks
            .iter()
            .take(max_levels)
            .map(|(price_cents, qty)| {
                let price =
                    rust_decimal::Decimal::from(*price_cents) / rust_decimal::Decimal::from(100);
                (price.to_string(), qty.to_string())
            })
            .collect()
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(default)
}

fn env_decimal(name: &str, default: rust_decimal::Decimal) -> rust_decimal::Decimal {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<rust_decimal::Decimal>().ok())
        .unwrap_or(default)
}

fn env_decimal_opt(name: &str) -> Option<rust_decimal::Decimal> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<rust_decimal::Decimal>().ok())
}

fn deployments_state_path() -> PathBuf {
    if let Ok(path) = std::env::var("PLOY_DEPLOYMENTS_FILE") {
        return PathBuf::from(path);
    }
    let container_data_root = Path::new("/opt/ploy/data");
    if container_data_root.exists() {
        return container_data_root.join("state/deployments.json");
    }
    let repo_state_deployment = Path::new("data/state/deployments.json");
    if repo_state_deployment.exists() {
        return repo_state_deployment.to_path_buf();
    }
    let repo_root_deployment = Path::new("deployment/deployments.json");
    if repo_root_deployment.exists() {
        return repo_root_deployment.to_path_buf();
    }
    let container_deployment = Path::new("/opt/ploy/deployment/deployments.json");
    if container_deployment.exists() {
        return container_deployment.to_path_buf();
    }
    PathBuf::from("data/state/deployments.json")
}

fn parse_strategy_deployments(raw: &str) -> Vec<StrategyDeployment> {
    let mut out = Vec::new();
    match serde_json::from_str::<Vec<StrategyDeployment>>(raw) {
        Ok(items) => {
            for mut dep in items {
                if dep.id.trim().is_empty() {
                    continue;
                }
                dep.normalize_account_ids_in_place();
                out.push(dep);
            }
        }
        Err(e) => {
            warn!(error = %e, "failed to parse strategy deployments JSON");
        }
    }
    out
}

fn load_strategy_deployments() -> Vec<StrategyDeployment> {
    let raw = std::env::var("PLOY_STRATEGY_DEPLOYMENTS_JSON")
        .or_else(|_| std::env::var("PLOY_DEPLOYMENTS_JSON"))
        .unwrap_or_default();
    if !raw.trim().is_empty() {
        return parse_strategy_deployments(&raw);
    }

    let repo_state_path = Path::new("data/state/deployments.json");
    let container_data_path = Path::new("/opt/ploy/data/state/deployments.json");
    let deployment_file_candidates = [
        deployments_state_path(),
        repo_state_path.to_path_buf(),
        container_data_path.to_path_buf(),
        Path::new("deployment/deployments.json").to_path_buf(),
        Path::new("/opt/ploy/deployment/deployments.json").to_path_buf(),
    ];

    for path in deployment_file_candidates {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let items = parse_strategy_deployments(&contents);
            if !items.is_empty() {
                return items;
            }
        }
    }
    Vec::new()
}

fn add_coin_from_text(raw: &str, coins: &mut HashSet<String>) {
    let upper = raw.trim().to_ascii_uppercase();
    if upper.is_empty() {
        return;
    }

    for known in ["BTC", "ETH", "SOL", "XRP"] {
        if upper.contains(known) {
            coins.insert(known.to_string());
        }
    }

    for token in upper.split(|c: char| !c.is_ascii_alphanumeric()) {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        let base = t.strip_suffix("USDT").unwrap_or(t);
        if (2..=8).contains(&base.len()) && base.chars().all(|c| c.is_ascii_alphabetic()) {
            coins.insert(base.to_string());
        }
    }
}

fn add_coins_from_selector(selector: &MarketSelector, coins: &mut HashSet<String>) {
    match selector {
        MarketSelector::Static {
            symbol,
            series_id,
            market_slug,
        } => {
            if let Some(raw) = symbol.as_deref() {
                add_coin_from_text(raw, coins);
            }
            if let Some(raw) = series_id.as_deref() {
                add_coin_from_text(raw, coins);
            }
            if let Some(raw) = market_slug.as_deref() {
                add_coin_from_text(raw, coins);
            }
        }
        MarketSelector::Dynamic { query, .. } => {
            if let Some(raw) = query.as_deref() {
                add_coin_from_text(raw, coins);
            }
        }
    }
}

/// Top-level config for the platform bootstrap
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformBootstrapConfig {
    pub coordinator: CoordinatorConfig,
    pub enable_crypto: bool,
    #[serde(default)]
    pub enable_crypto_momentum: bool,
    #[serde(default)]
    pub enable_crypto_pattern_memory: bool,
    #[serde(default)]
    pub enable_crypto_split_arb: bool,
    #[serde(default)]
    pub enable_crypto_lob_ml: bool,
    #[serde(default)]
    #[cfg(feature = "rl")]
    pub enable_crypto_rl_policy: bool,
    pub enable_sports: bool,
    pub enable_politics: bool,
    #[serde(default)]
    pub enable_economics: bool,
    /// Enable OpenClaw meta-agent (Layer 3 orchestrator)
    #[serde(default)]
    pub enable_openclaw: bool,
    pub dry_run: bool,
    pub crypto: CryptoTradingConfig,
    pub crypto_lob_ml: CryptoLobMlConfig,
    #[serde(default)]
    #[cfg(feature = "rl")]
    pub crypto_rl_policy: CryptoRlPolicyConfig,
    pub sports: SportsTradingConfig,
    pub politics: PoliticsTradingConfig,
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
            enable_crypto_lob_ml: false,
            #[cfg(feature = "rl")]
            enable_crypto_rl_policy: false,
            enable_sports: false,
            enable_politics: false,
            enable_economics: false,
            enable_openclaw: false,
            dry_run: true,
            crypto: CryptoTradingConfig::default(),
            crypto_lob_ml: CryptoLobMlConfig::default(),
            #[cfg(feature = "rl")]
            crypto_rl_policy: CryptoRlPolicyConfig::default(),
            sports: SportsTradingConfig::default(),
            politics: PoliticsTradingConfig::default(),
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

        // Coordinator risk from app config
        cfg.coordinator.risk = crate::platform::RiskConfig {
            max_platform_exposure: app.risk.max_single_exposure_usd,
            max_consecutive_failures: app.risk.max_consecutive_failures,
            daily_loss_limit: app.risk.daily_loss_limit_usd,
            max_spread_bps: 500,
            critical_bypass_exposure: false,
            ..Default::default()
        };
        cfg.coordinator.risk.max_drawdown_limit = env_decimal_opt("PLOY_RISK__MAX_DRAWDOWN_USD")
            .map(|v| v.max(rust_decimal::Decimal::ZERO));
        cfg.coordinator.risk.circuit_breaker_auto_recover = env_bool(
            "PLOY_RISK__CIRCUIT_BREAKER_AUTO_RECOVER",
            cfg.coordinator.risk.circuit_breaker_auto_recover,
        );
        cfg.coordinator.risk.circuit_breaker_cooldown_secs = env_u64(
            "PLOY_RISK__CIRCUIT_BREAKER_COOLDOWN_SECS",
            cfg.coordinator.risk.circuit_breaker_cooldown_secs,
        );

        // Optional domain-level risk splits.
        // Example:
        // - PLOY_RISK__ACCOUNT_RESERVE_PCT=0.15
        // - PLOY_RISK__ACCOUNT_DEPLOYABLE_PCT=0.85
        // - PLOY_RISK__CRYPTO_ALLOCATION_PCT=0.5
        // - PLOY_RISK__SPORTS_ALLOCATION_PCT=0.5
        // - PLOY_RISK__CRYPTO_DAILY_LOSS_LIMIT_USD=45
        // - PLOY_RISK__SPORTS_DAILY_LOSS_LIMIT_USD=45
        let normalize_pct = |v: rust_decimal::Decimal| {
            if v >= rust_decimal::Decimal::ZERO && v <= rust_decimal::Decimal::ONE {
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
            .unwrap_or(rust_decimal::Decimal::ZERO);
        let account_deployable_pct = env_decimal_opt("PLOY_RISK__ACCOUNT_DEPLOYABLE_PCT")
            .and_then(normalize_pct)
            .unwrap_or_else(|| rust_decimal::Decimal::ONE - account_reserve_pct);
        let alloc_base = (cfg.coordinator.risk.max_platform_exposure * account_deployable_pct)
            .max(rust_decimal::Decimal::ZERO);

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
        // Coordinator-level Kelly sizing (optional; applied when intents carry `signal_fair_value`).
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
            cfg.coordinator.kelly_min_edge = v
                .max(rust_decimal::Decimal::ZERO)
                .min(rust_decimal::Decimal::ONE);
        }
        cfg.coordinator.kelly_min_shares = env_u64(
            "PLOY_COORDINATOR__KELLY_MIN_SHARES",
            cfg.coordinator.kelly_min_shares,
        );

        // Execution venue minimums (used to prevent deterministic 400s that would otherwise
        // trip the circuit breaker and make the system look like it "stops after one loop").
        cfg.coordinator.min_order_shares = env_u64(
            "PLOY_COORDINATOR__MIN_ORDER_SHARES",
            cfg.coordinator.min_order_shares,
        );
        if let Some(v) = env_decimal_opt("PLOY_COORDINATOR__MIN_ORDER_NOTIONAL_USD") {
            cfg.coordinator.min_order_notional_usd = v.max(rust_decimal::Decimal::ZERO);
        }
        // Map legacy [strategy]/[risk] values into crypto-agent defaults so
        // platform mode follows deployed config instead of hardcoded defaults.
        cfg.crypto.default_shares = app.strategy.shares.max(1);
        let effective_threshold = app.strategy.effective_sum_target();
        if effective_threshold > rust_decimal::Decimal::ZERO {
            cfg.crypto.sum_threshold = effective_threshold;
        } else if app.strategy.sum_target > rust_decimal::Decimal::ZERO {
            cfg.crypto.sum_threshold = app.strategy.sum_target;
        }
        cfg.crypto.exit_edge_floor = app.strategy.profit_buffer.max(rust_decimal::Decimal::ZERO);
        cfg.crypto.risk_params.max_order_value = app.risk.max_single_exposure_usd;
        let max_positions = if app.risk.max_positions > 0 {
            app.risk.max_positions
        } else {
            3
        };
        cfg.crypto.risk_params.max_total_exposure =
            app.risk.max_single_exposure_usd * rust_decimal::Decimal::from(max_positions);
        cfg.crypto.risk_params.max_daily_loss = app.risk.daily_loss_limit_usd;
        cfg.crypto.risk_params.max_unhedged_positions = app.risk.max_positions_per_symbol.max(1);

        // Environment overrides for crypto agent tuning (service-level).
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
        // Entry mode: arb_only | directional | vol_straddle
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_AGENT__ENTRY_MODE") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "arb_only" | "arb" => {
                    cfg.crypto.entry_mode = crate::agents::crypto::CryptoEntryMode::ArbOnly
                }
                "directional" | "dir" => {
                    cfg.crypto.entry_mode = crate::agents::crypto::CryptoEntryMode::Directional
                }
                "vol_straddle" | "straddle" => {
                    cfg.crypto.entry_mode = crate::agents::crypto::CryptoEntryMode::VolStraddle
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
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::ONE);
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

        // Optional LOB+ML crypto agent (disabled by default).
        // Default to the same risk envelope as the momentum agent unless overridden.
        cfg.crypto_lob_ml.default_shares = cfg.crypto.default_shares;
        cfg.crypto_lob_ml.exit_edge_floor = cfg.crypto.exit_edge_floor;
        cfg.crypto_lob_ml.exit_price_band = cfg.crypto.exit_price_band;
        cfg.crypto_lob_ml.risk_params = cfg.crypto.risk_params.clone();

        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__ENABLED") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.enable_crypto_lob_ml = true,
                "0" | "false" | "no" | "off" => cfg.enable_crypto_lob_ml = false,
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
                cfg.crypto_lob_ml.coins = coins;
            }
        }

        cfg.crypto_lob_ml.default_shares = env_u64(
            "PLOY_CRYPTO_LOB_ML__DEFAULT_SHARES",
            cfg.crypto_lob_ml.default_shares,
        )
        .max(1);
        cfg.crypto_lob_ml.exit_edge_floor = env_decimal(
            "PLOY_CRYPTO_LOB_ML__EXIT_EDGE_FLOOR",
            cfg.crypto_lob_ml.exit_edge_floor,
        );
        cfg.crypto_lob_ml.exit_price_band = env_decimal(
            "PLOY_CRYPTO_LOB_ML__EXIT_PRICE_BAND",
            cfg.crypto_lob_ml.exit_price_band,
        );
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__EXIT_MODE") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "settle_only" | "settle" => {
                    cfg.crypto_lob_ml.exit_mode = CryptoLobMlExitMode::SettleOnly
                }
                "ev_exit" | "ev" | "model_ev" => {
                    cfg.crypto_lob_ml.exit_mode = CryptoLobMlExitMode::EvExit
                }
                "signal_flip" | "flip" => {
                    cfg.crypto_lob_ml.exit_mode = CryptoLobMlExitMode::SignalFlip
                }
                "trailing_exit" | "trailing" | "price_exit" | "price" | "mtm" => {
                    cfg.crypto_lob_ml.exit_mode = CryptoLobMlExitMode::TrailingExit
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
        cfg.crypto_lob_ml.min_hold_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__MIN_HOLD_SECS",
            cfg.crypto_lob_ml.min_hold_secs,
        );
        cfg.crypto_lob_ml.min_edge =
            env_decimal("PLOY_CRYPTO_LOB_ML__MIN_EDGE", cfg.crypto_lob_ml.min_edge);
        cfg.crypto_lob_ml.max_entry_price = env_decimal(
            "PLOY_CRYPTO_LOB_ML__MAX_ENTRY_PRICE",
            cfg.crypto_lob_ml.max_entry_price,
        );
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__ENTRY_SIDE_POLICY") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "best_ev" | "best" => {
                    cfg.crypto_lob_ml.entry_side_policy = CryptoLobMlEntrySidePolicy::BestEv
                }
                "lagging_only" | "lagging" => {
                    cfg.crypto_lob_ml.entry_side_policy = CryptoLobMlEntrySidePolicy::LaggingOnly
                }
                _ => {}
            }
        }
        cfg.crypto_lob_ml.entry_late_window_secs_5m = env_u64(
            "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M",
            cfg.crypto_lob_ml.entry_late_window_secs_5m,
        )
        .min(300);
        if std::env::var_os("PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M").is_none()
            && std::env::var_os("PLOY_CRYPTO_LOB_ML__ENTRY_EARLY_WINDOW_SECS_5M").is_some()
        {
            warn!(
                "PLOY_CRYPTO_LOB_ML__ENTRY_EARLY_WINDOW_SECS_5M is deprecated; use PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M"
            );
            cfg.crypto_lob_ml.entry_late_window_secs_5m = env_u64(
                "PLOY_CRYPTO_LOB_ML__ENTRY_EARLY_WINDOW_SECS_5M",
                cfg.crypto_lob_ml.entry_late_window_secs_5m,
            )
            .min(300);
        }
        cfg.crypto_lob_ml.entry_late_window_secs_15m = env_u64(
            "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_15M",
            cfg.crypto_lob_ml.entry_late_window_secs_15m,
        )
        .min(900);
        cfg.crypto_lob_ml.taker_fee_rate = env_decimal(
            "PLOY_CRYPTO_LOB_ML__TAKER_FEE_RATE",
            cfg.crypto_lob_ml.taker_fee_rate,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::new(25, 2));
        cfg.crypto_lob_ml.entry_slippage_bps = env_decimal(
            "PLOY_CRYPTO_LOB_ML__ENTRY_SLIPPAGE_BPS",
            cfg.crypto_lob_ml.entry_slippage_bps,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::new(2500, 0));
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__USE_PRICE_TO_BEAT") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto_lob_ml.use_price_to_beat = true,
                "0" | "false" | "no" | "off" => cfg.crypto_lob_ml.use_price_to_beat = false,
                _ => {}
            }
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__REQUIRE_PRICE_TO_BEAT") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto_lob_ml.require_price_to_beat = true,
                "0" | "false" | "no" | "off" => cfg.crypto_lob_ml.require_price_to_beat = false,
                _ => {}
            }
        }
        cfg.crypto_lob_ml.model_blend_weight = env_decimal(
            "PLOY_CRYPTO_LOB_ML__MODEL_BLEND_WEIGHT",
            cfg.crypto_lob_ml.model_blend_weight,
        )
        .max(rust_decimal::Decimal::new(1, 2))
        .min(rust_decimal::Decimal::new(99, 2));
        cfg.crypto_lob_ml.min_direction_strength = env_decimal(
            "PLOY_CRYPTO_LOB_ML__MIN_DIRECTION_STRENGTH",
            cfg.crypto_lob_ml.min_direction_strength,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::new(49, 2));
        cfg.crypto_lob_ml.event_refresh_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__EVENT_REFRESH_SECS",
            cfg.crypto_lob_ml.event_refresh_secs,
        )
        .max(1);
        cfg.crypto_lob_ml.min_time_remaining_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__MIN_TIME_REMAINING_SECS",
            cfg.crypto_lob_ml.min_time_remaining_secs,
        );
        cfg.crypto_lob_ml.max_time_remaining_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__MAX_TIME_REMAINING_SECS",
            cfg.crypto_lob_ml.max_time_remaining_secs,
        );
        cfg.crypto_lob_ml.max_time_remaining_secs_5m = env_u64(
            "PLOY_CRYPTO_LOB_ML__MAX_TIME_REMAINING_SECS_5M",
            cfg.crypto_lob_ml.max_time_remaining_secs_5m,
        )
        .max(1);
        cfg.crypto_lob_ml.max_time_remaining_secs_15m = env_u64(
            "PLOY_CRYPTO_LOB_ML__MAX_TIME_REMAINING_SECS_15M",
            cfg.crypto_lob_ml.max_time_remaining_secs_15m,
        )
        .max(1);
        if cfg.crypto_lob_ml.max_time_remaining_secs < cfg.crypto_lob_ml.min_time_remaining_secs {
            cfg.crypto_lob_ml.max_time_remaining_secs = cfg.crypto_lob_ml.min_time_remaining_secs;
        }
        if cfg.crypto_lob_ml.max_time_remaining_secs_5m < cfg.crypto_lob_ml.min_time_remaining_secs
        {
            cfg.crypto_lob_ml.max_time_remaining_secs_5m =
                cfg.crypto_lob_ml.min_time_remaining_secs;
        }
        if cfg.crypto_lob_ml.max_time_remaining_secs_15m < cfg.crypto_lob_ml.min_time_remaining_secs
        {
            cfg.crypto_lob_ml.max_time_remaining_secs_15m =
                cfg.crypto_lob_ml.min_time_remaining_secs;
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__PREFER_CLOSE_TO_END") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto_lob_ml.prefer_close_to_end = true,
                "0" | "false" | "no" | "off" => cfg.crypto_lob_ml.prefer_close_to_end = false,
                _ => {}
            }
        }
        cfg.crypto_lob_ml.cooldown_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__COOLDOWN_SECS",
            cfg.crypto_lob_ml.cooldown_secs,
        );
        cfg.crypto_lob_ml.max_lob_snapshot_age_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__MAX_LOB_SNAPSHOT_AGE_SECS",
            cfg.crypto_lob_ml.max_lob_snapshot_age_secs,
        )
        .max(1);
        cfg.crypto_lob_ml.heartbeat_interval_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__HEARTBEAT_INTERVAL_SECS",
            cfg.crypto_lob_ml.heartbeat_interval_secs,
        )
        .max(1);
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_TYPE") {
            let v = raw.trim().to_ascii_lowercase();
            if !v.is_empty() {
                cfg.crypto_lob_ml.model_type = v;
            }
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_PATH") {
            let v = raw.trim();
            cfg.crypto_lob_ml.model_path = if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            };
        }
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_VERSION") {
            let v = raw.trim();
            cfg.crypto_lob_ml.model_version = if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            };
        }
        // window_fallback_weight env var kept for backward compat but is unused
        // in the 2-layer blend model. Ignore silently.
        cfg.crypto_lob_ml.ev_exit_buffer = env_decimal(
            "PLOY_CRYPTO_LOB_ML__EV_EXIT_BUFFER",
            cfg.crypto_lob_ml.ev_exit_buffer,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::new(50, 2));
        cfg.crypto_lob_ml.ev_exit_vol_scale = env_decimal(
            "PLOY_CRYPTO_LOB_ML__EV_EXIT_VOL_SCALE",
            cfg.crypto_lob_ml.ev_exit_vol_scale,
        )
        .max(rust_decimal::Decimal::ZERO)
        .min(rust_decimal::Decimal::new(50, 2));
        cfg.crypto_lob_ml.oracle_lag_buffer_secs = env_u64(
            "PLOY_CRYPTO_LOB_ML__ORACLE_LAG_BUFFER_SECS",
            cfg.crypto_lob_ml.oracle_lag_buffer_secs,
        );
        cfg.crypto_lob_ml.max_spread_pct = env_decimal(
            "PLOY_CRYPTO_LOB_ML__MAX_SPREAD_PCT",
            cfg.crypto_lob_ml.max_spread_pct,
        );
        if let Ok(raw) = std::env::var("PLOY_CRYPTO_LOB_ML__FORCE_SETTLE_ONLY_5M") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => cfg.crypto_lob_ml.force_settle_only_5m = true,
                "0" | "false" | "no" | "off" => cfg.crypto_lob_ml.force_settle_only_5m = false,
                _ => {}
            }
        }

        #[cfg(feature = "rl")]
        {
            // Optional RL policy crypto agent (disabled by default).
            // Default to the same risk envelope as the momentum agent unless overridden.
            cfg.crypto_rl_policy.default_shares = cfg.crypto.default_shares;
            cfg.crypto_rl_policy.risk_params = cfg.crypto.risk_params.clone();
            cfg.crypto_rl_policy.heartbeat_interval_secs = cfg.crypto.heartbeat_interval_secs;

            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__ENABLED") {
                match raw.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => cfg.enable_crypto_rl_policy = true,
                    "0" | "false" | "no" | "off" => cfg.enable_crypto_rl_policy = false,
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
                    cfg.crypto_rl_policy.coins = coins;
                }
            }

            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__MODEL_PATH") {
                let v = raw.trim();
                if !v.is_empty() {
                    cfg.crypto_rl_policy.policy_model_path = Some(v.to_string());
                }
            }
            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__POLICY_OUTPUT") {
                let v = raw.trim().to_ascii_lowercase();
                if !v.is_empty() {
                    cfg.crypto_rl_policy.policy_output = v;
                }
            }
            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__MODEL_VERSION") {
                let v = raw.trim();
                if !v.is_empty() {
                    cfg.crypto_rl_policy.policy_model_version = Some(v.to_string());
                }
            }

            cfg.crypto_rl_policy.default_shares = env_u64(
                "PLOY_CRYPTO_RL_POLICY__DEFAULT_SHARES",
                cfg.crypto_rl_policy.default_shares,
            )
            .max(1);
            cfg.crypto_rl_policy.max_entry_price = env_decimal(
                "PLOY_CRYPTO_RL_POLICY__MAX_ENTRY_PRICE",
                cfg.crypto_rl_policy.max_entry_price,
            );
            cfg.crypto_rl_policy.cooldown_secs = env_u64(
                "PLOY_CRYPTO_RL_POLICY__COOLDOWN_SECS",
                cfg.crypto_rl_policy.cooldown_secs,
            );
            cfg.crypto_rl_policy.max_lob_snapshot_age_secs = env_u64(
                "PLOY_CRYPTO_RL_POLICY__MAX_LOB_SNAPSHOT_AGE_SECS",
                cfg.crypto_rl_policy.max_lob_snapshot_age_secs,
            )
            .max(1);
            cfg.crypto_rl_policy.decision_interval_ms = env_u64(
                "PLOY_CRYPTO_RL_POLICY__DECISION_INTERVAL_MS",
                cfg.crypto_rl_policy.decision_interval_ms,
            )
            .max(50);
            cfg.crypto_rl_policy.observation_version = env_u64(
                "PLOY_CRYPTO_RL_POLICY__OBS_VERSION",
                cfg.crypto_rl_policy.observation_version as u64,
            ) as u32;
            cfg.crypto_rl_policy.event_refresh_secs = env_u64(
                "PLOY_CRYPTO_RL_POLICY__EVENT_REFRESH_SECS",
                cfg.crypto_rl_policy.event_refresh_secs,
            )
            .max(1);
            cfg.crypto_rl_policy.min_time_remaining_secs = env_u64(
                "PLOY_CRYPTO_RL_POLICY__MIN_TIME_REMAINING_SECS",
                cfg.crypto_rl_policy.min_time_remaining_secs,
            );
            cfg.crypto_rl_policy.max_time_remaining_secs = env_u64(
                "PLOY_CRYPTO_RL_POLICY__MAX_TIME_REMAINING_SECS",
                cfg.crypto_rl_policy.max_time_remaining_secs,
            );
            if cfg.crypto_rl_policy.max_time_remaining_secs
                < cfg.crypto_rl_policy.min_time_remaining_secs
            {
                cfg.crypto_rl_policy.max_time_remaining_secs =
                    cfg.crypto_rl_policy.min_time_remaining_secs;
            }
            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__PREFER_CLOSE_TO_END") {
                match raw.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => cfg.crypto_rl_policy.prefer_close_to_end = true,
                    "0" | "false" | "no" | "off" => {
                        cfg.crypto_rl_policy.prefer_close_to_end = false
                    }
                    _ => {}
                }
            }
            if let Ok(raw) = std::env::var("PLOY_CRYPTO_RL_POLICY__EXPLORATION_RATE") {
                if let Ok(v) = raw.trim().parse::<f32>() {
                    if v.is_finite() {
                        cfg.crypto_rl_policy.exploration_rate = v.clamp(0.0, 1.0);
                    }
                }
            }
            cfg.crypto_rl_policy.heartbeat_interval_secs = env_u64(
                "PLOY_CRYPTO_RL_POLICY__HEARTBEAT_INTERVAL_SECS",
                cfg.crypto_rl_policy.heartbeat_interval_secs,
            )
            .max(1);
        }

        // Enable sports if NBA comeback config is present and enabled
        if let Some(ref nba) = app.nba_comeback {
            if nba.enabled {
                cfg.enable_sports = true;
                // Keep the agent poll cadence aligned with the NBA comeback config.
                cfg.sports.poll_interval_secs = nba.espn_poll_interval_secs;
            }
        }

        // Enable politics if event edge config is present and enabled
        if let Some(ref ee) = app.event_edge_agent {
            if ee.enabled {
                cfg.enable_politics = true;
            }
        }

        cfg.reapply_strategy_deployments_for_runtime(app);

        // OpenClaw-first runtime lockdown:
        // keep coordinator available, but disable built-in agent loops.
        if app.openclaw_runtime_lockdown() {
            cfg.enable_crypto = false;
            cfg.enable_crypto_momentum = false;
            cfg.enable_crypto_pattern_memory = false;
            cfg.enable_crypto_split_arb = false;
            cfg.enable_crypto_lob_ml = false;
            #[cfg(feature = "rl")]
            {
                cfg.enable_crypto_rl_policy = false;
            }
            cfg.enable_sports = false;
            cfg.enable_politics = false;
            cfg.enable_economics = false;
            info!("agent framework lockdown active (mode=openclaw): built-in agents are disabled");
        }

        cfg
    }
}

/// Optional control commands to apply immediately after platform startup.
#[derive(Debug, Clone, Default)]
pub struct PlatformStartControl {
    pub pause: Option<String>,
    pub resume: Option<String>,
}

/// Start the multi-agent platform
///
/// Creates shared infrastructure, registers configured agents,
/// and runs the coordinator loop until shutdown.
pub async fn start_platform(
    config: PlatformBootstrapConfig,
    app_config: &AppConfig,
    control: PlatformStartControl,
) -> Result<()> {
    let exchange_kind = parse_exchange_kind(&app_config.execution.exchange)?;
    let exchange_client = build_exchange_client(app_config, config.dry_run).await?;
    let non_pm_builtin_agents_enabled = exchange_kind != ExchangeKind::Polymarket
        && (config.enable_crypto || config.enable_sports || config.enable_politics);
    if non_pm_builtin_agents_enabled {
        return Err(crate::error::PloyError::Validation(format!(
            "execution.exchange={} is not yet supported with built-in agents (crypto/sports/politics). Disable built-in agents or set execution.exchange=polymarket",
            exchange_kind
        )));
    }

    // Polymarket client is required for:
    // - crypto event discovery (Gamma)
    // - settlement persistence (Gamma)
    // - politics agent
    // - sports settlement labeling (Gamma)
    let needs_polymarket_client =
        config.enable_crypto || config.enable_sports || config.enable_politics;
    let pm_client = if needs_polymarket_client {
        let rest_url = app_config
            .market
            .exchange_rest_url
            .as_deref()
            .unwrap_or(&app_config.market.rest_url);

        if config.dry_run {
            Some(PolymarketClient::new(rest_url, true)?)
        } else {
            let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
            let funder = std::env::var("POLYMARKET_FUNDER").ok();
            if let Some(funder_addr) = funder {
                Some(
                    PolymarketClient::new_authenticated_proxy(rest_url, wallet, &funder_addr, true)
                        .await?,
                )
            } else {
                Some(PolymarketClient::new_authenticated(rest_url, wallet, true).await?)
            }
        }
    } else {
        None
    };

    let account_id = if app_config.account.id.trim().is_empty() {
        "default".to_string()
    } else {
        app_config.account.id.clone()
    };
    let runtime_crypto_targets =
        collect_runtime_crypto_strategy_targets(&account_id, config.dry_run);
    #[cfg(feature = "rl")]
    let crypto_rl_policy_enabled = config.enable_crypto_rl_policy;
    #[cfg(not(feature = "rl"))]
    let crypto_rl_policy_enabled = false;

    info!(
        account_id = %account_id,
        crypto = config.enable_crypto,
        crypto_momentum = config.enable_crypto_momentum,
        crypto_pattern_memory = config.enable_crypto_pattern_memory,
        crypto_split_arb = config.enable_crypto_split_arb,
        crypto_lob_ml = config.enable_crypto_lob_ml,
        crypto_rl_policy = crypto_rl_policy_enabled,
        sports = config.enable_sports,
        politics = config.enable_politics,
        economics = config.enable_economics,
        openclaw = config.enable_openclaw || config.openclaw.enabled,
        exchange = %exchange_kind,
        dry_run = config.dry_run,
        "starting multi-agent platform"
    );
    if config.enable_economics {
        warn!(
            "economics domain enabled, but no built-in economics agent is registered; coordinator-level risk and allocator gates remain active"
        );
    }

    let mut allowed_domains: HashSet<Domain> = HashSet::new();
    if config.enable_crypto {
        allowed_domains.insert(Domain::Crypto);
    }
    if config.enable_sports {
        allowed_domains.insert(Domain::Sports);
    }
    if config.enable_politics {
        allowed_domains.insert(Domain::Politics);
    }

    let db_required = env_bool(
        "PLOY_DB_REQUIRED",
        env_bool("PLOY_REQUIRE_DB", !app_config.dry_run.enabled),
    );

    // Optional shared DB pool used for (a) coordinator execution logs and (b) market data persistence.
    // Crypto agents can run without DB; sports agent requires DB for calendar/stats.
    let shared_pool = match PgPoolOptions::new()
        .max_connections(app_config.database.max_connections)
        .connect(&app_config.database.url)
        .await
    {
        Ok(pool) => Some(pool),
        Err(e) => {
            if db_required {
                return Err(crate::error::PloyError::Internal(format!(
                    "database connection is required but failed at startup: {}",
                    e
                )));
            }
            warn!(
                error = %e,
                "failed to connect DB at startup; continuing without shared pool"
            );
            None
        }
    };

    // 1. Create shared executor (+ DB-backed idempotency when DB is available)
    let exec_config = app_config.execution.clone();
    let mut executor_builder =
        OrderExecutor::new_with_exchange(exchange_client.clone(), exec_config);
    if let Some(pool) = shared_pool.as_ref() {
        let idem_store = PostgresStore::from_pool(pool.clone());
        let idem_mgr = Arc::new(IdempotencyManager::new_with_account(
            idem_store,
            account_id.clone(),
        ));
        executor_builder = executor_builder.with_idempotency(idem_mgr.clone());
        info!("order executor idempotency enabled");

        // Spawn hourly idempotency key cleanup task
        let cleanup_mgr = idem_mgr.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                match cleanup_mgr.cleanup_expired().await {
                    Ok(n) if n > 0 => info!("idempotency cleanup: removed {} expired keys", n),
                    Err(e) => warn!("idempotency cleanup error: {}", e),
                    _ => {}
                }
            }
        });
    } else {
        warn!("order executor idempotency disabled (no database connection)");
    }
    let executor = Arc::new(executor_builder);

    // 2. Create coordinator
    let mut coordinator = Coordinator::new(
        config.coordinator.clone(),
        executor,
        account_id.clone(),
        allowed_domains.clone(),
    );
    if let Some(pool) = shared_pool.as_ref() {
        // Run migrations by default whenever a DB connection is available, even in dry-run.
        // This prevents long-lived services from starting on a stale schema.
        let mut run_sqlx_migrations = env_bool("PLOY_RUN_SQLX_MIGRATIONS", true);
        let require_sqlx_migrations = env_bool("PLOY_REQUIRE_SQLX_MIGRATIONS", true);
        if require_sqlx_migrations && !run_sqlx_migrations {
            warn!(
                "PLOY_RUN_SQLX_MIGRATIONS=false but PLOY_REQUIRE_SQLX_MIGRATIONS=true; forcing migrations"
            );
            run_sqlx_migrations = true;
        }
        let require_startup_schema =
            env_bool("PLOY_REQUIRE_STARTUP_SCHEMA", !app_config.dry_run.enabled);
        let require_runtime_restore = env_bool(
            "PLOY_REQUIRE_RUNTIME_STATE_RESTORE",
            !app_config.dry_run.enabled,
        );
        let migration_store = PostgresStore::from_pool(pool.clone());
        if run_sqlx_migrations {
            if let Err(e) = migration_store.migrate().await {
                if require_sqlx_migrations {
                    return Err(e);
                }
                warn!(
                    error = %e,
                    "sqlx migration runner failed at startup; continuing due to PLOY_REQUIRE_SQLX_MIGRATIONS=false"
                );
            }
        } else {
            info!("sqlx migration runner skipped at startup (PLOY_RUN_SQLX_MIGRATIONS=false)");
        }
        ensure_schema_repairs(pool).await?;
        if let Err(e) = ensure_accounts_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure accounts table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure accounts table");
        } else if let Err(e) =
            upsert_account_from_config(pool, &account_id, &app_config.account).await
        {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to upsert account metadata: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to upsert account metadata");
        }
        if let Err(e) = ensure_coordinator_governance_policies_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure coordinator_governance_policies table: {}",
                    e
                )));
            }
            warn!(
                error = %e,
                "failed to ensure coordinator_governance_policies table; governance persistence disabled"
            );
        } else if let Err(e) = ensure_coordinator_governance_policy_history_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure coordinator_governance_policy_history table: {}",
                    e
                )));
            }
            warn!(
                error = %e,
                "failed to ensure coordinator_governance_policy_history table; governance history persistence disabled"
            );
        } else {
            coordinator.set_governance_store_pool(pool.clone());
            if let Err(e) = coordinator.load_persisted_governance_policy().await {
                if require_startup_schema {
                    return Err(crate::error::PloyError::Internal(format!(
                        "failed to restore coordinator governance policy: {}",
                        e
                    )));
                }
                warn!(
                    error = %e,
                    "failed to restore coordinator governance policy from DB"
                );
            }
        }
        if let Err(e) = ensure_agent_order_executions_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure agent_order_executions table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure agent_order_executions table; execution logging disabled");
        } else {
            coordinator.set_execution_log_pool(pool.clone());
            if let Err(e) = coordinator.restore_runtime_state_from_execution_log().await {
                if require_runtime_restore {
                    return Err(crate::error::PloyError::Internal(format!(
                        "failed to restore coordinator runtime state from execution log: {}",
                        e
                    )));
                }
                warn!(
                    error = %e,
                    "failed to restore coordinator runtime state from execution log"
                );
            }
        }
        if let Err(e) = ensure_strategy_observability_tables(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure strategy observability tables: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure strategy observability tables");
        }
        if let Err(e) = ensure_pm_market_metadata_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure pm_market_metadata table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure pm_market_metadata table");
        }
        if let Err(e) = ensure_pm_token_settlements_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure pm_token_settlements table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure pm_token_settlements table");
        }
        if let Err(e) = ensure_risk_runtime_state_table(pool).await {
            if require_startup_schema {
                return Err(crate::error::PloyError::Internal(format!(
                    "failed to ensure risk_runtime_state table: {}",
                    e
                )));
            }
            warn!(error = %e, "failed to ensure risk_runtime_state table");
        } else if let Err(e) = coordinator.restore_risk_runtime_state().await {
            warn!(error = %e, "failed to restore risk runtime state");
        }
        if config.enable_crypto {
            if let Err(e) = ensure_clob_trade_alerts_table(pool).await {
                if require_startup_schema {
                    return Err(crate::error::PloyError::Internal(format!(
                        "failed to ensure clob_trade_alerts table: {}",
                        e
                    )));
                }
                warn!(
                    error = %e,
                    "failed to ensure clob_trade_alerts table at startup"
                );
            }
        }
    }

    let ingress_agents = std::env::var("PLOY_EXTERNAL_INGRESS_AGENT_IDS")
        .unwrap_or_else(|_| "openclaw_rpc,sidecar".to_string());
    for agent_id in ingress_agents
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        coordinator
            .authorize_external_agent(agent_id, AgentRiskParams::conservative())
            .await;
    }
    let handle = coordinator.handle();
    let _global_state = coordinator.global_state();

    // 2a. Start API server with platform services (if api feature enabled)
    #[cfg(feature = "api")]
    let _api_handle = {
        use crate::adapters::{start_api_server_platform_background, PostgresStore};
        use crate::ai_clients::grok::GrokClient;
        use crate::api::state::StrategyConfigState;

        let api_port = std::env::var("API_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(8081);

        // Initialize Grok client if GROK_API_KEY is set
        let grok_client = std::env::var("GROK_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .and_then(|_| match GrokClient::from_env() {
                Ok(client) => {
                    info!("Grok client initialized for sidecar endpoints");
                    Some(Arc::new(client))
                }
                Err(e) => {
                    warn!(error = %e, "failed to initialize Grok client");
                    None
                }
            });

        if let Some(ref pool) = shared_pool {
            let store = Arc::new(PostgresStore::from_pool(pool.clone()));
            let api_config = StrategyConfigState {
                symbols: vec![],
                min_move: 0.0,
                max_entry: 1.0,
                shares: 0,
                predictive: false,
                exit_edge_floor: None,
                exit_price_band: None,
                time_decay_exit_secs: None,
                liquidity_exit_spread_bps: None,
            };

            match start_api_server_platform_background(
                store,
                api_port,
                api_config,
                Some(handle.clone()),
                grok_client,
                account_id.clone(),
                config.dry_run,
            )
            .await
            {
                Ok(handle) => {
                    info!(
                        port = api_port,
                        "API server started in platform mode with sidecar endpoints"
                    );
                    Some(handle)
                }
                Err(e) => {
                    warn!(error = %e, "API server failed to start");
                    None
                }
            }
        } else {
            warn!("API server not started: no database connection");
            None
        }
    };
    #[cfg(not(feature = "api"))]
    let _api_handle: Option<tokio::task::JoinHandle<crate::error::Result<()>>> = None;

    // 3. Shutdown broadcast channel
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // 3b. Optional Polymarket settlement persistence (Gamma) for training labels.
    // Keep it read-only and enabled even in dry-run (no order placement).
    if let Some(pool) = shared_pool.as_ref() {
        let mut collector_domains: Vec<&'static str> = Vec::new();
        if config.enable_crypto {
            collector_domains.push("CRYPTO");
        }
        if config.enable_sports {
            collector_domains.push("SPORTS_NBA");
        }

        if !collector_domains.is_empty() {
            if let Some(client) = pm_client.clone() {
                spawn_pm_token_settlement_persistence(
                    client,
                    pool.clone(),
                    format!("settlements:{}", account_id),
                    collector_domains,
                );
            } else {
                warn!(
                    account_id = %account_id,
                    "pm client not configured; skipping token settlement persistence task"
                );
            }
        }
    }

    // 4. Spawn agents
    let mut agent_handles = Vec::new();

    // Shared per-symbol freshness tracker — attached to all WS adapters.
    let freshness = Arc::new(crate::platform::DataPlaneFreshness::new());
    let use_data_plane = env_bool("PLOY_DATA_PLANE", false);

    if config.enable_crypto {
        let crypto_cfg = config.crypto.clone();
        let momentum_enabled = config.enable_crypto_momentum;
        let pattern_memory_enabled = config.enable_crypto_pattern_memory;
        let split_arb_enabled = config.enable_crypto_split_arb;
        let lob_cfg = config.crypto_lob_ml.clone();
        let lob_agent_enabled = config.enable_crypto_lob_ml;
        #[cfg(feature = "rl")]
        let rl_cfg = config.crypto_rl_policy.clone();
        #[cfg(feature = "rl")]
        let rl_agent_enabled = config.enable_crypto_rl_policy;
        #[cfg(not(feature = "rl"))]
        let rl_agent_enabled = false;

        // Discover active crypto events and token IDs (Gamma API) via EventMatcher
        let pm_client_ref = pm_client.as_ref().ok_or_else(|| {
            crate::error::PloyError::Validation(
                "crypto domain requires a Polymarket client, but none was initialized".to_string(),
            )
        })?;
        let event_matcher = Arc::new(EventMatcher::new(pm_client_ref.clone()));
        if let Err(e) = event_matcher.refresh().await {
            warn!(error = %e, "crypto event matcher refresh failed (continuing)");
        }

        // Build a unified coin set across all enabled crypto strategies.
        // Also build a SubscriptionPlan for audit/observability (Phase 1 shadow).
        let mut all_coins: Vec<String> = Vec::new();
        let mut planner_requirements: Vec<(crate::platform::ConsumerId, Domain, Vec<DataFeed>)> =
            Vec::new();
        if momentum_enabled {
            let symbols: Vec<String> = crypto_cfg
                .coins
                .iter()
                .map(|c| format!("{}USDT", c))
                .collect();
            planner_requirements.push((
                crate::platform::ConsumerId::from(format!("momentum-{}", crypto_cfg.agent_id)),
                Domain::Crypto,
                vec![DataFeed::BinanceSpot { symbols }],
            ));
            for coin in &crypto_cfg.coins {
                if !all_coins.contains(coin) {
                    all_coins.push(coin.clone());
                }
            }
        }
        if lob_agent_enabled {
            let symbols: Vec<String> = lob_cfg.coins.iter().map(|c| format!("{}USDT", c)).collect();
            planner_requirements.push((
                crate::platform::ConsumerId::from("lob-ml"),
                Domain::Crypto,
                vec![DataFeed::BinanceSpot { symbols }],
            ));
            for coin in &lob_cfg.coins {
                if !all_coins.contains(coin) {
                    all_coins.push(coin.clone());
                }
            }
        }
        #[cfg(feature = "rl")]
        if rl_agent_enabled {
            let symbols: Vec<String> = rl_cfg.coins.iter().map(|c| format!("{}USDT", c)).collect();
            planner_requirements.push((
                crate::platform::ConsumerId::from("rl-policy"),
                Domain::Crypto,
                vec![DataFeed::BinanceSpot { symbols }],
            ));
            for coin in &rl_cfg.coins {
                if !all_coins.contains(coin) {
                    all_coins.push(coin.clone());
                }
            }
        }
        if use_data_plane && pattern_memory_enabled {
            let mut coins: Vec<String> = if runtime_crypto_targets.pattern_memory_coins.is_empty() {
                crypto_cfg.coins.clone()
            } else {
                runtime_crypto_targets
                    .pattern_memory_coins
                    .iter()
                    .cloned()
                    .collect()
            };
            coins.sort();
            coins.dedup();
            for coin in coins {
                if !all_coins.contains(&coin) {
                    all_coins.push(coin);
                }
            }
        }
        if use_data_plane && split_arb_enabled {
            let mut coins: Vec<String> = if runtime_crypto_targets.split_arb_coins.is_empty() {
                crypto_cfg.coins.clone()
            } else {
                runtime_crypto_targets
                    .split_arb_coins
                    .iter()
                    .cloned()
                    .collect()
            };
            coins.sort();
            coins.dedup();
            for coin in coins {
                if !all_coins.contains(&coin) {
                    all_coins.push(coin);
                }
            }
        }
        if all_coins.is_empty() {
            warn!("crypto domain enabled but no crypto agents are active (coins set is empty)");
        }

        // Build and log the subscription plan (Phase 1: shadow audit).
        let subscription_plan =
            crate::platform::SubscriptionPlanner::build_plan(planner_requirements);
        info!(
            unique_keys = subscription_plan.key_count(),
            total_refs = subscription_plan.ref_count(),
            binance_symbols = subscription_plan.binance_symbols().len(),
            "subscription plan built (shadow audit)"
        );

        // Create WebSocket feeds
        let symbols: Vec<String> = all_coins.iter().map(|c| format!("{}USDT", c)).collect();
        let mut data_plane: Option<Arc<PlatformDataPlane>> = None;
        let (binance_ws, pm_ws) = if use_data_plane {
            let data_plane_config = DataPlaneConfig {
                polymarket_ws_url: app_config.market.ws_url.clone(),
                binance_spot_symbols: symbols.clone(),
                binance_kline_symbols: symbols.clone(),
                binance_kline_intervals: vec!["5m".to_string(), "15m".to_string()],
                binance_kline_closed_only: true,
                chainlink_symbols: vec![],
            };
            let dp = Arc::new(PlatformDataPlane::new(
                data_plane_config,
                Arc::clone(&freshness),
            ));
            dp.start(Vec::new()).await?;
            info!("PlatformDataPlane started");

            let binance_ws = dp.binance_ws().ok_or_else(|| {
                crate::error::PloyError::Validation(
                    "PLOY_DATA_PLANE=1 but PlatformDataPlane has no Binance WS adapter".to_string(),
                )
            })?;
            let pm_ws = dp.polymarket_ws().ok_or_else(|| {
                crate::error::PloyError::Validation(
                    "PLOY_DATA_PLANE=1 but PlatformDataPlane has no Polymarket WS adapter"
                        .to_string(),
                )
            })?;
            data_plane = Some(dp);
            (binance_ws, pm_ws)
        } else {
            let binance_ws = Arc::new(BinanceWebSocket::new(symbols));
            let pm_ws = Arc::new(PolymarketWebSocket::new(&app_config.market.ws_url));

            // Attach per-symbol freshness tracker to WS adapters.
            binance_ws.set_freshness(Arc::clone(&freshness));
            pm_ws.set_freshness(Arc::clone(&freshness));
            info!("data plane freshness tracker attached to WS adapters");
            (binance_ws, pm_ws)
        };
        let crypto_market_data = CryptoDataPlaneHandle::new(binance_ws.clone(), pm_ws.clone());

        // Seed PM token → side mapping for data collection, so QuoteUpdates carry the correct
        // UP/DOWN side and can be persisted to Postgres.
        //
        // IMPORTANT: Keep the collector subscription set bounded. The trading agent only adds
        // tokens; without pruning, the WS subscription grows forever and can overwhelm the box.
        let collector_min_remaining_secs = env_i64("PM_COLLECTOR_MIN_REMAINING_SECS", 0)
            .max(-86400)
            .min(86400);
        let mut desired: HashMap<String, Side> = HashMap::new();
        let mut collector_targets: Vec<crate::collector::CollectorTokenTarget> = Vec::new();
        for coin in &all_coins {
            let symbol = format!("{}USDT", coin.to_uppercase());
            for ev in event_matcher
                .get_events_with_min_remaining(&symbol, collector_min_remaining_secs)
                .await
            {
                desired.insert(ev.up_token_id.clone(), Side::Up);
                desired.insert(ev.down_token_id.clone(), Side::Down);

                // Feed the L2 orderbook-history collector with an explicit token target list.
                // This prevents "collect everything" behavior when other markets become active.
                let expires_at = Some(ev.end_time + chrono::Duration::hours(1));
                collector_targets.push(
                    crate::collector::CollectorTokenTarget::new(ev.up_token_id.clone(), "CRYPTO")
                        .with_expires_at(expires_at)
                        .with_metadata(serde_json::json!({
                            "symbol": symbol.as_str(),
                            "side": "UP",
                            "condition_id": ev.condition_id.as_str(),
                            "slug": ev.slug.as_str(),
                            "title": ev.title.as_str(),
                            "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                        })),
                );
                collector_targets.push(
                    crate::collector::CollectorTokenTarget::new(ev.down_token_id.clone(), "CRYPTO")
                        .with_expires_at(expires_at)
                        .with_metadata(serde_json::json!({
                            "symbol": symbol.as_str(),
                            "side": "DOWN",
                            "condition_id": ev.condition_id.as_str(),
                            "slug": ev.slug.as_str(),
                            "title": ev.title.as_str(),
                            "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                        })),
                );
            }
        }
        if use_data_plane {
            for (token, side) in desired.iter() {
                pm_ws.register_token(token, *side).await;
            }
            pm_ws.request_resubscribe();
            info!(
                agent = %crypto_cfg.agent_id,
                token_count = desired.len(),
                "seeded PM token mappings for crypto data collection"
            );
        } else {
            let (_added, _removed, _updated, total) = pm_ws.reconcile_token_sides(&desired).await;
            info!(
                agent = %crypto_cfg.agent_id,
                token_count = total,
                "seeded PM token mappings for crypto data collection"
            );
        }

        if let Some(pool) = shared_pool.as_ref() {
            if let Err(e) = crate::collector::ensure_collector_token_targets_table(pool).await {
                warn!(
                    agent = %crypto_cfg.agent_id,
                    error = %e,
                    "failed to ensure collector_token_targets table"
                );
            }

            if let Err(e) =
                crate::collector::upsert_collector_token_targets(pool, &collector_targets).await
            {
                warn!(
                    agent = %crypto_cfg.agent_id,
                    error = %e,
                    "failed to upsert collector token targets (crypto)"
                );
            }
        }

        // Keep refreshing the subscription token set over time so 5m + 15m markets continue
        // to be recorded throughout the day, independent of which single market the agent
        // is currently trading.
        let pm_ws_collector = pm_ws.clone();
        let matcher_collector = event_matcher.clone();
        let coins_collector = all_coins.clone();
        let agent_id_collector = crypto_cfg.agent_id.clone();
        let pool_collector = shared_pool.clone();
        let use_data_plane_collector = use_data_plane;
        let initial_last_desired = if use_data_plane_collector {
            desired.clone()
        } else {
            HashMap::new()
        };
        tokio::spawn(async move {
            let refresh_secs =
                env_u64("PM_COLLECTOR_REFRESH_SECS", PM_COLLECTOR_REFRESH_SECS).max(10);
            let mut tick = tokio::time::interval(Duration::from_secs(refresh_secs));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut last_desired = initial_last_desired;

            loop {
                tick.tick().await;

                if let Err(e) = matcher_collector.refresh().await {
                    warn!(agent = %agent_id_collector, error = %e, "pm token collector refresh failed");
                    continue;
                }

                let mut desired: HashMap<String, Side> = HashMap::new();
                let mut collector_targets: Vec<crate::collector::CollectorTokenTarget> = Vec::new();
                for coin in &coins_collector {
                    let symbol = format!("{}USDT", coin.to_uppercase());
                    for ev in matcher_collector
                        .get_events_with_min_remaining(&symbol, collector_min_remaining_secs)
                        .await
                    {
                        desired.insert(ev.up_token_id.clone(), Side::Up);
                        desired.insert(ev.down_token_id.clone(), Side::Down);

                        let expires_at = Some(ev.end_time + chrono::Duration::hours(1));
                        collector_targets.push(
                            crate::collector::CollectorTokenTarget::new(
                                ev.up_token_id.clone(),
                                "CRYPTO",
                            )
                            .with_expires_at(expires_at)
                            .with_metadata(serde_json::json!({
                                "symbol": symbol.as_str(),
                                "side": "UP",
                                "condition_id": ev.condition_id.as_str(),
                                "slug": ev.slug.as_str(),
                                "title": ev.title.as_str(),
                                "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                            })),
                        );
                        collector_targets.push(
                            crate::collector::CollectorTokenTarget::new(
                                ev.down_token_id.clone(),
                                "CRYPTO",
                            )
                            .with_expires_at(expires_at)
                            .with_metadata(serde_json::json!({
                                "symbol": symbol.as_str(),
                                "side": "DOWN",
                                "condition_id": ev.condition_id.as_str(),
                                "slug": ev.slug.as_str(),
                                "title": ev.title.as_str(),
                                "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                            })),
                        );
                    }
                }

                if use_data_plane_collector {
                    if desired != last_desired {
                        let previous_token_count = last_desired.len();
                        for (token, side) in desired.iter() {
                            pm_ws_collector.register_token(token, *side).await;
                        }
                        pm_ws_collector.request_resubscribe();
                        info!(
                            agent = %agent_id_collector,
                            previous_token_count,
                            token_count = desired.len(),
                            "pm token collector refreshed token set on shared data-plane ws; resubscribe requested"
                        );
                        last_desired = desired;
                    }
                } else {
                    let (added, removed, updated, total) =
                        pm_ws_collector.reconcile_token_sides(&desired).await;
                    if added > 0 || removed > 0 {
                        pm_ws_collector.request_resubscribe();
                        info!(
                            agent = %agent_id_collector,
                            added,
                            removed,
                            updated,
                            token_count = total,
                            "pm token collector reconciled token set; resubscribe requested"
                        );
                    }
                }

                if let Some(pool) = pool_collector.as_ref() {
                    // Table may not exist if migrations were not applied; ensure it.
                    let ensured =
                        crate::collector::ensure_collector_token_targets_table(pool).await;
                    if let Err(e) = ensured {
                        warn!(
                            agent = %agent_id_collector,
                            error = %e,
                            "failed to ensure collector_token_targets table"
                        );
                    }

                    if let Err(e) =
                        crate::collector::upsert_collector_token_targets(pool, &collector_targets)
                            .await
                    {
                        warn!(
                            agent = %agent_id_collector,
                            error = %e,
                            "failed to upsert collector token targets (crypto)"
                        );
                    }
                }
            }
        });

        let mut crypto_persistence_pipeline: Option<crate::platform::PersistencePipelineHandle> =
            None;
        // Optional persistence pipeline for WS-driven market data (best-effort).
        // Do not block agent startup if DB is temporarily unavailable.
        if let Some(pool) = shared_pool.as_ref() {
            let (orderbook_levels_default, orderbook_snapshot_secs_default) = (20usize, 60i64);
            let orderbook_levels =
                env_usize("PM_ORDERBOOK_LEVELS", orderbook_levels_default).clamp(1, 200);
            let orderbook_snapshot_ms = match std::env::var("PM_ORDERBOOK_SNAPSHOT_MS") {
                Ok(raw) => raw.parse::<u64>().unwrap_or(0),
                Err(_) => (env_i64(
                    "PM_ORDERBOOK_SNAPSHOT_SECS",
                    orderbook_snapshot_secs_default,
                )
                .max(0) as u64)
                    .saturating_mul(1000),
            };
            let orderbook_require_hash_change = env_bool("PM_ORDERBOOK_REQUIRE_HASH_CHANGE", true);

            let quote_table_ready = match ensure_clob_quote_ticks_table(pool).await {
                Ok(()) => true,
                Err(e) => {
                    warn!(
                        agent = crypto_cfg.agent_id,
                        error = %e,
                        "failed to ensure clob_quote_ticks table; quote persistence bridge disabled"
                    );
                    false
                }
            };
            let price_table_ready = match ensure_binance_price_ticks_table(pool).await {
                Ok(()) => true,
                Err(e) => {
                    warn!(
                        agent = crypto_cfg.agent_id,
                        error = %e,
                        "failed to ensure binance_price_ticks table; Binance price persistence bridge disabled"
                    );
                    false
                }
            };
            let orderbook_table_ready = match ensure_clob_orderbook_snapshots_table(pool).await {
                Ok(()) => true,
                Err(e) => {
                    warn!(
                        agent = crypto_cfg.agent_id,
                        error = %e,
                        "failed to ensure clob_orderbook_snapshots table; orderbook persistence bridge disabled"
                    );
                    false
                }
            };

            if quote_table_ready || price_table_ready || orderbook_table_ready {
                let pipeline_config = crate::platform::PersistenceConfig {
                    clob_quote_min_interval_secs: CLOB_PERSIST_MIN_INTERVAL_SECS,
                    binance_price_min_interval_secs: BINANCE_PERSIST_MIN_INTERVAL_SECS,
                    binance_lob_snapshot_interval_ms: env_u64("BN_LOB_SNAPSHOT_MS", 1000).max(100)
                        as i64,
                    binance_lob_max_levels: env_usize("BN_LOB_LEVELS", 20).clamp(0, 200),
                    clob_orderbook_snapshot_interval_ms: orderbook_snapshot_ms as i64,
                    clob_orderbook_max_levels: orderbook_levels,
                    clob_orderbook_require_hash_change: orderbook_require_hash_change,
                    ..Default::default()
                };
                let pipeline_handle = crate::platform::PersistencePipeline::spawn_with_freshness(
                    pool.clone(),
                    pipeline_config,
                    Some(Arc::clone(&freshness)),
                );
                crypto_persistence_pipeline = Some(pipeline_handle.clone());

                if quote_table_ready {
                    let quote_rx = if use_data_plane {
                        data_plane.as_ref().and_then(|dp| dp.subscribe_quotes())
                    } else {
                        Some(pm_ws.subscribe_updates())
                    };
                    if let Some(quote_rx) = quote_rx {
                        pipeline_handle.spawn_bridge(
                            quote_rx,
                            format!("{}.quote", crypto_cfg.agent_id),
                            |update| {
                                Some(crate::platform::PersistenceEvent::ClobQuote(
                                    crate::platform::ClobQuoteTick {
                                        token_id: update.token_id.clone(),
                                        side: update.side.as_str().to_string(),
                                        best_bid: update.quote.best_bid,
                                        best_ask: update.quote.best_ask,
                                        bid_size: update.quote.bid_size,
                                        ask_size: update.quote.ask_size,
                                        domain: Domain::Crypto,
                                        received_at: Utc::now(),
                                    },
                                ))
                            },
                        );
                    } else {
                        warn!("persistence quote bridge unavailable: no quote receiver");
                    }
                }

                if price_table_ready {
                    let price_rx = if use_data_plane {
                        data_plane.as_ref().and_then(|dp| dp.subscribe_prices())
                    } else {
                        Some(binance_ws.subscribe())
                    };
                    if let Some(price_rx) = price_rx {
                        pipeline_handle.spawn_bridge(
                            price_rx,
                            format!("{}.price", crypto_cfg.agent_id),
                            |update| {
                                Some(crate::platform::PersistenceEvent::BinancePrice(
                                    crate::platform::BinancePriceTick {
                                        symbol: update.symbol.clone(),
                                        price: Some(update.price),
                                        quantity: update.quantity,
                                        trade_time: update.timestamp,
                                    },
                                ))
                            },
                        );
                    } else {
                        warn!("persistence price bridge unavailable: no price receiver");
                    }
                }

                if orderbook_table_ready {
                    let book_rx = if use_data_plane {
                        data_plane.as_ref().and_then(|dp| dp.subscribe_books())
                    } else {
                        Some(pm_ws.subscribe_books())
                    };
                    if let Some(book_rx) = book_rx {
                        pipeline_handle.spawn_bridge(
                            book_rx,
                            format!("{}.orderbook", crypto_cfg.agent_id),
                            |book_msg| {
                                use sha2::{Digest, Sha256};
                                let bids_json =
                                    serde_json::to_value(&book_msg.bids).unwrap_or_default();
                                let asks_json =
                                    serde_json::to_value(&book_msg.asks).unwrap_or_default();
                                let mut hasher = Sha256::new();
                                hasher.update(bids_json.to_string().as_bytes());
                                hasher.update(asks_json.to_string().as_bytes());
                                let hash = format!("{:x}", hasher.finalize());
                                Some(crate::platform::PersistenceEvent::ClobOrderbook(
                                    crate::platform::ClobOrderbookSnapshot {
                                        domain: Domain::Crypto,
                                        token_id: book_msg.asset_id.clone(),
                                        market: Some(book_msg.market.clone()),
                                        bids: bids_json,
                                        asks: asks_json,
                                        book_timestamp: book_msg
                                            .timestamp
                                            .as_deref()
                                            .and_then(|s| {
                                                chrono::DateTime::parse_from_rfc3339(s).ok()
                                            })
                                            .map(|dt| dt.with_timezone(&Utc)),
                                        hash,
                                        source: "polymarket_ws".into(),
                                        context: None,
                                    },
                                ))
                            },
                        );
                    } else {
                        warn!("persistence orderbook bridge unavailable: no book receiver");
                    }
                }

                info!(agent = crypto_cfg.agent_id, "persistence pipeline started");
            } else {
                warn!(
                    agent = crypto_cfg.agent_id,
                    "all market-data persistence tables unavailable; WS persistence bridges disabled"
                );
            }

            // Trade persistence (polling-based) — still separate from WS pipeline.
            spawn_polymarket_trade_persistence(
                event_matcher.clone(),
                pool.clone(),
                crypto_cfg.agent_id.clone(),
                all_coins.clone(),
                Domain::Crypto,
            );
            info!(
                agent = crypto_cfg.agent_id,
                "market data persistence tasks started"
            );
        }

        // Optional Binance LOB depth stream (for ML/RL feature generation).
        let mut enable_binance_lob = lob_agent_enabled || rl_agent_enabled;
        if let Ok(raw) = std::env::var("PLOY_BINANCE_LOB__ENABLED") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => enable_binance_lob = true,
                "0" | "false" | "no" | "off" => enable_binance_lob = false,
                _ => {}
            }
        }

        let mut lob_cache_opt: Option<crate::collector::LobCache> = None;
        if enable_binance_lob {
            let depth_symbols: Vec<String> = match std::env::var("PLOY_BINANCE_LOB__SYMBOLS") {
                Ok(raw) => raw
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_ascii_uppercase())
                    .collect(),
                Err(_) => all_coins.iter().map(|c| format!("{}USDT", c)).collect(),
            };

            let depth_stream = Arc::new(
                crate::collector::BinanceDepthStream::new(depth_symbols)
                    .with_freshness(Arc::clone(&freshness)),
            );
            let lob_cache = depth_stream.cache().clone();
            lob_cache_opt = Some(lob_cache.clone());

            if let Some(pool) = shared_pool.as_ref() {
                match ensure_binance_lob_ticks_table(pool).await {
                    Ok(()) => {
                        if let Some(ph) = crypto_persistence_pipeline.clone() {
                            let rx = depth_stream.subscribe();
                            let agent_id = crypto_cfg.agent_id.clone();
                            let max_levels = env_usize("BN_LOB_LEVELS", 20).clamp(0, 200);
                            ph.spawn_bridge(
                                rx,
                                format!("{}.binance_lob", agent_id),
                                move |update| {
                                    let symbol = update.symbol.clone();
                                    let (bids, asks) = if max_levels == 0 {
                                        (Vec::new(), Vec::new())
                                    } else {
                                        (
                                            lob_levels_json(&update.raw_state, true, max_levels),
                                            lob_levels_json(&update.raw_state, false, max_levels),
                                        )
                                    };
                                    Some(crate::platform::PersistenceEvent::BinanceLob(
                                        crate::platform::BinanceLobTick {
                                            symbol,
                                            update_id: update.snapshot.update_id,
                                            best_bid: Some(update.snapshot.best_bid),
                                            best_ask: Some(update.snapshot.best_ask),
                                            mid_price: Some(update.snapshot.mid_price),
                                            spread_bps: Some(update.snapshot.spread_bps),
                                            obi_5: update.snapshot.obi_5.to_f64(),
                                            obi_10: update.snapshot.obi_10.to_f64(),
                                            bid_volume_5: Some(update.snapshot.bid_volume_5),
                                            ask_volume_5: Some(update.snapshot.ask_volume_5),
                                            bids: serde_json::to_value(&bids).unwrap_or_default(),
                                            asks: serde_json::to_value(&asks).unwrap_or_default(),
                                            event_time: update.snapshot.timestamp,
                                        },
                                    ))
                                },
                            );
                        } else {
                            warn!(
                                agent = crypto_cfg.agent_id,
                                "Binance LOB persistence requested but pipeline handle unavailable"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            agent = crypto_cfg.agent_id,
                            error = %e,
                            "failed to ensure binance_lob_ticks table; Binance LOB persistence bridge disabled"
                        );
                    }
                }
            }

            let ds = depth_stream.clone();
            tokio::spawn(async move {
                if let Err(e) = ds.run().await {
                    error!(error = %e, "binance depth stream error");
                }
            });

            info!(
                agent = crypto_cfg.agent_id,
                "binance LOB depth stream started"
            );
        }

        if !use_data_plane {
            // Spawn Binance WS in background
            let bws = binance_ws.clone();
            tokio::spawn(async move {
                if let Err(e) = bws.run().await {
                    error!(error = %e, "binance websocket error");
                }
            });

            // Spawn PM WS in background
            let pws = pm_ws.clone();
            tokio::spawn(async move {
                if let Err(e) = pws.run(Vec::new()).await {
                    error!(error = %e, "polymarket websocket error");
                }
            });
        }

        if momentum_enabled {
            match build_momentum_runtime_config(&crypto_cfg) {
                Ok(toml_cfg) => {
                    let _ = spawn_managed_strategy_runtime_task(
                        ManagedStrategyRuntimeSpawn {
                            strategy_label: "momentum",
                            agent_id: crypto_cfg.agent_id.clone(),
                            domain: Domain::Crypto,
                            risk_params: crypto_cfg.risk_params.clone(),
                            strategy_config_toml: toml_cfg,
                        },
                        &mut coordinator,
                        &shutdown_tx,
                        &mut agent_handles,
                        config.dry_run,
                        pm_client.as_ref(),
                        &app_config.market.ws_url,
                        data_plane.clone(),
                        shared_pool.clone(),
                        &account_id,
                    );
                }
                Err(e) => {
                    warn!(
                        agent = crypto_cfg.agent_id,
                        error = %e,
                        entry_mode = ?crypto_cfg.entry_mode,
                        "momentum runtime config unavailable; falling back to legacy trading agent"
                    );
                    let agent = CryptoTradingAgent::new(
                        crypto_cfg.clone(),
                        crypto_market_data.clone(),
                        event_matcher.clone(),
                    );
                    spawn_trading_agent_task(
                        agent,
                        &mut coordinator,
                        &handle,
                        &mut agent_handles,
                        "crypto_momentum_legacy",
                    );
                }
            }
        } else {
            info!(
                agent = crypto_cfg.agent_id,
                "crypto momentum agent disabled"
            );
        }

        if pattern_memory_enabled {
            // Ensure persistence table exists
            if let Some(ref pool) = shared_pool {
                if let Err(e) =
                    crate::strategy::pattern_memory::persistence::ensure_table(pool).await
                {
                    warn!(error = %e, "failed to create pattern_memory_samples table");
                }
            }

            let mut coins: Vec<String> = if runtime_crypto_targets.pattern_memory_coins.is_empty() {
                crypto_cfg.coins.clone()
            } else {
                runtime_crypto_targets
                    .pattern_memory_coins
                    .iter()
                    .cloned()
                    .collect()
            };
            coins.sort();
            coins.dedup();

            match build_pattern_memory_runtime_config(&coins) {
                Ok(toml_cfg) => {
                    let _ = spawn_managed_strategy_runtime_task(
                        ManagedStrategyRuntimeSpawn {
                            strategy_label: "pattern_memory",
                            agent_id: "pattern_memory".to_string(),
                            domain: Domain::Crypto,
                            risk_params: crypto_cfg.risk_params.clone(),
                            strategy_config_toml: toml_cfg,
                        },
                        &mut coordinator,
                        &shutdown_tx,
                        &mut agent_handles,
                        config.dry_run,
                        pm_client.as_ref(),
                        &app_config.market.ws_url,
                        data_plane.clone(),
                        shared_pool.clone(),
                        &account_id,
                    );
                }
                Err(e) => {
                    warn!(
                        agent = "pattern_memory",
                        error = %e,
                        "pattern_memory enabled but no valid runtime config could be built"
                    );
                }
            }
        }

        if split_arb_enabled {
            let mut coins: Vec<String> = if runtime_crypto_targets.split_arb_coins.is_empty() {
                crypto_cfg.coins.clone()
            } else {
                runtime_crypto_targets
                    .split_arb_coins
                    .iter()
                    .cloned()
                    .collect()
            };
            coins.sort();
            coins.dedup();

            let mut horizons: Vec<String> = if runtime_crypto_targets.split_arb_horizons.is_empty()
            {
                vec!["5m".to_string(), "15m".to_string()]
            } else {
                runtime_crypto_targets
                    .split_arb_horizons
                    .iter()
                    .cloned()
                    .collect()
            };
            horizons.sort();
            horizons.dedup();

            let mut series_set: HashSet<String> = HashSet::new();
            for coin in &coins {
                let normalized = coin.trim_end_matches("USDT");
                for horizon in &horizons {
                    if let Some(series_id) = crypto_series_id_for(normalized, horizon) {
                        series_set.insert(series_id.to_string());
                    }
                }
            }
            let mut series_ids: Vec<String> = series_set.into_iter().collect();
            series_ids.sort();

            let mut symbols: Vec<String> = coins
                .iter()
                .filter_map(|coin| {
                    let normalized = coin.trim_end_matches("USDT");
                    coin_symbol_for(normalized)
                })
                .collect();
            symbols.sort();
            symbols.dedup();
            if symbols.is_empty() {
                symbols = series_ids
                    .iter()
                    .filter_map(|series_id| {
                        symbol_for_crypto_series_id(series_id).map(str::to_string)
                    })
                    .collect();
                symbols.sort();
                symbols.dedup();
            }

            if series_ids.is_empty() {
                warn!(
                    agent = "staggered_arb",
                    "staggered_arb enabled but no recognized coin/horizon series ids were resolved"
                );
            } else {
                let toml_cfg = build_split_arb_runtime_config(&symbols, &series_ids);
                let _ = spawn_managed_strategy_runtime_task(
                    ManagedStrategyRuntimeSpawn {
                        strategy_label: "staggered_arb",
                        agent_id: "staggered_arb".to_string(),
                        domain: Domain::Crypto,
                        risk_params: crypto_cfg.risk_params.clone(),
                        strategy_config_toml: toml_cfg,
                    },
                    &mut coordinator,
                    &shutdown_tx,
                    &mut agent_handles,
                    config.dry_run,
                    pm_client.as_ref(),
                    &app_config.market.ws_url,
                    data_plane.clone(),
                    shared_pool.clone(),
                    &account_id,
                );
            }
        }

        if lob_agent_enabled {
            let model_type = lob_cfg.model_type.trim().to_ascii_lowercase();
            let model_is_tcn = matches!(
                model_type.as_str(),
                "onnx_tcn" | "tcn" | "tcn_onnx" | "tcn-onnx"
            );

            if model_is_tcn && !cfg!(feature = "onnx") {
                warn!(
                    agent = lob_cfg.agent_id,
                    model_type = %model_type,
                    "crypto lob-ml agent model_type=onnx_tcn requires --features onnx; skipping agent spawn"
                );
            } else if model_is_tcn && shared_pool.is_none() {
                warn!(
                    agent = lob_cfg.agent_id,
                    model_type = %model_type,
                    "crypto lob-ml agent model_type=onnx_tcn requires DB for feature parity with training; skipping agent spawn"
                );
            } else if !model_is_tcn && lob_cache_opt.is_none() {
                warn!(
                    agent = lob_cfg.agent_id,
                    model_type = %model_type,
                    "crypto lob-ml agent requires binance depth stream but it is disabled; skipping agent spawn"
                );
            } else {
                if let Some(lob_cache) = lob_cache_opt.clone() {
                    let agent = CryptoLobMlAgent::new(
                        lob_cfg.clone(),
                        crypto_market_data.clone(),
                        event_matcher.clone(),
                        lob_cache,
                    )?;
                    spawn_trading_agent_task(
                        agent,
                        &mut coordinator,
                        &handle,
                        &mut agent_handles,
                        "crypto_lob_ml",
                    );
                } else {
                    warn!(
                        agent = lob_cfg.agent_id,
                        model_type = %model_type,
                        "crypto lob-ml agent requires binance depth stream but it is disabled; skipping agent spawn"
                    );
                }
            }
        }

        #[cfg(feature = "rl")]
        if rl_agent_enabled {
            if let Some(lob_cache) = lob_cache_opt.clone() {
                let agent = CryptoRlPolicyAgent::new(
                    rl_cfg.clone(),
                    crypto_market_data.clone(),
                    event_matcher.clone(),
                    lob_cache,
                );
                spawn_trading_agent_task(
                    agent,
                    &mut coordinator,
                    &handle,
                    &mut agent_handles,
                    "crypto_rl_policy",
                );
            } else {
                warn!(
                    agent = rl_cfg.agent_id,
                    "RL policy agent enabled but binance depth stream is disabled; skipping agent spawn"
                );
            }
        }
    }

    if config.enable_sports {
        spawn_sports_trading_agent(
            &config,
            &app_config,
            shared_pool.as_ref(),
            &freshness,
            &mut coordinator,
            &handle,
            &mut agent_handles,
        )
        .await?;
    }

    if config.enable_politics {
        spawn_politics_trading_agent(
            &config,
            &app_config,
            &mut coordinator,
            &handle,
            pm_client.as_ref(),
            &mut agent_handles,
        )
        .await?;
    }

    // --- OpenClaw meta-agent (Layer 3 orchestrator) ---
    spawn_openclaw_governance_agent(
        &config,
        &freshness,
        &mut coordinator,
        &handle,
        &mut agent_handles,
    );

    // ── Auto-claimer background task ──
    // Ensure single account-level daemon (deduped) and avoid spawning a second
    // direct AutoClaimer loop in platform bootstrap.
    #[cfg(feature = "claimer_daemon")]
    if !config.dry_run && pm_client.is_some() {
        if let Err(e) = crate::strategy::ensure_account_claimer_daemon().await {
            warn!(error = %e, "failed to ensure account-level auto-claimer daemon");
        } else {
            info!("auto-claimer background task ensured (account-level)");
        }
    }

    #[cfg(not(feature = "claimer_daemon"))]
    if pm_client.is_some() {
        info!("claimer feature disabled; skipping auto-claimer background task");
    }

    info!(
        agents = agent_handles.len(),
        "all agents spawned, starting coordinator"
    );

    // 4b. Apply initial control commands (pause/resume)
    if let Some(agent_id) = control.pause.as_deref() {
        if agent_id == "all" {
            coordinator.pause_all().await;
        } else if let Err(e) = coordinator
            .send_command(agent_id, crate::coordinator::CoordinatorCommand::Pause)
            .await
        {
            warn!(agent_id, error = %e, "failed to pause agent at startup");
        }
    } else if let Some(agent_id) = control.resume.as_deref() {
        if agent_id == "all" {
            coordinator.resume_all().await;
        } else if let Err(e) = coordinator
            .send_command(agent_id, crate::coordinator::CoordinatorCommand::Resume)
            .await
        {
            warn!(agent_id, error = %e, "failed to resume agent at startup");
        }
    }

    // 5. Run coordinator (blocks until shutdown signal)
    let shutdown_rx = shutdown_tx.subscribe();

    // Spawn Ctrl+C handler
    let stx = shutdown_tx.clone();
    tokio::spawn(async move {
        if let Ok(()) = tokio::signal::ctrl_c().await {
            info!("Ctrl+C received, initiating shutdown");
            let _ = stx.send(());
        }
    });

    coordinator.run(shutdown_rx).await;

    // 6. Wait for agents to finish (with timeout)
    info!("waiting for agents to finish...");
    let timeout = tokio::time::Duration::from_secs(10);
    for jh in agent_handles {
        let _ = tokio::time::timeout(timeout, jh).await;
    }

    info!("platform shutdown complete");
    Ok(())
}

/// Print the current global state (for `ploy platform status`)
pub fn print_platform_status(state: &GlobalState) {
    println!("=== Platform Status ===");
    println!(
        "Started: {} | Last refresh: {}",
        state.started_at.format("%H:%M:%S"),
        state.last_refresh.format("%H:%M:%S")
    );
    println!("Risk state: {:?}", state.risk_state);
    println!(
        "Portfolio: exposure={} unrealized_pnl={} realized_pnl={}",
        state.total_exposure(),
        state.total_unrealized_pnl(),
        state.total_realized_pnl
    );
    println!(
        "Queue: size={} enqueued={} dequeued={}",
        state.queue_stats.current_size,
        state.queue_stats.enqueued_total,
        state.queue_stats.dequeued_total
    );
    println!("\n--- Agents ({}) ---", state.agents.len());
    for (id, agent) in &state.agents {
        println!(
            "  {} [{}] {:?} | pos={} exp={} pnl={} | hb={}",
            id,
            agent.name,
            agent.status,
            agent.position_count,
            agent.exposure,
            agent.daily_pnl,
            agent.last_heartbeat.format("%H:%M:%S")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::OrderStatus;
    use crate::platform::{
        DeploymentExecutionMode, StrategyLifecycleStage, StrategyProductType, Timeframe,
    };
    use rust_decimal_macros::dec;
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn set_env(key: &str, value: Option<&str>) {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    fn economics_deployment(enabled: bool) -> StrategyDeployment {
        StrategyDeployment {
            id: "deploy.econ.fed.15m".to_string(),
            strategy: "macro_regime".to_string(),
            strategy_version: "v1".to_string(),
            domain: Domain::Economics,
            market_selector: MarketSelector::Static {
                symbol: None,
                series_id: None,
                market_slug: Some("fed-rate-15m".to_string()),
            },
            timeframe: Timeframe::M15,
            enabled,
            allocator_profile: "default".to_string(),
            risk_profile: "default".to_string(),
            priority: 0,
            cooldown_secs: 60,
            account_ids: Vec::new(),
            execution_mode: DeploymentExecutionMode::Any,
            lifecycle_stage: StrategyLifecycleStage::Live,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: None,
            last_evaluation_score: None,
        }
    }

    fn crypto_deployment(strategy: &str, enabled: bool) -> StrategyDeployment {
        StrategyDeployment {
            id: format!("deploy.crypto.{strategy}.5m"),
            strategy: strategy.to_string(),
            strategy_version: "v1".to_string(),
            domain: Domain::Crypto,
            market_selector: MarketSelector::Static {
                symbol: Some("BTCUSDT".to_string()),
                series_id: None,
                market_slug: None,
            },
            timeframe: Timeframe::M5,
            enabled,
            allocator_profile: "default".to_string(),
            risk_profile: "default".to_string(),
            priority: 0,
            cooldown_secs: 60,
            account_ids: Vec::new(),
            execution_mode: DeploymentExecutionMode::Any,
            lifecycle_stage: StrategyLifecycleStage::Live,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: None,
            last_evaluation_score: None,
        }
    }

    #[test]
    fn apply_strategy_deployments_enables_economics_domain() {
        let mut cfg = PlatformBootstrapConfig::default();
        let deployments = vec![economics_deployment(true)];

        apply_strategy_deployments(&mut cfg, &deployments, "default", false);

        assert!(cfg.enable_economics);
        assert!(!cfg.enable_crypto);
        assert!(!cfg.enable_sports);
        assert!(!cfg.enable_politics);
    }

    #[test]
    fn apply_strategy_deployments_ignores_disabled_economics_domain() {
        let mut cfg = PlatformBootstrapConfig::default();
        let deployments = vec![economics_deployment(false)];

        apply_strategy_deployments(&mut cfg, &deployments, "default", false);

        assert!(!cfg.enable_economics);
    }

    #[test]
    fn apply_strategy_deployments_does_not_route_unknown_crypto_strategy_to_momentum() {
        let mut cfg = PlatformBootstrapConfig::default();
        let deployments = vec![crypto_deployment("totally_new_crypto_strategy", true)];

        apply_strategy_deployments(&mut cfg, &deployments, "default", false);

        assert!(
            !cfg.enable_crypto,
            "unknown crypto strategy should not auto-enable crypto domain"
        );
        assert!(
            !cfg.enable_crypto_momentum,
            "unknown crypto strategy must not auto-route to momentum"
        );
        assert!(!cfg.enable_crypto_pattern_memory);
        assert!(!cfg.enable_crypto_split_arb);
        assert!(!cfg.enable_crypto_lob_ml);
    }

    #[test]
    fn apply_strategy_deployments_maps_gamma_scalping_alias_to_split_arb() {
        let mut cfg = PlatformBootstrapConfig::default();
        let deployments = vec![crypto_deployment("gamma_scalping", true)];

        apply_strategy_deployments(&mut cfg, &deployments, "default", false);

        assert!(cfg.enable_crypto);
        assert!(cfg.enable_crypto_split_arb);
        assert!(!cfg.enable_crypto_momentum);
    }

    #[test]
    fn build_split_arb_runtime_config_renders_symbols_and_series_ids() {
        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let series_ids = vec!["10192".to_string(), "10684".to_string()];
        let rendered = build_split_arb_runtime_config(&symbols, &series_ids);

        assert!(rendered.contains("symbols = [\"BTCUSDT\", \"ETHUSDT\"]"));
        assert!(rendered.contains("series_ids = [\"10192\", \"10684\"]"));
        assert!(rendered.contains("shares_per_trade = 20"));
        assert!(
            !rendered.contains("fixed_amount_usd"),
            "managed staggered_arb runtime should honor share sizing defaults"
        );
    }

    #[test]
    fn build_split_arb_runtime_config_overrides_template_symbols_and_series_ids() {
        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let series_ids = vec!["10192".to_string(), "10684".to_string()];
        let rendered = build_split_arb_runtime_config(&symbols, &series_ids);

        assert!(rendered.contains("symbols = [\"BTCUSDT\", \"ETHUSDT\"]"));
        assert!(
            !rendered.contains("symbols = [\"BTCUSDT\", \"ETHUSDT\", \"SOLUSDT\"]"),
            "managed runtime should replace template symbols with deployment-scoped symbols"
        );
        assert!(rendered.contains("series_ids = [\"10192\", \"10684\"]"));
    }

    #[test]
    fn build_split_arb_runtime_config_overrides_external_symbols_and_series_ids() {
        let _guard = ENV_LOCK.lock().unwrap();

        let env_key = "PLOY_STAGGERED_ARB_CONFIG";
        let prev = std::env::var(env_key).ok();
        let temp_path = std::env::temp_dir().join(format!(
            "ploy-stag-arb-{}.toml",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(
            &temp_path,
            r#"[strategy]
name = "staggered_arb"

[entry]
symbols = ["SOLUSDT"]
"#,
        )
        .expect("write temp staggered arb config");
        std::env::set_var(env_key, &temp_path);

        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let series_ids = vec!["10192".to_string(), "10684".to_string()];
        let rendered = build_split_arb_runtime_config(&symbols, &series_ids);

        match prev {
            Some(v) => std::env::set_var(env_key, v),
            None => std::env::remove_var(env_key),
        }
        let _ = std::fs::remove_file(&temp_path);

        assert!(rendered.contains("[entry]\nsymbols = [\"BTCUSDT\", \"ETHUSDT\"]"));
        assert!(rendered.contains("[markets]\nseries_ids = [\"10192\", \"10684\"]"));
    }

    #[test]
    fn build_momentum_runtime_config_renders_directional_crypto_settings() {
        let mut cfg = CryptoTradingConfig::default();
        cfg.coins = vec!["ETH".to_string(), "BTCUSDT".to_string()];
        cfg.min_window_move_pct = dec!(0.0007);
        cfg.min_edge = dec!(0.03);
        cfg.min_time_remaining_secs = 90;
        cfg.max_time_remaining_secs = 420;
        cfg.entry_cooldown_secs = 15;
        cfg.default_shares = 42;
        cfg.enable_price_exits = false;
        cfg.min_signal_score = dec!(0.55);
        cfg.risk_params.max_unhedged_positions = 7;

        let rendered = build_momentum_runtime_config(&cfg).expect("render momentum config");
        let value: toml::Value = toml::from_str(&rendered).expect("valid momentum runtime toml");

        assert_eq!(value["strategy"]["name"].as_str(), Some("momentum"));
        assert_eq!(value["strategy"]["mode"].as_str(), Some("confirmatory"));
        assert_eq!(
            value["entry"]["symbols"]
                .as_array()
                .expect("symbols array")
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>(),
            vec!["BTCUSDT", "ETHUSDT"]
        );
        assert_eq!(value["entry"]["directional_mode"].as_bool(), Some(true));
        assert_eq!(value["entry"]["require_mtf_agreement"].as_bool(), Some(true));
        assert_eq!(value["timing"]["min_time_remaining"].as_float(), Some(90.0));
        assert_eq!(value["timing"]["max_time_remaining"].as_float(), Some(420.0));
        assert_eq!(value["risk"]["shares"].as_float(), Some(42.0));
        assert_eq!(value["risk"]["max_positions"].as_float(), Some(7.0));
    }

    #[test]
    fn build_momentum_runtime_config_rejects_non_directional_modes() {
        let mut cfg = CryptoTradingConfig::default();
        cfg.entry_mode = CryptoEntryMode::VolStraddle;

        let err = build_momentum_runtime_config(&cfg).expect_err("non-directional mode rejected");
        assert!(
            err.to_string().contains("only supports directional entry_mode"),
            "unexpected error: {}",
            err
        );
    }

    #[tokio::test]
    async fn ensure_pm_market_metadata_table_exists() {
        let _guard = ENV_LOCK.lock().unwrap();

        let db_url = std::env::var("PLOY_TEST_DATABASE_URL")
            .ok()
            .or_else(|| std::env::var("DATABASE_URL").ok());
        let Some(db_url) = db_url else {
            return;
        };

        let pool = match PgPoolOptions::new()
            .max_connections(1)
            .connect(&db_url)
            .await
        {
            Ok(pool) => pool,
            Err(_) => return,
        };

        ensure_pm_market_metadata_table(&pool)
            .await
            .expect("ensure table");

        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT to_regclass('public.pm_market_metadata') IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .expect("check relation exists");
        assert!(exists);

        let cols = sqlx::query_scalar::<_, String>(
            r#"
            SELECT column_name
            FROM information_schema.columns
            WHERE table_schema = 'public' AND table_name = 'pm_market_metadata'
            "#,
        )
        .fetch_all(&pool)
        .await
        .expect("read table columns");

        for col in [
            "market_slug",
            "price_to_beat",
            "start_time",
            "end_time",
            "horizon",
            "symbol",
            "raw_market",
            "updated_at",
        ] {
            assert!(
                cols.iter().any(|c| c == col),
                "missing pm_market_metadata column: {col}"
            );
        }
    }

    #[test]
    fn from_app_config_reads_crypto_lob_ml_model_env_vars() {
        let _guard = ENV_LOCK.lock().unwrap();

        let model_type_key = "PLOY_CRYPTO_LOB_ML__MODEL_TYPE";
        let model_path_key = "PLOY_CRYPTO_LOB_ML__MODEL_PATH";
        let model_version_key = "PLOY_CRYPTO_LOB_ML__MODEL_VERSION";
        let blend_weight_key = "PLOY_CRYPTO_LOB_ML__MODEL_BLEND_WEIGHT";
        let min_direction_strength_key = "PLOY_CRYPTO_LOB_ML__MIN_DIRECTION_STRENGTH";
        let ev_exit_buffer_key = "PLOY_CRYPTO_LOB_ML__EV_EXIT_BUFFER";
        let ev_exit_vol_scale_key = "PLOY_CRYPTO_LOB_ML__EV_EXIT_VOL_SCALE";
        let taker_fee_key = "PLOY_CRYPTO_LOB_ML__TAKER_FEE_RATE";
        let slippage_key = "PLOY_CRYPTO_LOB_ML__ENTRY_SLIPPAGE_BPS";
        let use_threshold_key = "PLOY_CRYPTO_LOB_ML__USE_PRICE_TO_BEAT";
        let require_threshold_key = "PLOY_CRYPTO_LOB_ML__REQUIRE_PRICE_TO_BEAT";
        let exit_mode_key = "PLOY_CRYPTO_LOB_ML__EXIT_MODE";
        let entry_side_policy_key = "PLOY_CRYPTO_LOB_ML__ENTRY_SIDE_POLICY";
        let entry_late_window_5m_key = "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M";
        let entry_late_window_15m_key = "PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_15M";

        let prev_model_type = std::env::var(model_type_key).ok();
        let prev_model_path = std::env::var(model_path_key).ok();
        let prev_model_version = std::env::var(model_version_key).ok();
        let prev_blend_weight = std::env::var(blend_weight_key).ok();
        let prev_min_direction_strength = std::env::var(min_direction_strength_key).ok();
        let prev_ev_exit_buffer = std::env::var(ev_exit_buffer_key).ok();
        let prev_ev_exit_vol_scale = std::env::var(ev_exit_vol_scale_key).ok();
        let prev_taker_fee = std::env::var(taker_fee_key).ok();
        let prev_slippage = std::env::var(slippage_key).ok();
        let prev_use_threshold = std::env::var(use_threshold_key).ok();
        let prev_require_threshold = std::env::var(require_threshold_key).ok();
        let prev_exit_mode = std::env::var(exit_mode_key).ok();
        let prev_entry_side_policy = std::env::var(entry_side_policy_key).ok();
        let prev_entry_late_window_5m = std::env::var(entry_late_window_5m_key).ok();
        let prev_entry_late_window_15m = std::env::var(entry_late_window_15m_key).ok();

        set_env(model_type_key, Some("onnx"));
        set_env(model_path_key, Some("/tmp/models/lob_tcn_v2.onnx"));
        set_env(model_version_key, Some("lob_tcn_v2"));
        set_env(blend_weight_key, Some("0.75"));
        set_env(min_direction_strength_key, Some("0.06"));
        set_env(ev_exit_buffer_key, Some("0.01"));
        set_env(ev_exit_vol_scale_key, Some("0.03"));
        set_env(taker_fee_key, Some("0.03"));
        set_env(slippage_key, Some("12"));
        set_env(use_threshold_key, Some("true"));
        set_env(require_threshold_key, Some("false"));
        set_env(exit_mode_key, Some("ev_exit"));
        set_env(entry_side_policy_key, Some("lagging_only"));
        set_env(entry_late_window_5m_key, Some("170"));
        set_env(entry_late_window_15m_key, Some("180"));

        let app = AppConfig::default_config(true, "btc-up-or-down-test");
        let cfg = PlatformBootstrapConfig::from_app_config(&app);

        assert_eq!(cfg.crypto_lob_ml.model_type, "onnx");
        assert_eq!(
            cfg.crypto_lob_ml.model_path.as_deref(),
            Some("/tmp/models/lob_tcn_v2.onnx")
        );
        assert_eq!(
            cfg.crypto_lob_ml.model_version.as_deref(),
            Some("lob_tcn_v2")
        );
        assert_eq!(
            cfg.crypto_lob_ml.model_blend_weight,
            rust_decimal::Decimal::new(75, 2)
        );
        assert_eq!(
            cfg.crypto_lob_ml.min_direction_strength,
            rust_decimal::Decimal::new(6, 2)
        );
        assert_eq!(
            cfg.crypto_lob_ml.ev_exit_buffer,
            rust_decimal::Decimal::new(1, 2)
        );
        assert_eq!(
            cfg.crypto_lob_ml.ev_exit_vol_scale,
            rust_decimal::Decimal::new(3, 2)
        );
        assert_eq!(
            cfg.crypto_lob_ml.taker_fee_rate,
            rust_decimal::Decimal::new(3, 2)
        );
        assert_eq!(
            cfg.crypto_lob_ml.entry_slippage_bps,
            rust_decimal::Decimal::new(12, 0)
        );
        assert!(cfg.crypto_lob_ml.use_price_to_beat);
        assert!(!cfg.crypto_lob_ml.require_price_to_beat);
        assert_eq!(cfg.crypto_lob_ml.exit_mode, CryptoLobMlExitMode::EvExit);
        assert_eq!(
            cfg.crypto_lob_ml.entry_side_policy,
            CryptoLobMlEntrySidePolicy::LaggingOnly
        );
        assert_eq!(cfg.crypto_lob_ml.entry_late_window_secs_5m, 170);
        assert_eq!(cfg.crypto_lob_ml.entry_late_window_secs_15m, 180);

        match prev_model_type.as_deref() {
            Some(v) => set_env(model_type_key, Some(v)),
            None => set_env(model_type_key, None),
        }
        match prev_model_path.as_deref() {
            Some(v) => set_env(model_path_key, Some(v)),
            None => set_env(model_path_key, None),
        }
        match prev_model_version.as_deref() {
            Some(v) => set_env(model_version_key, Some(v)),
            None => set_env(model_version_key, None),
        }
        match prev_blend_weight.as_deref() {
            Some(v) => set_env(blend_weight_key, Some(v)),
            None => set_env(blend_weight_key, None),
        }
        match prev_min_direction_strength.as_deref() {
            Some(v) => set_env(min_direction_strength_key, Some(v)),
            None => set_env(min_direction_strength_key, None),
        }
        match prev_ev_exit_buffer.as_deref() {
            Some(v) => set_env(ev_exit_buffer_key, Some(v)),
            None => set_env(ev_exit_buffer_key, None),
        }
        match prev_ev_exit_vol_scale.as_deref() {
            Some(v) => set_env(ev_exit_vol_scale_key, Some(v)),
            None => set_env(ev_exit_vol_scale_key, None),
        }
        match prev_taker_fee.as_deref() {
            Some(v) => set_env(taker_fee_key, Some(v)),
            None => set_env(taker_fee_key, None),
        }
        match prev_slippage.as_deref() {
            Some(v) => set_env(slippage_key, Some(v)),
            None => set_env(slippage_key, None),
        }
        match prev_use_threshold.as_deref() {
            Some(v) => set_env(use_threshold_key, Some(v)),
            None => set_env(use_threshold_key, None),
        }
        match prev_require_threshold.as_deref() {
            Some(v) => set_env(require_threshold_key, Some(v)),
            None => set_env(require_threshold_key, None),
        }
        match prev_exit_mode.as_deref() {
            Some(v) => set_env(exit_mode_key, Some(v)),
            None => set_env(exit_mode_key, None),
        }
        match prev_entry_side_policy.as_deref() {
            Some(v) => set_env(entry_side_policy_key, Some(v)),
            None => set_env(entry_side_policy_key, None),
        }
        match prev_entry_late_window_5m.as_deref() {
            Some(v) => set_env(entry_late_window_5m_key, Some(v)),
            None => set_env(entry_late_window_5m_key, None),
        }
        match prev_entry_late_window_15m.as_deref() {
            Some(v) => set_env(entry_late_window_15m_key, Some(v)),
            None => set_env(entry_late_window_15m_key, None),
        }
    }

    #[test]
    fn from_app_config_reads_crypto_agent_signal_gate_env_vars() {
        let _guard = ENV_LOCK.lock().unwrap();

        let min_momentum_key = "PLOY_CRYPTO_AGENT__MIN_MOMENTUM_1S";
        let min_window_move_key = "PLOY_CRYPTO_AGENT__MIN_WINDOW_MOVE_PCT";
        let min_signal_score_key = "PLOY_CRYPTO_AGENT__MIN_SIGNAL_SCORE";

        let prev_min_momentum = std::env::var(min_momentum_key).ok();
        let prev_min_window_move = std::env::var(min_window_move_key).ok();
        let prev_min_signal_score = std::env::var(min_signal_score_key).ok();

        set_env(min_momentum_key, Some("0.0025"));
        set_env(min_window_move_key, Some("0.0015"));
        set_env(min_signal_score_key, Some("0.65"));

        let app = AppConfig::default_config(true, "btc-up-or-down-test");
        let cfg = PlatformBootstrapConfig::from_app_config(&app);

        assert!((cfg.crypto.min_momentum_1s - 0.0025).abs() < f64::EPSILON);
        assert_eq!(
            cfg.crypto.min_window_move_pct,
            rust_decimal::Decimal::new(15, 4)
        );
        assert_eq!(
            cfg.crypto.min_signal_score,
            rust_decimal::Decimal::new(65, 2)
        );

        match prev_min_momentum.as_deref() {
            Some(v) => set_env(min_momentum_key, Some(v)),
            None => set_env(min_momentum_key, None),
        }
        match prev_min_window_move.as_deref() {
            Some(v) => set_env(min_window_move_key, Some(v)),
            None => set_env(min_window_move_key, None),
        }
        match prev_min_signal_score.as_deref() {
            Some(v) => set_env(min_signal_score_key, Some(v)),
            None => set_env(min_signal_score_key, None),
        }
    }

    #[test]
    fn from_app_config_ignores_legacy_enable_price_exits_env() {
        let _guard = ENV_LOCK.lock().unwrap();

        let exit_mode_key = "PLOY_CRYPTO_LOB_ML__EXIT_MODE";
        let legacy_price_exits_key = "PLOY_CRYPTO_LOB_ML__ENABLE_PRICE_EXITS";

        let prev_exit_mode = std::env::var(exit_mode_key).ok();
        let prev_legacy_price_exits = std::env::var(legacy_price_exits_key).ok();

        set_env(exit_mode_key, None);
        set_env(legacy_price_exits_key, Some("true"));

        let app = AppConfig::default_config(true, "btc-up-or-down-test");
        let cfg = PlatformBootstrapConfig::from_app_config(&app);

        assert_eq!(cfg.crypto_lob_ml.exit_mode, CryptoLobMlExitMode::EvExit);

        match prev_exit_mode.as_deref() {
            Some(v) => set_env(exit_mode_key, Some(v)),
            None => set_env(exit_mode_key, None),
        }
        match prev_legacy_price_exits.as_deref() {
            Some(v) => set_env(legacy_price_exits_key, Some(v)),
            None => set_env(legacy_price_exits_key, None),
        }
    }
}
