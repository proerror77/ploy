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
use crate::config::AppConfig;
use crate::control_plane::{MarketSelector, StrategyDeployment};
use crate::coordinator::{Coordinator, CoordinatorHandle, GlobalState};
use crate::domain::Side;
use crate::error::Result;
use crate::exchange::{build_exchange_client, parse_exchange_kind, ExchangeKind};
use crate::platform::{
    BinanceDataPlaneHandle, DataPlaneConfig, Domain, PlatformDataPlane,
};
use crate::signing::Wallet;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::idempotency::IdempotencyManager;
use crate::strategy::momentum::EventMatcher;
use crate::strategy::DataFeed;
use chrono::Utc;
use futures_util::StreamExt;
use polymarket_client_sdk::data::types::request::TradesRequest as DataTradesRequest;
use polymarket_client_sdk::data::types::MarketFilter as DataMarketFilter;
use polymarket_client_sdk::data::Client as DataApiClient;

use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::instrument;

use super::strategy_runtime::run_managed_strategy_runtime;

mod bootstrap_config;
mod coordinator_bootstrap;
mod crypto_runtime_support;
mod managed_crypto;
mod market_persistence;
mod openclaw_config;
mod runtime_config;
mod runtime_orchestration;
mod runtime_spawns;
mod schema;
mod sports_runtime_support;
mod startup_context;
mod strategy_deployments;
mod support;

pub use self::bootstrap_config::PlatformBootstrapConfig;
pub use self::openclaw_config::{
    AllocatorConfig, OpenClawConfig, RegimeConfig, StraddleConfig,
};
use self::coordinator_bootstrap::initialize_coordinator_runtime;
use self::market_persistence::{
    ensure_clob_trade_alerts_table, spawn_pm_token_settlement_persistence,
    spawn_polymarket_trade_persistence, spawn_polymarket_trade_persistence_from_collector_targets,
};
use self::crypto_runtime_support::initialize_crypto_runtime_support;
use self::runtime_spawns::{
    spawn_managed_strategy_runtime_task, spawn_openclaw_governance_agent,
};
use self::schema::{
    ensure_accounts_table, ensure_binance_lob_ticks_table, ensure_binance_price_ticks_table,
    ensure_clob_quote_ticks_table, ensure_schema_repairs, upsert_account_from_config,
};
pub(crate) use self::schema::{
    ensure_agent_order_executions_table, ensure_clob_orderbook_snapshots_table,
    ensure_coordinator_governance_policies_table,
    ensure_coordinator_governance_policy_history_table, ensure_pm_market_metadata_table,
    ensure_pm_token_settlements_table, ensure_risk_runtime_state_table,
    ensure_strategy_observability_tables,
};
#[cfg(all(test, feature = "rl"))]
use self::strategy_deployments::build_crypto_rl_policy_runtime_config;
#[cfg(test)]
use self::strategy_deployments::{
    build_crypto_lob_ml_runtime_config, build_event_edge_runtime_config,
    build_momentum_runtime_config, build_nba_comeback_runtime_config,
    build_split_arb_runtime_config,
};
use self::strategy_deployments::{
    collect_managed_strategy_runtime_plans, collect_runtime_crypto_strategy_targets,
    ManagedRuntimeBootstrapStep, ManagedRuntimeDataPlaneKind,
};
use self::support::{
    add_coins_from_selector, env_bool, env_i64, env_u64, env_usize, lob_levels_json,
};
use self::sports_runtime_support::prepare_sports_runtime_support;
use self::startup_context::initialize_startup_context;
use self::runtime_orchestration::run_platform_runtime;

