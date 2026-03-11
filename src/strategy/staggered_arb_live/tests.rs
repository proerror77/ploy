use super::*;
use crate::domain::OrderStatus;
use crate::strategy::OrderUpdate;

fn default_config() -> StaggeredArbLiveConfig {
    StaggeredArbLiveConfig {
        backtest_config: StaggeredArbBacktestConfig::default(),
        fee_rate: dec!(0.015),
    }
}

fn sample_leg1_track(now: DateTime<Utc>) -> LiveOrderTrack {
    LiveOrderTrack {
        event_id: "evt-1".to_string(),
        condition_id: Some("cond-1".to_string()),
        symbol: "ETHUSDT".to_string(),
        up_token: "up-token".to_string(),
        down_token: "down-token".to_string(),
        direction: Direction::Up,
        token_id: "up-token".to_string(),
        leg: 1,
        price: dec!(0.51),
        shares: 20,
        position_idx: None,
        close_reason: None,
        submitted_at: now - chrono::Duration::seconds(35),
        cancel_requested_at: Some(now - chrono::Duration::seconds(5)),
        exchange_order_id: Some("0xabc".to_string()),
        acknowledged_filled_qty: 0,
        entry_obi: Some(0.02),
    }
}

fn sample_leg2_track(now: DateTime<Utc>, shares: u64, idx: usize) -> LiveOrderTrack {
    LiveOrderTrack {
        event_id: "evt-1".to_string(),
        condition_id: Some("cond-1".to_string()),
        symbol: "ETHUSDT".to_string(),
        up_token: "up-token".to_string(),
        down_token: "down-token".to_string(),
        direction: Direction::Down,
        token_id: "down-token".to_string(),
        leg: 2,
        price: dec!(0.38),
        shares,
        position_idx: Some(idx),
        close_reason: Some("merge".to_string()),
        submitted_at: now - chrono::Duration::seconds(10),
        cancel_requested_at: None,
        exchange_order_id: Some("0xleg2".to_string()),
        acknowledged_filled_qty: 0,
        entry_obi: Some(-0.02),
    }
}

fn seed_persistent_pm_quotes(
    adapter: &mut StaggeredArbAdapter,
    event_id: &str,
    up_ask: Option<Decimal>,
    down_ask: Option<Decimal>,
    first_seen_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
) {
    adapter.record_pm_quote(event_id, Direction::Up, up_ask, None, first_seen_at);
    adapter.record_pm_quote(event_id, Direction::Down, down_ask, None, first_seen_at);
    if last_seen_at != first_seen_at {
        adapter.record_pm_quote(event_id, Direction::Up, up_ask, None, last_seen_at);
        adapter.record_pm_quote(event_id, Direction::Down, down_ask, None, last_seen_at);
    }
}

#[test]
fn test_adapter_creation() {
    let adapter = StaggeredArbAdapter::new("test_stag".to_string(), default_config(), true);
    assert_eq!(adapter.id(), "test_stag");
    assert_eq!(adapter.name(), "Staggered Arbitrage");
    assert!(!adapter.is_active());
    assert_eq!(adapter.equity, dec!(10000));
}

#[test]
fn test_series_mapping() {
    assert_eq!(
        StaggeredArbAdapter::series_to_symbol("10684"),
        Some(("BTCUSDT", 300)),
    );
    assert_eq!(
        StaggeredArbAdapter::series_to_symbol("10192"),
        Some(("BTCUSDT", 900)),
    );
    assert_eq!(StaggeredArbAdapter::series_to_symbol("99999"), None);
}

#[test]
fn test_from_toml() {
    let toml = r#"
[strategy]
name = "staggered_arb"

[entry]
symbols = ["BTCUSDT"]
shares_per_trade = 20
max_concurrent = 3
direction_threshold = 0.03
premium_sum_threshold = 1.00
premium_sum_direction_slope = 1.25
premium_sum_obi_slope = 0.25
max_initial_sum = 1.20
max_leg1_price = 0.80
merge_target_sum = 0.95
min_profit_target = 0.02
min_ask_price = 0.05
min_entry_sum = 0.70

[timing]
max_wait_secs = 180
max_wait_pct = 0.40
min_time_remaining = 60
cooldown_secs = 5
min_leg2_delay_secs = 3
max_trades_per_event = 2

[risk]
max_leg1_loss = 0.0
force_complete_threshold = 1.00

[model]
mu = 0.0
vol_lookback_secs = 300
vol_floor = 0.005

[filter]
allowed_windows = [300, 900]
window_tolerance = 30

[markets]
series_ids = ["10684", "10192", "10684"]
"#;
    let adapter = StaggeredArbAdapter::from_toml("test".into(), toml, true).unwrap();
    assert_eq!(adapter.config.backtest_config.shares_per_trade, 20);
    assert_eq!(adapter.config.backtest_config.max_concurrent_positions, 3);
    assert_eq!(
        adapter.config.backtest_config.premium_sum_threshold,
        Decimal::ONE
    );
    assert_eq!(
        adapter.config.backtest_config.premium_sum_direction_slope,
        1.25
    );
    assert_eq!(adapter.config.backtest_config.premium_sum_obi_slope, 0.25);
    assert_eq!(adapter.config.backtest_config.obi_confirm_threshold, 0.005);
    assert_eq!(adapter.config.backtest_config.strong_obi_threshold, 0.015);
    assert_eq!(adapter.config.backtest_config.symbols, vec!["BTCUSDT"]);
    assert_eq!(
        adapter.series_ids,
        vec!["10192".to_string(), "10684".to_string()]
    );
}

#[test]
fn test_from_toml_defaults_match_delayed_entry_profile() {
    let toml = r#"
[strategy]
name = "staggered_arb"
"#;

    let adapter = StaggeredArbAdapter::from_toml("test".into(), toml, true).unwrap();
    let config = &adapter.config.backtest_config;

    assert_eq!(config.max_concurrent_positions, 0);
    assert_eq!(config.max_initial_sum, Decimal::ZERO);
    assert_eq!(config.entry_after_start_min_secs, 30);
    assert_eq!(config.entry_after_start_max_secs, 240);
    assert_eq!(config.pm_quote_max_stale_secs, 10);
    assert_eq!(config.entry_quote_persistence_secs, 8);
    assert_eq!(config.strong_obi_window_bonus_secs, 60);
    assert_eq!(config.allowed_window_durations, vec![300]);
    assert_eq!(config.protective_recovery_window_secs, 0);
    assert_eq!(config.max_trades_per_event, 0);
    assert_eq!(config.force_complete_threshold, dec!(1.06));
    assert_eq!(config.protective_close_threshold, dec!(1.06));
    assert_eq!(config.obi_decay_exit_ratio, 0.35);
    assert_eq!(config.obi_flip_exit_threshold, 0.008);
    assert_eq!(config.min_entry_sum, dec!(0.30));
    assert_eq!(config.max_entry_sigma, 0.0);
}

