//! Integration test: full backtest loop through StrategyRuntime.
//!
//! Simulates a complete pm_5m_directional backtest with:
//! - Event discovered → spot price moves → quotes update → entry signal
//! - SimulatedExecutor produces realistic fill
//! - TradingRuntime tracks position and PnL

use chrono::{Duration, Utc};
use ploy_strategy_bundles::{
    DirectionalStrategy, HistoricalFeed, MarketUpdate, NullRecorder, RuntimeConfig, RuntimeMode,
    SimulatedExecutor, SimulatedExecutorConfig, StrategyRuntime,
};
use ploy_strategy_bundles::config::FullConfig;
use ploy_strategy_bundles::strategies::directional::DirectionalConfig;
use rust_decimal_macros::dec;

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
    });

    updates
}

#[tokio::test]
async fn backtest_full_loop_produces_entry() {
    let config = DirectionalConfig {
        symbols: vec!["BTCUSDT".into()],
        vol_floor: 0.001,
        min_probability: 0.62,
        min_z_score: 0.35,
        min_entry_price: 0.15,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: 0.05,
        min_time_remaining_secs: 60,
        max_time_remaining_secs: 300,
        cooldown_secs: 60,
        quantity: dec!(25),
        max_positions: 3,
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
    assert!(result.intents_submitted >= 1, "Expected at least 1 intent, got {}", result.intents_submitted);
    assert!(result.fills_recorded >= 1, "Expected at least 1 fill, got {}", result.fills_recorded);

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
min_edge = 0.05
min_time_remaining_secs = 60
max_time_remaining_secs = 300
cooldown_secs = 60
quantity = 25.0
max_positions = 3
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
        min_probability: 0.62,
        min_z_score: 0.35,
        min_entry_price: 0.15,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: 0.05,
        min_time_remaining_secs: 60,
        max_time_remaining_secs: 300,
        cooldown_secs: 60,
        quantity: dec!(25),
        max_positions: 3,
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