const CLOB_PERSIST_MIN_INTERVAL_SECS: i64 = 2;
const BINANCE_PERSIST_MIN_INTERVAL_SECS: i64 = 1;
const PM_COLLECTOR_REFRESH_SECS: u64 = 300;

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
    let startup = initialize_startup_context(&config, app_config).await?;
    let exchange_client = startup.exchange_client;
    let pm_client = startup.pm_client;
    let account_id = startup.account_id;
    let runtime_crypto_targets = startup.runtime_crypto_targets;
    let allowed_domains = startup.allowed_domains;
    let shared_pool = startup.shared_pool;

    let coordinator_bootstrap = initialize_coordinator_runtime(
        &config,
        &app_config,
        exchange_client.clone(),
        &account_id,
        allowed_domains.clone(),
        shared_pool.as_ref(),
    )
    .await?;
    let coordinator = coordinator_bootstrap.coordinator;
    let handle = coordinator_bootstrap.handle;
    let _api_handle = coordinator_bootstrap.api_handle;
    run_platform_runtime(
        &config,
        app_config,
        &control,
        coordinator,
        handle,
        pm_client,
        shared_pool,
        account_id,
        &runtime_crypto_targets,
    )
    .await
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
    use crate::coordinator::bootstrap::strategy_deployments::apply_strategy_deployments;
    use crate::control_plane::{
        DeploymentExecutionMode, StrategyLifecycleStage, StrategyProductType, Timeframe,
    };
    use crate::strategy::crypto_lob_ml::{
        CryptoLobMlConfig, CryptoLobMlEntrySidePolicy, CryptoLobMlExitMode,
    };
    use crate::strategy::CryptoTradingConfig;
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
        assert!(!cfg.managed_crypto.enable_lob_ml);
    }

    #[test]
    fn collect_managed_strategy_runtime_plans_collapses_crypto_spawn_specs() {
        let mut cfg = PlatformBootstrapConfig::default();
        cfg.enable_crypto = true;
        cfg.enable_crypto_momentum = true;
        cfg.enable_crypto_pattern_memory = true;
        cfg.enable_crypto_split_arb = true;
        cfg.managed_crypto.enable_lob_ml = true;
        cfg.crypto.coins = vec!["BTC".to_string(), "ETH".to_string()];
        cfg.managed_crypto.lob_ml.coins = vec!["BTC".to_string()];

        let app_config = AppConfig::default_config(true, "btc-up-down");
        let plans = collect_managed_strategy_runtime_plans(
            &cfg,
            &app_config,
            &strategy_deployments::RuntimeCryptoStrategyTargets::default(),
        );

        let labels: Vec<&str> = plans.iter().map(|plan| plan.spawn.strategy_label).collect();
        assert!(labels.contains(&"momentum"));
        assert!(labels.contains(&"pattern_memory"));
        assert!(labels.contains(&"staggered_arb"));
        assert!(labels.contains(&"crypto_lob_ml"));

        let momentum = plans
            .iter()
            .find(|plan| plan.spawn.strategy_label == "momentum")
            .expect("momentum plan");
        assert_eq!(
            momentum.data_plane,
            ManagedRuntimeDataPlaneKind::ManagedCrypto
        );

        let pattern_memory = plans
            .iter()
            .find(|plan| plan.spawn.strategy_label == "pattern_memory")
            .expect("pattern_memory plan");
        assert_eq!(
            pattern_memory.bootstrap_step,
            ManagedRuntimeBootstrapStep::EnsurePatternMemoryTable
        );
        assert_eq!(
            pattern_memory.data_plane,
            ManagedRuntimeDataPlaneKind::ManagedCrypto
        );

        let split_arb = plans
            .iter()
            .find(|plan| plan.spawn.strategy_label == "staggered_arb")
            .expect("staggered_arb plan");
        assert_eq!(
            split_arb.data_plane,
            ManagedRuntimeDataPlaneKind::SharedCrypto
        );
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
        assert_eq!(
            value["entry"]["require_mtf_agreement"].as_bool(),
            Some(true)
        );
        assert_eq!(value["timing"]["min_time_remaining"].as_float(), Some(90.0));
        assert_eq!(
            value["timing"]["max_time_remaining"].as_float(),
            Some(420.0)
        );
        assert_eq!(value["risk"]["shares"].as_float(), Some(42.0));
        assert_eq!(value["risk"]["max_positions"].as_float(), Some(7.0));
    }

    #[test]
    fn build_momentum_runtime_config_rejects_non_directional_modes() {
        let mut cfg = CryptoTradingConfig::default();
        cfg.entry_mode = crate::strategy::CryptoEntryMode::VolStraddle;

        let err = build_momentum_runtime_config(&cfg).expect_err("non-directional mode rejected");
        assert!(
            err.to_string()
                .contains("only supports directional entry_mode"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn build_crypto_lob_ml_runtime_config_renders_coin_filters() {
        let mut cfg = CryptoLobMlConfig::default();
        cfg.coins = vec!["btc".to_string(), "ETH".to_string()];
        cfg.min_time_remaining_secs = 90;
        cfg.max_time_remaining_secs = 600;
        cfg.max_time_remaining_secs_5m = 180;
        cfg.max_time_remaining_secs_15m = 300;
        cfg.require_price_to_beat = true;
        cfg.max_lob_snapshot_age_secs = 3;

        let rendered =
            build_crypto_lob_ml_runtime_config(&cfg).expect("render crypto_lob_ml config");
        let value: toml::Value =
            toml::from_str(&rendered).expect("valid crypto_lob_ml runtime toml");

        assert_eq!(value["strategy"]["name"].as_str(), Some("crypto_lob_ml"));
        assert_eq!(
            value["crypto_lob_ml"]["coins"]
                .as_array()
                .expect("coins array")
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>(),
            vec!["BTC", "ETH"]
        );
        assert_eq!(
            value["crypto_lob_ml"]["tick_interval_ms"].as_integer(),
            Some(1000)
        );
    }

    #[cfg(feature = "rl")]
    #[test]
    fn build_crypto_rl_policy_runtime_config_preserves_model_controls() {
        let mut cfg = crate::strategy::crypto_rl_policy::CryptoRlPolicyConfig::default();
        cfg.coins = vec!["sol".to_string()];
        cfg.min_time_remaining_secs = 75;
        cfg.max_time_remaining_secs = 420;
        cfg.default_shares = 25;
        cfg.max_entry_price = dec!(0.62);
        cfg.max_lob_snapshot_age_secs = 4;
        cfg.decision_interval_ms = 1500;
        cfg.observation_version = 1;
        cfg.policy_output = "discrete_probs".to_string();
        cfg.policy_model_path = Some("/tmp/model.onnx".to_string());
        cfg.policy_model_version = Some("vtest".to_string());

        let rendered =
            build_crypto_rl_policy_runtime_config(&cfg).expect("render crypto_rl_policy config");
        let value: toml::Value =
            toml::from_str(&rendered).expect("valid crypto_rl_policy runtime toml");

        assert_eq!(value["strategy"]["name"].as_str(), Some("crypto_rl_policy"));
        assert_eq!(
            value["crypto_rl_policy"]["coins"]
                .as_array()
                .expect("coins array")
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>(),
            vec!["SOL"]
        );
        assert_eq!(
            value["crypto_rl_policy"]["policy_output"].as_str(),
            Some("discrete_probs")
        );
        assert_eq!(
            value["crypto_rl_policy"]["policy_model_path"].as_str(),
            Some("/tmp/model.onnx")
        );
        assert_eq!(
            value["crypto_rl_policy"]["tick_interval_ms"].as_integer(),
            Some(1500)
        );
    }

    #[test]
    fn build_event_edge_runtime_config_renders_targets_and_limits() {
        let cfg = crate::config::EventEdgeAgentConfig {
            enabled: true,
            framework: "event_driven".to_string(),
            event_ids: vec!["1234".to_string()],
            titles: vec!["OpenAI funding".to_string()],
            interval_secs: 45,
            min_edge: dec!(0.07),
            max_entry: dec!(0.62),
            shares: 33,
            trade: false,
            cooldown_secs: 180,
            max_daily_spend_usd: dec!(75),
            model: Some("claude-test".to_string()),
            claude_max_turns: 9,
        };

        let rendered = build_event_edge_runtime_config(&cfg).expect("render event_edge config");
        let value: toml::Value = toml::from_str(&rendered).expect("valid event_edge runtime toml");

        assert_eq!(value["strategy"]["name"].as_str(), Some("event_edge"));
        assert_eq!(
            value["event_edge"]["event_ids"]
                .as_array()
                .expect("event_ids array")
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>(),
            vec!["1234"]
        );
        assert_eq!(
            value["event_edge"]["titles"]
                .as_array()
                .expect("titles array")
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>(),
            vec!["OpenAI funding"]
        );
        assert_eq!(value["event_edge"]["trade"].as_bool(), Some(false));
        assert_eq!(value["event_edge"]["interval_secs"].as_integer(), Some(45));
        assert_eq!(value["event_edge"]["shares"].as_integer(), Some(33));
        assert_eq!(value["event_edge"]["model"].as_str(), Some("claude-test"));
    }

    #[test]
    fn build_event_edge_runtime_config_rejects_empty_targets() {
        let cfg = crate::config::EventEdgeAgentConfig {
            event_ids: Vec::new(),
            titles: Vec::new(),
            ..Default::default()
        };

        let err =
            build_event_edge_runtime_config(&cfg).expect_err("event_edge config without targets");
        assert!(
            err.to_string()
                .contains("requires at least one event_id or title"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn build_nba_comeback_runtime_config_renders_risk_controls() {
        let cfg = crate::config::NbaComebackConfig {
            enabled: true,
            min_edge: dec!(0.06),
            max_entry_price: dec!(0.61),
            shares: 42,
            cooldown_secs: 90,
            max_daily_spend_usd: dec!(120),
            min_deficit: 2,
            max_deficit: 18,
            target_quarter: 3,
            espn_poll_interval_secs: 20,
            min_comeback_rate: 0.22,
            season: "2026-27".to_string(),
            grok_enabled: true,
            grok_interval_secs: 180,
            grok_min_edge: dec!(0.09),
            grok_min_confidence: 0.7,
            grok_decision_cooldown_secs: 30,
            grok_fallback_enabled: false,
            min_reward_risk_ratio: 5.0,
            min_expected_value: 0.06,
            kelly_fraction_cap: 0.20,
            performance_daily_loss_limit_usd: dec!(40),
            performance_min_settled_trades: 12,
            performance_min_win_rate: 0.5,
            performance_low_winrate_multiplier: 0.5,
            performance_loss_streak_threshold: 4,
            performance_loss_streak_multiplier: 0.4,
            scaling_enabled: true,
            scaling_max_adds: 2,
            scaling_min_price_drop_pct: 4.0,
            scaling_max_game_exposure_usd: dec!(80),
            scaling_min_comeback_retention: 0.75,
            scaling_min_time_remaining_mins: 9.0,
            early_exit_enabled: true,
            early_exit_take_profit_pct: 12.0,
            early_exit_stop_loss_pct: 18.0,
        };

        let rendered = build_nba_comeback_runtime_config(&cfg, "postgres://localhost/unused");
        let value: toml::Value =
            toml::from_str(&rendered).expect("valid nba_comeback runtime toml");

        assert_eq!(value["strategy"]["name"].as_str(), Some("nba_comeback"));
        assert_eq!(value["nba_comeback"]["shares"].as_integer(), Some(42));
        assert_eq!(value["nba_comeback"]["season"].as_str(), Some("2026-27"));
        assert_eq!(value["nba_comeback"]["grok_enabled"].as_bool(), Some(true));
        assert_eq!(
            value["nba_comeback"]["performance_min_settled_trades"].as_integer(),
            Some(12)
        );
        assert_eq!(
            value["nba_comeback"]["scaling_enabled"].as_bool(),
            Some(true)
        );
        assert_eq!(
            value["nba_comeback"]["early_exit_enabled"].as_bool(),
            Some(true)
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

        assert_eq!(cfg.managed_crypto.lob_ml.model_type, "onnx");
        assert_eq!(
            cfg.managed_crypto.lob_ml.model_path.as_deref(),
            Some("/tmp/models/lob_tcn_v2.onnx")
        );
        assert_eq!(
            cfg.managed_crypto.lob_ml.model_version.as_deref(),
            Some("lob_tcn_v2")
        );
        assert_eq!(
            cfg.managed_crypto.lob_ml.model_blend_weight,
            rust_decimal::Decimal::new(75, 2)
        );
        assert_eq!(
            cfg.managed_crypto.lob_ml.min_direction_strength,
            rust_decimal::Decimal::new(6, 2)
        );
        assert_eq!(
            cfg.managed_crypto.lob_ml.ev_exit_buffer,
            rust_decimal::Decimal::new(1, 2)
        );
        assert_eq!(
            cfg.managed_crypto.lob_ml.ev_exit_vol_scale,
            rust_decimal::Decimal::new(3, 2)
        );
        assert_eq!(
            cfg.managed_crypto.lob_ml.taker_fee_rate,
            rust_decimal::Decimal::new(3, 2)
        );
        assert_eq!(
            cfg.managed_crypto.lob_ml.entry_slippage_bps,
            rust_decimal::Decimal::new(12, 0)
        );
        assert!(cfg.managed_crypto.lob_ml.use_price_to_beat);
        assert!(!cfg.managed_crypto.lob_ml.require_price_to_beat);
        assert_eq!(
            cfg.managed_crypto.lob_ml.exit_mode,
            CryptoLobMlExitMode::EvExit
        );
        assert_eq!(
            cfg.managed_crypto.lob_ml.entry_side_policy,
            CryptoLobMlEntrySidePolicy::LaggingOnly
        );
        assert_eq!(cfg.managed_crypto.lob_ml.entry_late_window_secs_5m, 170);
        assert_eq!(cfg.managed_crypto.lob_ml.entry_late_window_secs_15m, 180);

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
    fn from_app_config_ignores_deprecated_price_exits_env() {
        let _guard = ENV_LOCK.lock().unwrap();

        let exit_mode_key = "PLOY_CRYPTO_LOB_ML__EXIT_MODE";
        let legacy_price_exits_key = "PLOY_CRYPTO_LOB_ML__ENABLE_PRICE_EXITS";

        let prev_exit_mode = std::env::var(exit_mode_key).ok();
        let prev_legacy_price_exits = std::env::var(legacy_price_exits_key).ok();

        set_env(exit_mode_key, None);
        set_env(legacy_price_exits_key, Some("true"));

        let app = AppConfig::default_config(true, "btc-up-or-down-test");
        let cfg = PlatformBootstrapConfig::from_app_config(&app);

        assert_eq!(
            cfg.managed_crypto.lob_ml.exit_mode,
            CryptoLobMlExitMode::EvExit
        );

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