#[test]
fn test_strong_obi_bonus_adjusts_entry_thresholds() {
    let config = StaggeredArbBacktestConfig::default();
    assert!(config.strong_obi_entry_bonus_active(true, 0.02, Some(0.01), dec!(1.02), Some(0.03)));
    assert!((config.direction_threshold_now(dec!(1.02), true) - 0.06).abs() < 1e-9);
    assert_eq!(config.max_leg1_price_now(true), dec!(0.58));
    assert_eq!(config.entry_after_start_max_secs_now(900, true), 300);
}

#[test]
fn test_summary_empty() {
    let adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let summary = adapter.build_summary();
    assert!(summary.contains("trades=0"));
    assert!(summary.contains("open=0"));
}

#[test]
fn test_summary_includes_per_symbol_gate_breakdown() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    adapter.config.backtest_config.symbols = vec!["BTCUSDT".into(), "ETHUSDT".into()];
    adapter.bump_entry_reject_for_symbol("BTCUSDT", "obi_stale");
    adapter.bump_leg2_skip_for_symbol("ETHUSDT", "missing_pm_quotes");

    let summary = adapter.build_summary();

    assert!(summary.contains("entry_signal_by_symbol=BTCUSDT:[obi_stale:1]"));
    assert!(summary.contains("leg2_by_symbol=ETHUSDT:[missing_pm_quotes:1]"));
}

#[test]
fn test_live_leg1_submit_sets_client_order_and_idempotency_key() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
    let now = Utc::now();
    adapter.config.backtest_config.direction_threshold = 0.0;
    adapter.config.backtest_config.use_greeks = false;
    adapter.config.backtest_config.max_initial_sum = Decimal::ZERO;
    adapter.config.backtest_config.entry_after_start_min_secs = 0;
    adapter.config.backtest_config.entry_after_start_max_secs = 0;
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.03));
    adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
    seed_persistent_pm_quotes(
        &mut adapter,
        "evt-live-order",
        Some(dec!(0.55)),
        Some(dec!(0.48)),
        now - chrono::Duration::seconds(10),
        now,
    );

    let window = LiveWindow {
        event_id: "evt-live-order".into(),
        symbol: "BTCUSDT".into(),
        up_token: "up-live".into(),
        down_token: "down-live".into(),
        condition_id: None,
        end_time: now + chrono::Duration::seconds(260),
        open_price: Some(dec!(100)),
        window_secs: 300,
    };

    let action = adapter
        .try_entry_for_window(
            "BTCUSDT",
            now,
            &window,
            dec!(101),
            (Some(0.01), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.48)),
        )
        .expect("entry should be accepted");

    match action {
        StrategyAction::SubmitIntent { intent } => {
            let order = crate::domain::order_request_from_strategy_intent(&intent);
            assert_eq!(order.client_order_id, intent.client_order_id);
            assert_eq!(
                order.idempotency_key.as_deref(),
                Some(intent.client_order_id.as_str())
            );
            assert_eq!(intent.market_slug, "evt-live-order");
        }
        other => panic!("expected submit intent action, got {:?}", other),
    }
}

#[tokio::test]
async fn test_required_feeds() {
    let adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let feeds = adapter.required_feeds();
    assert_eq!(feeds.len(), 3);
    // Should have BinanceSpot, PolymarketEvents, Tick
    assert!(matches!(&feeds[0], DataFeed::BinanceSpot { .. }));
    match &feeds[1] {
        DataFeed::PolymarketEvents { series_ids } => {
            assert_eq!(series_ids, &default_staggered_series_ids());
        }
        _ => panic!("expected polymarket events feed"),
    }
    assert!(matches!(&feeds[2], DataFeed::Tick { .. }));
}

#[test]
fn test_leg2_does_not_merge_when_fee_adjusted_pnl_is_negative() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();

    // Raw sum is 0.99 (< 1.00) but fee-adjusted total > 1.00.
    adapter
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.49))));
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.50),
        leg1_shares: 10,
        leg1_fee: dec!(0.075),
        leg1_time: now - chrono::Duration::seconds(10),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(60),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let actions = adapter.check_leg2_opportunities("BTCUSDT", now);
    assert!(
        actions.is_empty(),
        "fee-adjusted negative trade should not auto-merge"
    );
}

#[test]
fn test_try_entry_uses_event_scoped_quotes() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.direction_threshold = 0.0;
    adapter.config.backtest_config.use_greeks = false;
    adapter.config.backtest_config.max_entry_sigma = 0.20;
    adapter.config.backtest_config.entry_after_start_min_secs = 0;
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.02));
    adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
    adapter.active_windows.insert(
        "BTCUSDT".into(),
        vec![
            LiveWindow {
                event_id: "evt-a".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up-a".into(),
                down_token: "down-a".into(),
                condition_id: None,
                end_time: now + chrono::Duration::seconds(280),
                open_price: Some(dec!(100)),
                window_secs: 300,
            },
            LiveWindow {
                event_id: "evt-b".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up-b".into(),
                down_token: "down-b".into(),
                condition_id: None,
                end_time: now + chrono::Duration::seconds(280),
                open_price: Some(dec!(100)),
                window_secs: 300,
            },
        ],
    );
    seed_persistent_pm_quotes(
        &mut adapter,
        "evt-a",
        Some(dec!(0.55)),
        Some(dec!(0.30)),
        now - chrono::Duration::seconds(10),
        now,
    );

    let actions = adapter.try_entry("BTCUSDT", now);

    assert_eq!(actions.len(), 1, "only the quoted event should be tradable");
    assert_eq!(adapter.positions.len(), 1);
    assert_eq!(adapter.positions[0].event_id, "evt-a");
}

