//! Integration test: full backtest loop through StrategyRuntime.
//!
//! Simulates a complete pm_5m_directional backtest with:
//! - Event discovered → spot price moves → quotes update → entry signal
//! - SimulatedExecutor produces realistic fill
//! - TradingRuntime tracks position and PnL

use chrono::{Duration, Utc};
use ploy_strategy_bundles::config::FullConfig;
use ploy_strategy_bundles::strategies::directional::DirectionalConfig;
use ploy_strategy_bundles::{
    DirectionalStrategy, HistoricalFeed, MarketUpdate, NullRecorder, RecordedFeed, RecordingFeed,
    RuntimeConfig, RuntimeMode, SimulatedExecutor, SimulatedExecutorConfig, StrategyRuntime,
};
use rust_decimal_macros::dec;
use std::fs;

/// Helper: build a sequence of market updates simulating one 5-min window.
fn build_scenario() -> Vec<MarketUpdate> {
    let now = Utc::now();
    let event_end = now + Duration::seconds(180); // 3 min from now
    let mut updates = Vec::new();

    // 1. Initial spot price FIRST (so event picks it up as open_price)
    updates.push(MarketUpdate::SpotPrice {
        symbol: "BTCUSDT".into(),
        price: dec!(100000),
        ts: now,
    });

    // 2. Event discovered (captures current spot as open_price)
    updates.push(MarketUpdate::EventDiscovered {
        event_id: "evt-btc-001".into(),
        symbol: "BTCUSDT".into(),
        up_token: "up-btc-001".into(),
        down_token: "dn-btc-001".into(),
        end_time: event_end,
        window_secs: 300,
        price_to_beat: None,
        resolved_up_won: Some(true),
    });

    // 3. Polymarket quotes — UP token cheap at 0.30
    updates.push(MarketUpdate::Quote {
        token_id: "up-btc-001".into(),
        bid: Some(dec!(0.29)),
        ask: Some(dec!(0.30)),
        ts: now,
    });
    updates.push(MarketUpdate::Quote {
        token_id: "dn-btc-001".into(),
        bid: Some(dec!(0.69)),
        ask: Some(dec!(0.70)),
        ts: now,
    });

    // 4. BTC trends up over several updates so realized vol has a usable estimate.
    updates.push(MarketUpdate::SpotPrice {
        symbol: "BTCUSDT".into(),
        price: dec!(100400),
        ts: now + Duration::seconds(20),
    });

    updates.push(MarketUpdate::SpotPrice {
        symbol: "BTCUSDT".into(),
        price: dec!(100900),
        ts: now + Duration::seconds(40),
    });

    updates.push(MarketUpdate::SpotPrice {
        symbol: "BTCUSDT".into(),
        price: dec!(101500),
        ts: now + Duration::seconds(60),
    });

    // 6. Event expires (settlement)
    updates.push(MarketUpdate::EventExpired {
        event_id: "evt-btc-001".into(),
        end_time: event_end,
        resolved_up_won: None, // synthetic data — use spot fallback
    });

    updates
}

#[tokio::test]
async fn backtest_full_loop_produces_entry() {
    let config = DirectionalConfig {
        symbols: vec!["BTCUSDT".into()],
        vol_floor: 0.001,
        min_probability: 0.55,
        min_z_score: 0.35,
        min_entry_price: 0.15,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: 0.02,
        min_time_remaining_secs: 60,
        max_time_remaining_secs: 300,
        cooldown_secs: 15,
        stake_usd: dec!(25),
        max_positions: 30,
        max_daily_trades: 1000,
    };

    let strategy = DirectionalStrategy::new(config);
    let feed = HistoricalFeed::new(build_scenario());
    let executor = SimulatedExecutor::new(SimulatedExecutorConfig {
        use_spread: true,
        spread_pct: dec!(0.02),
        enable_partial_fills: false,
        enable_market_impact: false,
        ..Default::default()
    });
    let recorder = Box::new(NullRecorder);
    let runtime_config = RuntimeConfig {
        mode: RuntimeMode::Backtest,
        throttle_hz: None,
        max_updates: None,
    };

    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = runtime.run().await;

    assert_eq!(result.mode, RuntimeMode::Backtest);
    assert_eq!(result.updates_processed, 8);
    assert!(
        result.intents_submitted >= 1,
        "Expected at least 1 intent, got {}",
        result.intents_submitted
    );
    assert!(
        result.fills_recorded >= 1,
        "Expected at least 1 fill, got {}",
        result.fills_recorded
    );

    // Position should exist
    let trading = runtime.trading();
    let snapshot = trading.snapshot(&std::collections::BTreeMap::new());
    assert!(
        !snapshot.fills.is_empty(),
        "Expected fills in trading runtime"
    );
}