#[test]
fn test_try_entry_waits_for_post_open_delay_then_allows() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    let later = now + chrono::Duration::seconds(25);
    adapter.config.backtest_config.direction_threshold = 0.0;
    adapter.config.backtest_config.use_greeks = false;
    adapter.config.backtest_config.max_entry_sigma = 0.20;
    adapter.config.backtest_config.entry_after_start_min_secs = 30;
    adapter.config.backtest_config.entry_after_start_max_secs = 0;
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, later));
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.02));
    adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), later);
    seed_persistent_pm_quotes(
        &mut adapter,
        "evt-delayed",
        Some(dec!(0.55)),
        Some(dec!(0.48)),
        now - chrono::Duration::seconds(10),
        now,
    );

    let window = LiveWindow {
        event_id: "evt-delayed".into(),
        symbol: "BTCUSDT".into(),
        up_token: "up-delayed".into(),
        down_token: "down-delayed".into(),
        condition_id: None,
        end_time: now + chrono::Duration::seconds(290),
        open_price: Some(dec!(100)),
        window_secs: 300,
    };

    let too_early_action = adapter.try_entry_for_window(
        "BTCUSDT",
        now,
        &window,
        dec!(101),
        (Some(0.01), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.48)),
    );
    assert!(
        too_early_action.is_none(),
        "entry should be blocked during the initial observation delay before the post-open entry window begins"
    );

    seed_persistent_pm_quotes(
        &mut adapter,
        "evt-delayed",
        Some(dec!(0.55)),
        Some(dec!(0.48)),
        later - chrono::Duration::seconds(10),
        later,
    );

    let delayed_action = adapter.try_entry_for_window(
        "BTCUSDT",
        later,
        &window,
        dec!(101),
        (Some(0.01), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.48)),
    );
    assert!(
        delayed_action.is_some(),
        "entry should be allowed once the configured post-open delay has elapsed"
    );
}

#[test]
fn test_try_entry_allows_high_sum_when_max_initial_sum_is_disabled() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.direction_threshold = 0.0;
    adapter.config.backtest_config.use_greeks = false;
    adapter.config.backtest_config.max_initial_sum = Decimal::ZERO;
    adapter.config.backtest_config.max_leg1_price = dec!(0.60);
    adapter.config.backtest_config.entry_after_start_min_secs = 0;
    adapter.config.backtest_config.entry_after_start_max_secs = 0;
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(103), None, now));
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.03));
    adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
    seed_persistent_pm_quotes(
        &mut adapter,
        "evt-premium",
        Some(dec!(0.58)),
        Some(dec!(0.50)),
        now - chrono::Duration::seconds(10),
        now,
    );

    let window = LiveWindow {
        event_id: "evt-premium".into(),
        symbol: "BTCUSDT".into(),
        up_token: "up-premium".into(),
        down_token: "down-premium".into(),
        condition_id: None,
        end_time: now + chrono::Duration::seconds(260),
        open_price: Some(dec!(100)),
        window_secs: 300,
    };

    let action = adapter.try_entry_for_window(
        "BTCUSDT",
        now,
        &window,
        dec!(103),
        (Some(0.001), 100.0),
        Some(dec!(0.58)),
        Some(dec!(0.50)),
    );

    assert!(
        action.is_some(),
        "entry should be allowed to rely on OBI/direction when max_initial_sum is explicitly disabled"
    );
}

#[test]
fn test_try_entry_does_not_cap_concurrency_when_max_concurrent_is_zero() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.direction_threshold = 0.0;
    adapter.config.backtest_config.use_greeks = false;
    adapter.config.backtest_config.max_concurrent_positions = 0;
    adapter.config.backtest_config.max_trades_per_event = 0;
    adapter.config.backtest_config.entry_after_start_min_secs = 0;
    adapter.config.backtest_config.entry_after_start_max_secs = 0;
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.03));
    adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
    seed_persistent_pm_quotes(
        &mut adapter,
        "evt-new",
        Some(dec!(0.55)),
        Some(dec!(0.48)),
        now - chrono::Duration::seconds(10),
        now,
    );
    adapter.positions.push(PaperPosition {
        symbol: "ETHUSDT".into(),
        event_id: "evt-existing".into(),
        condition_id: None,
        up_token: "up-existing".into(),
        down_token: "down-existing".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.51),
        leg1_shares: 20,
        leg1_fee: dec!(0.153),
        leg1_time: now - chrono::Duration::seconds(20),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let window = LiveWindow {
        event_id: "evt-new".into(),
        symbol: "BTCUSDT".into(),
        up_token: "up-new".into(),
        down_token: "down-new".into(),
        condition_id: None,
        end_time: now + chrono::Duration::seconds(260),
        open_price: Some(dec!(100)),
        window_secs: 300,
    };

    let action = adapter.try_entry_for_window(
        "BTCUSDT",
        now,
        &window,
        dec!(101),
        (Some(0.01), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.48)),
    );

    assert!(
        action.is_some(),
        "max_concurrent=0 should disable the concurrency cap instead of blocking every new entry"
    );
}

#[test]
fn test_try_entry_rejects_sigma_above_max_entry_sigma() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.direction_threshold = 0.0;
    adapter.config.backtest_config.use_greeks = false;
    adapter.config.backtest_config.max_initial_sum = dec!(1.20);
    adapter.config.backtest_config.max_entry_sigma = 0.01;
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.02));
    adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);

    let window = LiveWindow {
        event_id: "evt-open".into(),
        symbol: "BTCUSDT".into(),
        up_token: "up-open".into(),
        down_token: "down-open".into(),
        condition_id: None,
        end_time: now + chrono::Duration::seconds(280),
        open_price: Some(dec!(100)),
        window_secs: 300,
    };

    let action = adapter.try_entry_for_window(
        "BTCUSDT",
        now,
        &window,
        dec!(101),
        (Some(0.02), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.45)),
    );

    assert!(
        action.is_none(),
        "entry should be blocked when realized sigma exceeds the configured regime cap"
    );
}

#[tokio::test]
async fn test_on_tick_retries_entry_during_opening_window_without_new_quote_callback() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.direction_threshold = 0.0;
    adapter.config.backtest_config.use_greeks = false;
    adapter.config.backtest_config.entry_after_start_min_secs = 30;
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.02));
    adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
    adapter.active_windows.insert(
        "BTCUSDT".into(),
        vec![LiveWindow {
            event_id: "evt-open".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up-open".into(),
            down_token: "down-open".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(260),
            open_price: Some(dec!(100)),
            window_secs: 300,
        }],
    );
    seed_persistent_pm_quotes(
        &mut adapter,
        "evt-open",
        Some(dec!(0.55)),
        Some(dec!(0.45)),
        now - chrono::Duration::seconds(10),
        now,
    );

    let actions = adapter.on_tick(now).await.unwrap();

    assert_eq!(
        adapter.positions.len(),
        1,
        "tick-driven recheck should open leg1 inside the configured opening window"
    );
    assert!(
        actions.iter().any(|action| matches!(
            action,
            StrategyAction::LogEvent { event }
                if matches!(event.event_type, StrategyEventType::EntryTriggered)
        )),
        "tick-driven recheck should emit an EntryTriggered event when it opens leg1"
    );
}

#[test]
fn test_try_entry_requires_persistent_other_ask_before_leg1() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    let later = now + chrono::Duration::seconds(9);
    adapter.config.backtest_config.direction_threshold = 0.0;
    adapter.config.backtest_config.use_greeks = false;
    adapter.config.backtest_config.entry_after_start_min_secs = 0;
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, later));
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.02));
    adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), later);

    let window = LiveWindow {
        event_id: "evt-persist".into(),
        symbol: "BTCUSDT".into(),
        up_token: "up-persist".into(),
        down_token: "down-persist".into(),
        condition_id: None,
        end_time: later + chrono::Duration::seconds(280),
        open_price: Some(dec!(100)),
        window_secs: 300,
    };

    adapter.record_pm_quote("evt-persist", Direction::Up, Some(dec!(0.55)), None, now);
    adapter.record_pm_quote("evt-persist", Direction::Down, Some(dec!(0.45)), None, now);
    let early_action = adapter.try_entry_for_window(
        "BTCUSDT",
        now,
        &window,
        dec!(101),
        (Some(0.001), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.45)),
    );
    assert!(
        early_action.is_none(),
        "entry should wait until the opposite-side ask has persisted for the configured duration"
    );

    adapter.record_pm_quote("evt-persist", Direction::Up, Some(dec!(0.55)), None, later);
    adapter.record_pm_quote(
        "evt-persist",
        Direction::Down,
        Some(dec!(0.45)),
        None,
        later,
    );
    let delayed_action = adapter.try_entry_for_window(
        "BTCUSDT",
        later,
        &window,
        dec!(101),
        (Some(0.001), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.45)),
    );
    assert!(
        delayed_action.is_some(),
        "entry should proceed once the opposite-side ask has stayed visible long enough"
    );
}

#[test]
fn test_record_pm_quote_resets_persistence_after_stale_gap() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let first_seen_at = Utc::now();
    let reappeared_at = first_seen_at + chrono::Duration::seconds(20);

    adapter.record_pm_quote(
        "evt-persist",
        Direction::Down,
        Some(dec!(0.45)),
        None,
        first_seen_at,
    );
    adapter.record_pm_quote(
        "evt-persist",
        Direction::Down,
        Some(dec!(0.45)),
        None,
        reappeared_at,
    );

    let state = adapter
        .pm_quote_state_by_event
        .get("evt-persist")
        .copied()
        .expect("quote state should exist");
    assert_eq!(
        state.down.first_seen_at,
        Some(reappeared_at),
        "a quote that reappears after a stale gap must restart persistence timing"
    );
    assert!(
        !adapter
            .config
            .backtest_config
            .entry_quote_is_persistent(state.down.first_seen_at, reappeared_at),
        "reappearing quotes should not immediately satisfy the persistence gate"
    );
}

#[test]
fn test_min_balance_blocks_entry() {
    let toml = r#"
[entry]
symbols = ["BTCUSDT"]
initial_capital = 10.0
shares_per_trade = 5
direction_threshold = 0.0

[risk]
min_balance_usd = 9.0
"#;

    let mut adapter = StaggeredArbAdapter::from_toml("test".into(), toml, true).unwrap();
    let now = Utc::now();
    let window = LiveWindow {
        event_id: "evt".into(),
        symbol: "BTCUSDT".into(),
        up_token: "up-token".into(),
        down_token: "down-token".into(),
        condition_id: None,
        end_time: now + chrono::Duration::seconds(300),
        open_price: Some(dec!(100)),
        window_secs: 300,
    };

    let action = adapter.try_entry_for_window(
        "BTCUSDT",
        now,
        &window,
        dec!(200),
        (Some(0.01), 100.0),
        Some(dec!(0.60)),
        Some(dec!(0.30)),
    );

    assert!(
        action.is_none(),
        "entry should be blocked when reserve balance would be violated"
    );
    assert!(
        adapter.positions.is_empty(),
        "no leg1 position should be opened when min_balance_usd blocks entry"
    );
}

#[test]
fn test_force_threshold_not_triggered_without_timeout_or_risk() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();

    // Sum crosses threshold, but no timeout/time-safety/stop-loss condition is true.
    adapter
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.55))));
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.50),
        leg1_shares: 10,
        leg1_fee: dec!(0.075),
        leg1_time: now - chrono::Duration::seconds(10),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let actions = adapter.check_leg2_opportunities("BTCUSDT", now);
    assert!(
        actions.is_empty(),
        "force_complete_threshold should not trigger without timeout/time-safety/risk"
    );
}

#[test]
fn test_force_threshold_blocks_forced_timeout_above_cap() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.force_complete_threshold = Decimal::ONE;
    adapter
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.75)), Some(dec!(0.27))));
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.75),
        leg1_shares: 10,
        leg1_fee: dec!(0.1125),
        leg1_time: now - chrono::Duration::seconds(30),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now - chrono::Duration::seconds(1),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

    assert!(
        actions.is_empty(),
        "forced timeout should be blocked when sum exceeds force_complete_threshold"
    );
}

#[test]
fn test_stop_loss_uses_protective_close_threshold() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.force_complete_threshold = Decimal::ONE;
    adapter.config.backtest_config.protective_close_threshold = dec!(1.03);
    adapter.config.backtest_config.max_leg1_loss = dec!(0.05);
    adapter
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.48))));
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_fee: dec!(0.0825),
        leg1_time: now - chrono::Duration::seconds(30),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let _actions = adapter.check_leg2_opportunities("BTCUSDT", now);

    assert_eq!(adapter.closed_trades.len(), 1);
    assert_eq!(adapter.closed_trades[0].exit_reason, "protective_stop_loss");
}