#[tokio::test]
async fn toml_config_drives_backtest() {
    let toml = r#"
[runtime]
mode = "backtest"

[strategy]
symbols = ["BTCUSDT"]
min_edge = 0.02
min_time_remaining_secs = 60
max_time_remaining_secs = 300
cooldown_secs = 15
stake_usd = 25.0
max_positions = 30
max_daily_trades = 1000

[execution]
use_spread = false
enable_partial_fills = false
enable_market_impact = false
"#;

    let config = FullConfig::from_toml(toml).unwrap();
    let sim_config = config.sim_executor_config();
    let runtime_config = config.runtime_config();

    let strategy = DirectionalStrategy::new(config.strategy);
    let feed = HistoricalFeed::new(build_scenario());
    let executor = SimulatedExecutor::new(sim_config);
    let recorder = Box::new(NullRecorder);

    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = runtime.run().await;

    assert_eq!(result.mode, RuntimeMode::Backtest);
    // No spread/impact → fill at signal price
    assert!(result.fills_recorded >= 1);
}

#[tokio::test]
async fn empty_feed_produces_zero_trades() {
    let config = DirectionalConfig {
        symbols: vec!["BTCUSDT".into()],
        vol_floor: 0.001,
        min_probability: 0.55,
        min_z_score: 0.35,
        min_entry_price: 0.15,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: 0.02,
        min_time_remaining_secs: 60,
        max_time_remaining_secs: 300,
        cooldown_secs: 15,
        stake_usd: dec!(25),
        max_positions: 30,
        max_daily_trades: 1000,
    };

    let strategy = DirectionalStrategy::new(config);
    let feed = HistoricalFeed::new(vec![]); // empty
    let executor = SimulatedExecutor::new(SimulatedExecutorConfig::default());
    let recorder = Box::new(NullRecorder);
    let runtime_config = RuntimeConfig {
        mode: RuntimeMode::Backtest,
        throttle_hz: None,
        max_updates: None,
    };

    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = runtime.run().await;

    assert_eq!(result.updates_processed, 0);
    assert_eq!(result.intents_submitted, 0);
    assert_eq!(result.fills_recorded, 0);
}

#[tokio::test]
async fn recorded_updates_replay_to_the_same_runtime_result() {
    let config = DirectionalConfig {
        symbols: vec!["BTCUSDT".into()],
        vol_floor: 0.001,
        min_probability: 0.55,
        min_z_score: 0.35,
        min_entry_price: 0.15,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: 0.02,
        min_time_remaining_secs: 60,
        max_time_remaining_secs: 300,
        cooldown_secs: 15,
        stake_usd: dec!(25),
        max_positions: 30,
        max_daily_trades: 1000,
    };
    let sim_config = SimulatedExecutorConfig {
        use_spread: true,
        spread_pct: dec!(0.02),
        enable_partial_fills: false,
        enable_market_impact: false,
        ..Default::default()
    };
    let runtime_config = RuntimeConfig {
        mode: RuntimeMode::DryRun,
        throttle_hz: None,
        max_updates: None,
    };

    let mut record_path = std::env::temp_dir();
    record_path.push(format!(
        "ploy-replay-parity-{}.ndjson",
        uuid::Uuid::new_v4()
    ));

    let record_feed =
        RecordingFeed::new(HistoricalFeed::new(build_scenario()), &record_path).unwrap();
    let mut recorded_runtime = StrategyRuntime::new(
        DirectionalStrategy::new(config.clone()),
        record_feed,
        SimulatedExecutor::new(sim_config.clone()),
        Box::new(NullRecorder),
        runtime_config.clone(),
    );
    let recorded_result = recorded_runtime.run().await;
    let recorded_snapshot = recorded_runtime
        .trading()
        .snapshot(&std::collections::BTreeMap::new());

    let replay_feed = RecordedFeed::from_path(&record_path).unwrap();
    let mut replay_runtime = StrategyRuntime::new(
        DirectionalStrategy::new(config),
        replay_feed,
        SimulatedExecutor::new(sim_config),
        Box::new(NullRecorder),
        RuntimeConfig {
            mode: RuntimeMode::Replay,
            ..runtime_config
        },
    );
    let replay_result = replay_runtime.run().await;
    let replay_snapshot = replay_runtime
        .trading()
        .snapshot(&std::collections::BTreeMap::new());

    assert_eq!(
        recorded_result.updates_processed,
        replay_result.updates_processed
    );
    assert_eq!(
        recorded_result.intents_submitted,
        replay_result.intents_submitted
    );
    assert_eq!(recorded_result.fills_recorded, replay_result.fills_recorded);
    assert_eq!(recorded_result.pnl.net_pnl(), replay_result.pnl.net_pnl());
    assert_eq!(
        recorded_snapshot.fill_cashflow_summary(),
        replay_snapshot.fill_cashflow_summary()
    );
    assert_eq!(recorded_snapshot.fills.len(), replay_snapshot.fills.len());

    let _ = fs::remove_file(record_path);
}