#[test]
fn test_dynamic_protective_threshold_blocks_early_expensive_stop_loss() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.max_leg1_loss = dec!(0.05);
    adapter.config.backtest_config.protective_close_threshold = dec!(1.08);
    adapter
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.52))));
    adapter.active_windows.insert(
        "BTCUSDT".into(),
        vec![LiveWindow {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up".into(),
            down_token: "down".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(300),
            open_price: Some(dec!(100)),
            window_secs: 300,
        }],
    );
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_fee: dec!(0.0825),
        leg1_time: now - chrono::Duration::seconds(10),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

    assert!(actions.is_empty());
    assert_eq!(adapter.closed_trades.len(), 0);
}

#[test]
fn test_supportive_obi_skips_protective_stop_loss() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.max_leg1_loss = dec!(0.05);
    adapter
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.53))));
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(100.6), None, now));
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.01));
    adapter.active_windows.insert(
        "BTCUSDT".into(),
        vec![LiveWindow {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up".into(),
            down_token: "down".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(300),
            open_price: Some(dec!(100)),
            window_secs: 300,
        }],
    );
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_fee: dec!(0.0825),
        leg1_time: now - chrono::Duration::seconds(10),
        entry_obi: Some(0.02),
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

    assert!(actions.is_empty());
    assert_eq!(adapter.closed_trades.len(), 0);
}

#[test]
fn test_protective_stop_arms_then_waits_before_closing() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.use_greeks = false;
    adapter.config.backtest_config.max_leg1_loss = dec!(0.05);
    adapter
        .config
        .backtest_config
        .protective_recovery_window_secs = 12;
    adapter.config.backtest_config.protective_close_threshold = dec!(1.08);
    adapter
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.47))));
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.005));
    adapter.active_windows.insert(
        "BTCUSDT".into(),
        vec![LiveWindow {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up".into(),
            down_token: "down".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(300),
            open_price: Some(dec!(100)),
            window_secs: 300,
        }],
    );
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_fee: dec!(0.0825),
        leg1_time: now - chrono::Duration::seconds(10),
        entry_obi: Some(0.02),
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

    assert!(actions.is_empty());
    assert_eq!(adapter.closed_trades.len(), 0);
    assert_eq!(adapter.positions[0].protective_stop_armed_at, Some(now));

    let actions = adapter.check_leg2_opportunities("BTCUSDT", now + chrono::Duration::seconds(13));

    assert_eq!(actions.len(), 1);
    assert_eq!(adapter.closed_trades.len(), 1);
    assert_eq!(adapter.closed_trades[0].exit_reason, "protective_stop_loss");
}

#[test]
fn test_hard_obi_flip_bypasses_protective_recovery_window() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.use_greeks = false;
    adapter.config.backtest_config.max_leg1_loss = dec!(0.05);
    adapter
        .config
        .backtest_config
        .protective_recovery_window_secs = 12;
    adapter.config.backtest_config.protective_close_threshold = dec!(1.08);
    adapter
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.47))));
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(-0.02));
    adapter.active_windows.insert(
        "BTCUSDT".into(),
        vec![LiveWindow {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up".into(),
            down_token: "down".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(300),
            open_price: Some(dec!(100)),
            window_secs: 300,
        }],
    );
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_fee: dec!(0.0825),
        leg1_time: now - chrono::Duration::seconds(10),
        entry_obi: Some(0.02),
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

    assert_eq!(actions.len(), 1);
    assert_eq!(adapter.closed_trades.len(), 1);
    assert_eq!(adapter.closed_trades[0].exit_reason, "protective_stop_loss");
}

#[test]
fn test_dynamic_force_threshold_allows_late_close_within_cap() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.force_complete_threshold = dec!(1.08);
    adapter
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.75)), Some(dec!(0.32))));
    adapter.active_windows.insert(
        "BTCUSDT".into(),
        vec![LiveWindow {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up".into(),
            down_token: "down".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(20),
            open_price: Some(dec!(100)),
            window_secs: 300,
        }],
    );
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.75),
        leg1_shares: 10,
        leg1_fee: dec!(0.1125),
        leg1_time: now - chrono::Duration::seconds(30),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now - chrono::Duration::seconds(1),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let _actions = adapter.check_leg2_opportunities("BTCUSDT", now);

    assert_eq!(adapter.closed_trades.len(), 1);
    assert_eq!(adapter.closed_trades[0].exit_reason, "forced_timeout");
}

#[test]
fn test_theta_urgency_uses_protective_close_threshold() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.force_complete_threshold = Decimal::ONE;
    adapter.config.backtest_config.protective_close_threshold = dec!(1.02);
    adapter.config.backtest_config.max_theta_cost = 1e-12;
    adapter.config.backtest_config.use_greeks = true;
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(100.2), None, now));
    adapter.active_windows.insert(
        "BTCUSDT".into(),
        vec![LiveWindow {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up".into(),
            down_token: "down".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(20),
            open_price: Some(dec!(100)),
            window_secs: 300,
        }],
    );
    adapter
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.47))));
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_fee: dec!(0.0825),
        leg1_time: now - chrono::Duration::seconds(30),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let _actions = adapter.check_leg2_opportunities("BTCUSDT", now);

    assert_eq!(adapter.closed_trades.len(), 1);
    assert_eq!(adapter.closed_trades[0].exit_reason, "protective_theta");
}

#[test]
fn test_try_entry_rejects_far_from_mid_fair_value_for_long_gamma_profile() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.direction_threshold = 0.0;
    adapter.config.backtest_config.max_fair_value_distance = 0.20;
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.02));
    adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);

    let window = LiveWindow {
        event_id: "evt".into(),
        symbol: "BTCUSDT".into(),
        up_token: "up".into(),
        down_token: "down".into(),
        condition_id: None,
        end_time: now + chrono::Duration::seconds(250),
        open_price: Some(dec!(100)),
        window_secs: 300,
    };

    let action = adapter.try_entry_for_window(
        "BTCUSDT",
        now,
        &window,
        dec!(101),
        (Some(0.01), 200.0),
        Some(dec!(0.55)),
        Some(dec!(0.42)),
    );

    assert!(
        action.is_none(),
        "entry should be rejected when fair value is too far from mid and the long-gamma band is enabled"
    );
}

#[test]
fn test_try_entry_requires_stronger_obi_for_premium_sum() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.direction_threshold = 0.0;
    adapter.config.backtest_config.premium_sum_direction_slope = 0.0;
    adapter.config.backtest_config.use_greeks = false;
    adapter.config.backtest_config.entry_after_start_min_secs = 0;
    adapter
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.01));
    adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);

    let window = LiveWindow {
        event_id: "evt-premium".into(),
        symbol: "BTCUSDT".into(),
        up_token: "up-premium".into(),
        down_token: "down-premium".into(),
        condition_id: None,
        end_time: now + chrono::Duration::seconds(280),
        open_price: Some(dec!(100)),
        window_secs: 300,
    };

    let action = adapter.try_entry_for_window(
        "BTCUSDT",
        now,
        &window,
        dec!(100.04),
        (Some(0.001), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.48)),
    );

    assert!(
        action.is_none(),
        "premium-sum entries should require stronger OBI confirmation than base entries"
    );
    assert_eq!(
        adapter
            .entry_reject_counts
            .get("obi_not_confirmed_for_premium_entry")
            .copied()
            .unwrap_or(0),
        1
    );
}

#[test]
fn test_live_greeks_can_accelerate_leg2_close_before_merge_target() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.merge_target_sum = dec!(0.90);
    adapter.config.backtest_config.min_profit_target = dec!(0.12);
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
    adapter.active_windows.insert(
        "BTCUSDT".into(),
        vec![LiveWindow {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up".into(),
            down_token: "down".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(250),
            open_price: Some(dec!(100)),
            window_secs: 300,
        }],
    );
    adapter
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.44))));
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_fee: dec!(0.0825),
        leg1_time: now - chrono::Duration::seconds(20),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

    assert!(
        actions
            .iter()
            .any(|a| matches!(a, StrategyAction::LogEvent { .. })),
        "high-gamma state should allow live leg2 completion before the normal merge target is hit"
    );
    assert_eq!(adapter.closed_trades.len(), 1);
}

#[tokio::test]
async fn test_event_expired_settles_single_leg_position() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
    let now = Utc::now();
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
    adapter.active_windows.insert(
        "BTCUSDT".into(),
        vec![LiveWindow {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up".into(),
            down_token: "down".into(),
            condition_id: None,
            end_time: now,
            open_price: Some(dec!(100)),
            window_secs: 300,
        }],
    );
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_fee: dec!(0.0825),
        leg1_time: now - chrono::Duration::seconds(60),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now - chrono::Duration::seconds(1),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let actions = adapter
        .on_market_update(&MarketUpdate::EventExpired {
            event_id: "evt".into(),
        })
        .await
        .unwrap();

    assert!(
        actions
            .iter()
            .any(|a| matches!(a, StrategyAction::LogEvent { .. })),
        "settlement should emit a cycle completion log"
    );
    assert_eq!(adapter.closed_trades.len(), 1);
    assert_eq!(adapter.positions[0].state, PaperPositionState::Settled);
    assert_eq!(adapter.closed_trades[0].payout, dec!(10));
}

#[tokio::test]
async fn test_event_expired_settles_partial_leg2_without_double_close() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
    let now = Utc::now();
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(99), None, now));
    adapter.active_windows.insert(
        "BTCUSDT".into(),
        vec![LiveWindow {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up".into(),
            down_token: "down".into(),
            condition_id: None,
            end_time: now,
            open_price: Some(dec!(100)),
            window_secs: 300,
        }],
    );
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_fee: dec!(0.0825),
        leg1_time: now - chrono::Duration::seconds(60),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now - chrono::Duration::seconds(1),
        leg2_price: Some(dec!(0.40)),
        leg2_shares: Some(4),
        leg2_fee: Some(dec!(0.024)),
        leg2_time: Some(now - chrono::Duration::seconds(5)),
        state: PaperPositionState::Leg1Filled,
    });

    let client_id = "cid-expiry-leg2".to_string();
    let mut track = sample_leg2_track(now - chrono::Duration::seconds(10), 6, 0);
    track.event_id = "evt".to_string();
    track.symbol = "BTCUSDT".to_string();
    adapter.live_orders.insert(client_id.clone(), track);
    adapter.pending_leg2_positions.insert(0);

    let _actions = adapter
        .on_market_update(&MarketUpdate::EventExpired {
            event_id: "evt".into(),
        })
        .await
        .unwrap();

    assert_eq!(adapter.closed_trades.len(), 1);
    assert_eq!(adapter.positions[0].state, PaperPositionState::Settled);
    assert_eq!(adapter.closed_trades[0].payout, dec!(4));
    assert!(
        !adapter.pending_leg2_positions.contains(&0),
        "expiry settlement should clear pending leg2 markers for the event"
    );
    assert!(
        !adapter.live_orders.contains_key(&client_id),
        "expiry settlement should retire outstanding leg2 tracking for the event"
    );

    let late_update = OrderUpdate {
        order_id: "0xleg2fill".to_string(),
        client_order_id: Some(client_id),
        status: OrderStatus::Filled,
        filled_qty: 6,
        avg_fill_price: Some(dec!(0.39)),
        timestamp: now + chrono::Duration::seconds(1),
        error: None,
    };
    let late_actions = adapter.on_order_update(&late_update).await.unwrap();

    assert!(late_actions.is_empty());
    assert_eq!(
        adapter.closed_trades.len(),
        1,
        "late leg2 updates after settlement must not close the same cycle twice"
    );
}

#[test]
fn test_live_leg2_uses_position_tokens_even_without_active_window() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
    let now = Utc::now();

    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up-token".into(),
        down_token: "down-token".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.40),
        leg1_shares: 5,
        leg1_fee: dec!(0.03),
        leg1_time: now - chrono::Duration::seconds(10),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(30),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let action = adapter.fill_leg2(0, dec!(0.62), "forced_timeout", now);
    assert!(
        matches!(action, Some(StrategyAction::SubmitIntent { .. })),
        "live leg2 should still submit even if active window already expired"
    );
}

#[test]
fn test_live_fill_leg2_skips_residual_below_venue_minimum() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
    let now = Utc::now();

    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up-token".into(),
        down_token: "down-token".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.40),
        leg1_shares: 20,
        leg1_fee: dec!(0.12),
        leg1_time: now - chrono::Duration::seconds(60),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(30),
        leg2_price: Some(dec!(0.63)),
        leg2_shares: Some(19),
        leg2_fee: Some(dec!(0.17955)),
        leg2_time: Some(now - chrono::Duration::seconds(5)),
        state: PaperPositionState::Leg1Filled,
    });

    let action = adapter.fill_leg2(0, dec!(0.63), "forced_timeout", now);

    assert!(
        action.is_none(),
        "live leg2 should not submit venue-invalid residual orders"
    );
    assert!(adapter.live_orders.is_empty());
    assert!(!adapter.pending_leg2_positions.contains(&0));
    assert_eq!(adapter.positions[0].leg2_shares, Some(19));
}

#[test]
fn test_final_window_high_confidence_still_forces_leg2() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
    let now = Utc::now();
    adapter.config.backtest_config.use_greeks = false;
    adapter.config.backtest_config.min_leg2_delay_secs = 0;
    adapter.config.backtest_config.min_time_remaining_secs = 0;
    adapter
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(101.2), None, now));
    adapter.active_windows.insert(
        "BTCUSDT".into(),
        vec![LiveWindow {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up".into(),
            down_token: "down".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(10),
            open_price: Some(dec!(100)),
            window_secs: 300,
        }],
    );
    adapter
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.40))));
    adapter.positions.push(PaperPosition {
        symbol: "BTCUSDT".into(),
        event_id: "evt".into(),
        condition_id: None,
        up_token: "up".into(),
        down_token: "down".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_fee: dec!(0.0825),
        leg1_time: now - chrono::Duration::seconds(30),
        entry_obi: Some(0.02),
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(30),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

    assert!(
        actions
            .iter()
            .any(|a| matches!(a, StrategyAction::LogEvent { .. })),
        "final-window positions should close through leg2 instead of intentionally holding a single leg"
    );
    assert_eq!(adapter.closed_trades.len(), 1);
    assert_eq!(adapter.closed_trades[0].exit_reason, "forced_final_window");
}

#[tokio::test]
async fn test_leg1_cancelled_with_partial_fill_creates_position() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
    let now = Utc::now();
    let client_id = "cid-leg1".to_string();
    adapter
        .live_orders
        .insert(client_id.clone(), sample_leg1_track(now));
    adapter.pending_leg1_events.insert("evt-1".to_string());

    let update = OrderUpdate {
        order_id: "0xabc".to_string(),
        client_order_id: Some(client_id.clone()),
        status: OrderStatus::Cancelled,
        filled_qty: 7,
        avg_fill_price: Some(dec!(0.52)),
        timestamp: now,
        error: None,
    };

    let _actions = adapter.on_order_update(&update).await.unwrap();

    assert!(
        !adapter.pending_leg1_events.contains("evt-1"),
        "partial-cancelled leg1 should clear event pending lock"
    );
    assert!(
        !adapter.live_orders.contains_key(&client_id),
        "partial-cancelled leg1 should be removed from live order tracking"
    );
    assert_eq!(
        adapter.positions.len(),
        1,
        "leg1 partial fill should open position"
    );
    let pos = &adapter.positions[0];
    assert_eq!(pos.leg1_shares, 7);
    assert_eq!(pos.leg1_price, dec!(0.52));
    assert_eq!(pos.state, PaperPositionState::Leg1Filled);
}

#[tokio::test]
async fn test_leg1_partially_filled_updates_position_immediately_and_requests_cancel() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
    let now = Utc::now();
    let client_id = "cid-leg1-partial".to_string();
    let mut track = sample_leg1_track(now);
    track.cancel_requested_at = None;
    adapter.live_orders.insert(client_id.clone(), track);
    adapter.pending_leg1_events.insert("evt-1".to_string());

    let update = OrderUpdate {
        order_id: "0xabc".to_string(),
        client_order_id: Some(client_id.clone()),
        status: OrderStatus::PartiallyFilled,
        filled_qty: 7,
        avg_fill_price: Some(dec!(0.52)),
        timestamp: now,
        error: None,
    };

    let actions = adapter.on_order_update(&update).await.unwrap();

    assert_eq!(
        adapter.positions.len(),
        1,
        "partial fill should create the live leg1 position immediately"
    );
    assert_eq!(adapter.positions[0].leg1_shares, 7);
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, StrategyAction::CancelOrder { .. })),
        "once we accept a partial leg1 as the actual position size, the remaining order should be cancelled promptly"
    );
    assert!(
        adapter.live_orders.contains_key(&client_id),
        "the live order track should remain until the exchange confirms terminal cleanup"
    );
}

#[tokio::test]
async fn test_leg1_cancel_ack_without_fill_details_waits_for_poll_update() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
    let now = Utc::now();
    let client_id = "cid-leg1".to_string();
    adapter
        .live_orders
        .insert(client_id.clone(), sample_leg1_track(now));
    adapter.pending_leg1_events.insert("evt-1".to_string());

    let update = OrderUpdate {
        order_id: "0xabc".to_string(),
        client_order_id: None, // cancel ack path
        status: OrderStatus::Cancelled,
        filled_qty: 0,
        avg_fill_price: None,
        timestamp: now,
        error: None,
    };

    let _actions = adapter.on_order_update(&update).await.unwrap();

    assert!(
        adapter.pending_leg1_events.contains("evt-1"),
        "synthetic cancel ack should not clear pending lock before reconciliation"
    );
    assert!(
        adapter.live_orders.contains_key(&client_id),
        "synthetic cancel ack should keep live order for poll reconciliation"
    );
    assert!(
        adapter.positions.is_empty(),
        "no fill details => no position should be created yet"
    );
}

#[tokio::test]
async fn test_leg2_partial_cancel_tracks_progress_and_only_resubmits_remaining() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
    let now = Utc::now();
    adapter.positions.push(PaperPosition {
        symbol: "ETHUSDT".into(),
        event_id: "evt-1".into(),
        condition_id: Some("cond-1".into()),
        up_token: "up-token".into(),
        down_token: "down-token".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.62),
        leg1_shares: 20,
        leg1_fee: dec!(0.186),
        leg1_time: now - chrono::Duration::seconds(20),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let client_id = "cid-leg2".to_string();
    adapter
        .live_orders
        .insert(client_id.clone(), sample_leg2_track(now, 20, 0));
    adapter.pending_leg2_positions.insert(0);

    let update = OrderUpdate {
        order_id: "0xleg2".to_string(),
        client_order_id: Some(client_id.clone()),
        status: OrderStatus::Cancelled,
        filled_qty: 7,
        avg_fill_price: Some(dec!(0.38)),
        timestamp: now,
        error: None,
    };

    let _actions = adapter.on_order_update(&update).await.unwrap();
    assert!(
        !adapter.pending_leg2_positions.contains(&0),
        "leg2 partial cancel should clear in-flight marker so remaining shares can retry"
    );
    assert!(
        !adapter.live_orders.contains_key(&client_id),
        "leg2 partial cancel should remove completed attempt from tracking"
    );
    let pos = &adapter.positions[0];
    assert_eq!(pos.leg2_shares, Some(7));
    assert_eq!(pos.state, PaperPositionState::Leg1Filled);

    let action = adapter.fill_leg2(0, dec!(0.40), "merge", now);
    match action {
        Some(StrategyAction::SubmitIntent { intent }) => {
            assert_eq!(intent.shares, 13, "should only submit remaining shares")
        }
        _ => panic!("expected leg2 submit action"),
    }
}

#[tokio::test]
async fn test_leg2_partially_filled_updates_progress_before_terminal_status() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
    let now = Utc::now();
    adapter.positions.push(PaperPosition {
        symbol: "ETHUSDT".into(),
        event_id: "evt-1".into(),
        condition_id: Some("cond-1".into()),
        up_token: "up-token".into(),
        down_token: "down-token".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.62),
        leg1_shares: 20,
        leg1_fee: dec!(0.186),
        leg1_time: now - chrono::Duration::seconds(20),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        leg2_price: None,
        leg2_shares: None,
        leg2_fee: None,
        leg2_time: None,
        state: PaperPositionState::Leg1Filled,
    });

    let client_id = "cid-leg2-partial".to_string();
    adapter
        .live_orders
        .insert(client_id.clone(), sample_leg2_track(now, 20, 0));
    adapter.pending_leg2_positions.insert(0);

    let update = OrderUpdate {
        order_id: "0xleg2".to_string(),
        client_order_id: Some(client_id.clone()),
        status: OrderStatus::PartiallyFilled,
        filled_qty: 7,
        avg_fill_price: Some(dec!(0.38)),
        timestamp: now,
        error: None,
    };

    let _actions = adapter.on_order_update(&update).await.unwrap();

    assert_eq!(
        adapter.positions[0].leg2_shares,
        Some(7),
        "leg2 partial progress should be recorded immediately instead of waiting for cancel/failed terminal callbacks"
    );
    assert!(
        adapter.live_orders.contains_key(&client_id),
        "leg2 live order should stay tracked while the exchange order is still active"
    );
    assert!(
        adapter.pending_leg2_positions.contains(&0),
        "leg2 should remain marked in-flight until the terminal update arrives"
    );
}

#[tokio::test]
async fn test_orphan_leg1_cleanup_keeps_lock_and_allows_late_reconciliation() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
    let now = Utc::now();
    let client_id = "cid-orphan-leg1".to_string();
    let mut track = sample_leg1_track(now - chrono::Duration::seconds(100));
    track.cancel_requested_at = Some(now - chrono::Duration::seconds(70));
    adapter.live_orders.insert(client_id.clone(), track);
    adapter.pending_leg1_events.insert("evt-1".to_string());

    let _actions = adapter.on_tick(now).await.unwrap();

    assert!(
        !adapter.live_orders.contains_key(&client_id),
        "hard cleanup should move the stale order out of active tracking"
    );
    assert!(
        adapter.archived_live_orders.contains_key(&client_id),
        "stale order should stay archived for later reconciliation"
    );
    assert!(
        adapter.pending_leg1_events.contains("evt-1"),
        "same-event lock must remain until reconciliation or expiry"
    );

    let update = OrderUpdate {
        order_id: "0xabc".to_string(),
        client_order_id: Some(client_id.clone()),
        status: OrderStatus::Filled,
        filled_qty: 7,
        avg_fill_price: Some(dec!(0.52)),
        timestamp: now,
        error: None,
    };

    let _actions = adapter.on_order_update(&update).await.unwrap();

    assert_eq!(
        adapter.positions.len(),
        1,
        "late fill should still reconcile into a real position"
    );
    assert_eq!(adapter.positions[0].leg1_shares, 7);
    assert!(
        !adapter.pending_leg1_events.contains("evt-1"),
        "late reconciliation should finally release the event lock"
    );
    assert!(
        !adapter.archived_live_orders.contains_key(&client_id),
        "terminal reconciliation should retire the archived track"
    );
}

#[tokio::test]
async fn test_leg2_partial_then_full_fill_closes_once_with_weighted_price() {
    let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
    let now = Utc::now();
    adapter.positions.push(PaperPosition {
        symbol: "ETHUSDT".into(),
        event_id: "evt-1".into(),
        condition_id: Some("cond-1".into()),
        up_token: "up-token".into(),
        down_token: "down-token".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.62),
        leg1_shares: 20,
        leg1_fee: dec!(0.186),
        leg1_time: now - chrono::Duration::seconds(20),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        leg2_price: Some(dec!(0.38)),
        leg2_shares: Some(7),
        leg2_fee: Some(dec!(0.0399)),
        leg2_time: Some(now - chrono::Duration::seconds(10)),
        state: PaperPositionState::Leg1Filled,
    });

    let client_id = "cid-leg2-fill".to_string();
    adapter
        .live_orders
        .insert(client_id.clone(), sample_leg2_track(now, 13, 0));
    adapter.pending_leg2_positions.insert(0);

    let update = OrderUpdate {
        order_id: "0xleg2fill".to_string(),
        client_order_id: Some(client_id.clone()),
        status: OrderStatus::Filled,
        filled_qty: 13,
        avg_fill_price: Some(dec!(0.39)),
        timestamp: now,
        error: None,
    };

    let _actions = adapter.on_order_update(&update).await.unwrap();

    let pos = &adapter.positions[0];
    assert_eq!(
        pos.state,
        PaperPositionState::Merged,
        "position should close when cumulative leg2 shares reach leg1 size"
    );
    assert_eq!(pos.leg2_shares, Some(20));
    assert_eq!(adapter.closed_trades.len(), 1, "should only close once");
    let trade = &adapter.closed_trades[0];
    assert_eq!(trade.payout, dec!(20));
    assert_eq!(trade.leg2_price, dec!(0.3865));
}
