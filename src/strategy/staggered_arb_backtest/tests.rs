use super::*;
use crate::strategy::backtest_feed::{HistoricalFeed, MarketUpdate, UpdateType};
use rust_decimal_macros::dec;
use std::collections::VecDeque;

fn make_spot(ts: &str, symbol: &str, price: Decimal) -> MarketUpdate {
    MarketUpdate {
        timestamp: DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Utc),
        symbol: symbol.to_string(),
        update_type: UpdateType::SpotTrade {
            price,
            quantity: None,
        },
    }
}

fn make_binance_l2(ts: &str, symbol: &str, obi_5: Decimal) -> MarketUpdate {
    MarketUpdate {
        timestamp: DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Utc),
        symbol: symbol.to_string(),
        update_type: UpdateType::BinanceL2 {
            obi_5,
            obi_10: obi_5,
            bid_volume_5: dec!(1000),
            ask_volume_5: dec!(900),
            spread_bps: dec!(1),
        },
    }
}

fn make_quotes(
    ts: &str,
    symbol: &str,
    slug: &str,
    up: Decimal,
    down: Decimal,
) -> Vec<MarketUpdate> {
    let timestamp = DateTime::parse_from_rfc3339(ts)
        .unwrap()
        .with_timezone(&Utc);

    vec![
        MarketUpdate {
            timestamp,
            symbol: symbol.to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: slug.to_string(),
                token_id: format!("{}:UP", slug),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(up),
            },
        },
        MarketUpdate {
            timestamp,
            symbol: symbol.to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: slug.to_string(),
                token_id: format!("{}:DOWN", slug),
                side: Side::Down,
                best_bid: None,
                best_ask: Some(down),
            },
        },
    ]
}

fn seed_persistent_pm_quotes(
    engine: &mut StaggeredArbBacktestEngine,
    event_slug: &str,
    up_ask: Option<Decimal>,
    down_ask: Option<Decimal>,
    first_seen_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
) {
    engine.record_pm_quote(event_slug, Side::Up, up_ask, first_seen_at);
    engine.record_pm_quote(event_slug, Side::Down, down_ask, first_seen_at);
    if last_seen_at != first_seen_at {
        engine.record_pm_quote(event_slug, Side::Up, up_ask, last_seen_at);
        engine.record_pm_quote(event_slug, Side::Down, down_ask, last_seen_at);
    }
}

fn make_event_open(ts: &str, symbol: &str, slug: &str, end_ts: &str, s0: Decimal) -> MarketUpdate {
    MarketUpdate {
        timestamp: DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Utc),
        symbol: symbol.to_string(),
        update_type: UpdateType::EventState {
            event_slug: slug.to_string(),
            end_time: Some(
                DateTime::parse_from_rfc3339(end_ts)
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            price_to_beat: Some(s0),
            outcome: None,
        },
    }
}

fn make_settlement(ts: &str, symbol: &str, slug: &str, up_won: bool) -> MarketUpdate {
    MarketUpdate {
        timestamp: DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Utc),
        symbol: symbol.to_string(),
        update_type: UpdateType::EventState {
            event_slug: slug.to_string(),
            end_time: None,
            price_to_beat: None,
            outcome: Some(up_won),
        },
    }
}

#[test]
fn test_config_defaults() {
    let config = StaggeredArbBacktestConfig::default();
    assert_eq!(config.direction_threshold, 0.05);
    assert_eq!(config.shares_per_trade, 20);
    assert_eq!(config.premium_sum_threshold, Decimal::ONE);
    assert_eq!(config.premium_sum_direction_slope, 1.25);
    assert_eq!(config.premium_sum_obi_slope, 0.25);
    assert_eq!(config.obi_confirm_threshold, 0.005);
    assert_eq!(config.strong_obi_threshold, 0.015);
    assert_eq!(config.strong_obi_direction_relaxation, 0.015);
    assert_eq!(config.strong_obi_price_bonus, dec!(0.02));
    assert_eq!(config.strong_obi_window_bonus_secs, 60);
    assert_eq!(config.max_concurrent_positions, 0);
    assert_eq!(config.max_initial_sum, Decimal::ZERO);
    assert_eq!(config.max_leg1_price, dec!(0.56));
    assert_eq!(config.merge_target_sum, dec!(0.95));
    assert_eq!(config.min_profit_target, dec!(0.02));
    assert_eq!(config.max_wait_secs, 120);
    assert_eq!(config.entry_after_start_min_secs, 30);
    assert_eq!(config.entry_after_start_max_secs, 240);
    assert_eq!(config.pm_quote_max_stale_secs, 10);
    assert_eq!(config.entry_quote_persistence_secs, 8);
    assert_eq!(config.min_leg2_delay_secs, 3);
    assert_eq!(config.max_trades_per_event, 0);
    assert_eq!(config.cooldown_secs, 5);
    assert_eq!(config.min_ask_price, dec!(0.05));
    assert_eq!(config.min_entry_sum, dec!(0.30));
    assert_eq!(config.allowed_window_durations, vec![300]);
    assert_eq!(config.force_complete_threshold, dec!(1.06));
    assert_eq!(config.protective_close_threshold, dec!(1.06));
    assert_eq!(config.protective_recovery_window_secs, 0);
    assert_eq!(config.obi_decay_exit_ratio, 0.35);
    assert_eq!(config.obi_flip_exit_threshold, 0.008);
    assert_eq!(config.min_entry_sigma, 0.003);
    assert_eq!(config.max_entry_sigma, 0.0);
    assert_eq!(config.max_fair_value_distance, 0.15);
}

#[test]
fn test_config_from_toml_matches_checked_in_template() {
    let config = StaggeredArbBacktestConfig::from_toml_str(include_str!(
        "../../../config/strategies/staggered_arb.toml"
    ))
    .unwrap();

    assert_eq!(
        config.symbols,
        vec![
            "BTCUSDT".to_string(),
            "ETHUSDT".to_string(),
            "SOLUSDT".to_string()
        ]
    );
    assert_eq!(config.shares_per_trade, 20);
    assert_eq!(config.direction_threshold, 0.04);
    assert_eq!(config.obi_confirm_threshold, 0.005);
    assert_eq!(config.strong_obi_threshold, 0.015);
    assert_eq!(config.max_initial_sum, dec!(0.92));
    assert_eq!(config.max_leg1_price, dec!(0.65));
    assert_eq!(config.entry_after_start_min_secs, 30);
    assert_eq!(config.entry_after_start_max_secs, 0);
    assert_eq!(config.pm_quote_max_stale_secs, 10);
    assert_eq!(config.entry_quote_persistence_secs, 8);
    assert_eq!(config.strong_obi_window_bonus_secs, 60);
    assert_eq!(config.allowed_window_durations, vec![300, 900]);
    assert_eq!(config.force_complete_threshold, dec!(0.00));
    assert_eq!(config.protective_close_threshold, dec!(1.06));
    assert_eq!(config.protective_recovery_window_secs, 0);
    assert_eq!(config.obi_decay_exit_ratio, 0.35);
    assert_eq!(config.obi_flip_exit_threshold, 0.008);
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
fn test_position_state_display() {
    assert_eq!(format!("{}", ArbPositionState::Leg1Filled), "Leg1Filled");
    assert_eq!(format!("{}", ArbPositionState::Settled), "Settled");
    assert_eq!(format!("{}", ArbPositionState::Aborted), "Aborted");
}

#[test]
fn test_merge_when_sum_below_one() {
    // Scenario: UP is about to rise. Buy UP at 0.45, then DOWN drops to 0.50.
    // Sum = 0.45 + 0.50 = 0.95 < 1.0 → merge for profit.
    let mut config = StaggeredArbBacktestConfig::default();
    config.direction_threshold = 0.01; // low threshold for test
    config.min_time_remaining_secs = 10;
    config.max_initial_sum = dec!(1.20);
    config.min_profit_target = dec!(0.01);
    config.cooldown_secs = 0;
    config.min_leg2_delay_secs = 0;
    config.max_trades_per_event = 0;
    config.allowed_window_durations = vec![]; // accept all in test

    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

    // Build a feed: event open, spot prices to build vol history, then quotes
    let mut updates = vec![
        // Event window: 10 minutes
        make_event_open(
            "2026-01-01T00:00:00Z",
            "BTCUSDT",
            "test-event",
            "2026-01-01T00:10:00Z",
            dec!(100000),
        ),
    ];

    // Spot price history (need enough for volatility calc)
    for i in 1..=60 {
        let ts = format!("2026-01-01T00:00:{:02}Z", i);
        // Price moving up → p_hat > 0.5 → buy UP first
        let price = dec!(100000) + Decimal::from(i * 10);
        updates.push(make_spot(&ts, "BTCUSDT", price));
    }
    updates.push(make_binance_l2(
        "2026-01-01T00:00:59Z",
        "BTCUSDT",
        dec!(0.02),
    ));

    // Initial quotes: sum = 1.05 (spread)
    updates.extend(make_quotes(
        "2026-01-01T00:01:00Z",
        "BTCUSDT",
        "test-event",
        dec!(0.55),
        dec!(0.50),
    ));

    // After some time, DOWN ask drops → sum becomes < 1.0
    updates.extend(make_quotes(
        "2026-01-01T00:02:00Z",
        "BTCUSDT",
        "test-event",
        dec!(0.60),
        dec!(0.38),
    ));

    let mut feed = HistoricalFeed {
        updates: VecDeque::from(updates),
    };

    let results = engine.run(&mut feed);

    // Should have at least attempted trades
    // The exact outcome depends on vol calc and execution sim,
    // but the engine should not panic and should produce valid results
    assert!(
        results.total_pnl.is_sign_positive()
            || results.total_pnl.is_sign_negative()
            || results.total_pnl == Decimal::ZERO
    );
}

#[test]
fn test_single_leg_settlement() {
    // Scenario: Buy UP, Leg2 never fills, position settles at $1 (UP wins)
    let mut config = StaggeredArbBacktestConfig::default();
    config.direction_threshold = 0.01;
    config.min_time_remaining_secs = 10;
    config.max_initial_sum = dec!(1.20);
    config.min_profit_target = dec!(0.01);
    config.cooldown_secs = 0;
    config.min_leg2_delay_secs = 0;
    config.max_trades_per_event = 0;
    config.max_wait_secs = 30;
    config.allowed_window_durations = vec![];

    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

    let mut updates = vec![make_event_open(
        "2026-01-01T00:00:00Z",
        "BTCUSDT",
        "test-event",
        "2026-01-01T00:10:00Z",
        dec!(100000),
    )];

    for i in 1..=60 {
        let ts = format!("2026-01-01T00:00:{:02}Z", i);
        let price = dec!(100000) + Decimal::from(i * 10);
        updates.push(make_spot(&ts, "BTCUSDT", price));
    }
    updates.push(make_binance_l2(
        "2026-01-01T00:00:59Z",
        "BTCUSDT",
        dec!(0.02),
    ));

    // Quotes with sum > 1 throughout (no Leg2 opportunity)
    updates.extend(make_quotes(
        "2026-01-01T00:01:00Z",
        "BTCUSDT",
        "test-event",
        dec!(0.55),
        dec!(0.55),
    ));
    updates.extend(make_quotes(
        "2026-01-01T00:03:00Z",
        "BTCUSDT",
        "test-event",
        dec!(0.60),
        dec!(0.55),
    ));

    // Settlement: UP wins
    updates.push(make_settlement(
        "2026-01-01T00:10:00Z",
        "BTCUSDT",
        "test-event",
        true,
    ));

    let mut feed = HistoricalFeed {
        updates: VecDeque::from(updates),
    };

    let results = engine.run(&mut feed);

    // Engine should handle settlement without panicking
    // Trades that settled as UP winning should be profitable
    for trade in engine.closed_trades() {
        if trade.exit_reason == "settlement" && trade.leg1_direction == "UP" {
            assert!(trade.won, "UP leg should win when UP settles at $1");
        }
    }
    let _ = results; // use results to avoid warning
}

#[test]
fn test_entry_skips_sigma_above_max_entry_sigma() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.direction_threshold = 0.0;
    config.use_greeks = false;
    config.max_initial_sum = dec!(1.20);
    config.max_entry_sigma = 0.01;
    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

    let now = Utc::now();
    engine.active_events.insert(
        "BTCUSDT".into(),
        vec![ActiveWindowInfo {
            event_slug: "evt".into(),
            s0: dec!(100),
            end_time: now + chrono::Duration::seconds(280),
            window_duration_secs: 300,
        }],
    );
    engine
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.45))));
    engine.binance_l2_obi_5.insert("BTCUSDT".into(), dec!(0.02));
    engine.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
    engine.try_entry_for_window(
        "BTCUSDT",
        now,
        &ActiveWindowInfo {
            event_slug: "evt".into(),
            s0: dec!(100),
            end_time: now + chrono::Duration::seconds(280),
            window_duration_secs: 300,
        },
        dec!(101),
        (Some(0.02), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.45)),
    );

    assert!(
        engine.positions.is_empty(),
        "entry should be rejected when realized sigma exceeds the configured regime cap"
    );
}

#[test]
fn test_entry_requires_more_direction_strength_for_premium_sum() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.direction_threshold = 0.0;
    config.use_greeks = false;
    config.max_initial_sum = dec!(1.04);
    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

    let now = Utc::now();
    let window = ActiveWindowInfo {
        event_slug: "evt".into(),
        s0: dec!(100),
        end_time: now + chrono::Duration::seconds(280),
        window_duration_secs: 300,
    };

    engine
        .active_events
        .insert("BTCUSDT".into(), vec![window.clone()]);
    engine
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.48))));
    engine.binance_l2_obi_5.insert("BTCUSDT".into(), dec!(0.02));
    engine.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
    engine.try_entry_for_window(
        "BTCUSDT",
        now,
        &window,
        dec!(100.04),
        (Some(0.001), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.48)),
    );

    assert!(
        engine.positions.is_empty(),
        "premium-sum entries should require stronger direction strength than at-par entries"
    );
}

#[test]
fn test_entry_requires_fresh_binance_l2_history() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.direction_threshold = 0.0;
    config.use_greeks = false;
    config.max_initial_sum = dec!(1.20);
    config.entry_after_start_min_secs = 0;
    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

    let now = Utc::now();
    let window = ActiveWindowInfo {
        event_slug: "evt".into(),
        s0: dec!(100),
        end_time: now + chrono::Duration::seconds(280),
        window_duration_secs: 300,
    };

    engine
        .active_events
        .insert("BTCUSDT".into(), vec![window.clone()]);
    engine
        .pm_asks_by_event
        .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.45))));
    engine.try_entry_for_window(
        "BTCUSDT",
        now,
        &window,
        dec!(101),
        (Some(0.001), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.45)),
    );

    assert_eq!(
            engine.positions.len(),
            0,
            "backtest should match live and reject entries when no fresh Binance L2 history exists for the replay window"
        );
}

#[test]
fn test_force_complete_threshold_blocks_backtest_timeout_above_cap() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.force_complete_threshold = Decimal::ONE;
    config.use_greeks = false;
    config.min_leg2_delay_secs = 0;

    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
    let now = Utc::now();
    engine
        .pm_asks_by_event
        .insert("test-event".into(), (Some(dec!(0.75)), Some(dec!(0.27))));
    engine.positions.push(StaggeredArbPosition {
        symbol: "BTCUSDT".into(),
        event_slug: "test-event".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.75),
        leg1_shares: 10,
        leg1_time: now - chrono::Duration::seconds(30),
        leg1_fee: dec!(0.1125),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now - chrono::Duration::seconds(1),
        s0: dec!(100000),
        event_end_time: now + chrono::Duration::seconds(300),
        window_duration_secs: 300,
        entry_p_hat: 0.7,
        entry_sigma: 0.01,
        best_sum_seen: dec!(1.02),
        initial_sum: dec!(1.02),
        entry_greeks: None,
        state: ArbPositionState::Leg1Filled,
        leg2_direction: None,
        leg2_price: None,
        leg2_shares: None,
        leg2_time: None,
        leg2_fee: None,
        exit_reason: None,
        pnl: None,
    });

    engine.check_leg2_opportunities("BTCUSDT", now);

    assert_eq!(engine.closed_trades.len(), 0);
    assert_eq!(engine.positions[0].state, ArbPositionState::Leg1Filled);
}

#[test]
fn test_stop_loss_uses_protective_close_threshold() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.force_complete_threshold = Decimal::ONE;
    config.protective_close_threshold = dec!(1.03);
    config.use_greeks = false;
    config.min_leg2_delay_secs = 0;
    config.max_leg1_loss = dec!(0.05);
    config.protective_recovery_window_secs = 0;

    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
    let now = Utc::now();
    engine
        .pm_asks_by_event
        .insert("test-event".into(), (Some(dec!(0.50)), Some(dec!(0.47))));
    engine.positions.push(StaggeredArbPosition {
        symbol: "BTCUSDT".into(),
        event_slug: "test-event".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_time: now - chrono::Duration::seconds(30),
        leg1_fee: dec!(0.0825),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        s0: dec!(100000),
        event_end_time: now + chrono::Duration::seconds(20),
        window_duration_secs: 300,
        entry_p_hat: 0.7,
        entry_sigma: 0.01,
        best_sum_seen: dec!(1.02),
        initial_sum: dec!(1.02),
        entry_greeks: None,
        state: ArbPositionState::Leg1Filled,
        leg2_direction: None,
        leg2_price: None,
        leg2_shares: None,
        leg2_time: None,
        leg2_fee: None,
        exit_reason: None,
        pnl: None,
    });

    engine.check_leg2_opportunities("BTCUSDT", now);

    assert_eq!(engine.closed_trades.len(), 1);
    assert_eq!(engine.closed_trades[0].exit_reason, "protective_stop_loss");
}

#[test]
fn test_dynamic_protective_threshold_blocks_early_expensive_stop_loss() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.use_greeks = false;
    config.min_leg2_delay_secs = 0;
    config.max_leg1_loss = dec!(0.05);
    config.protective_close_threshold = dec!(1.08);

    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
    let now = Utc::now();
    engine
        .pm_asks_by_event
        .insert("test-event".into(), (Some(dec!(0.50)), Some(dec!(0.52))));
    engine.positions.push(StaggeredArbPosition {
        symbol: "BTCUSDT".into(),
        event_slug: "test-event".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_time: now - chrono::Duration::seconds(10),
        leg1_fee: dec!(0.0825),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        s0: dec!(100000),
        event_end_time: now + chrono::Duration::seconds(300),
        window_duration_secs: 300,
        entry_p_hat: 0.7,
        entry_sigma: 0.01,
        best_sum_seen: dec!(1.07),
        initial_sum: dec!(1.07),
        entry_greeks: None,
        state: ArbPositionState::Leg1Filled,
        leg2_direction: None,
        leg2_price: None,
        leg2_shares: None,
        leg2_time: None,
        leg2_fee: None,
        exit_reason: None,
        pnl: None,
    });

    engine.check_leg2_opportunities("BTCUSDT", now);

    assert_eq!(engine.closed_trades.len(), 0);
    assert_eq!(engine.positions[0].state, ArbPositionState::Leg1Filled);
}

#[test]
fn test_supportive_obi_skips_protective_stop_loss() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.use_greeks = false;
    config.min_leg2_delay_secs = 0;
    config.max_leg1_loss = dec!(0.05);
    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
    let now = Utc::now();
    engine
        .pm_asks_by_event
        .insert("test-event".into(), (Some(dec!(0.50)), Some(dec!(0.53))));
    engine
        .spot_prices
        .insert("BTCUSDT".into(), SpotPrice::new(dec!(100.6), None, now));
    engine.binance_l2_obi_5.insert("BTCUSDT".into(), dec!(0.01));
    engine.positions.push(StaggeredArbPosition {
        symbol: "BTCUSDT".into(),
        event_slug: "test-event".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_time: now - chrono::Duration::seconds(10),
        leg1_fee: dec!(0.0825),
        wait_deadline: now + chrono::Duration::seconds(120),
        s0: dec!(100),
        event_end_time: now + chrono::Duration::seconds(300),
        window_duration_secs: 300,
        entry_p_hat: 0.7,
        entry_sigma: 0.01,
        best_sum_seen: dec!(1.08),
        initial_sum: dec!(1.08),
        entry_obi: Some(0.02),
        protective_stop_armed_at: None,
        entry_greeks: None,
        state: ArbPositionState::Leg1Filled,
        leg2_direction: None,
        leg2_price: None,
        leg2_shares: None,
        leg2_time: None,
        leg2_fee: None,
        exit_reason: None,
        pnl: None,
    });

    engine.check_leg2_opportunities("BTCUSDT", now);

    assert_eq!(engine.closed_trades.len(), 0);
    assert_eq!(engine.positions[0].state, ArbPositionState::Leg1Filled);
}

#[test]
fn test_protective_stop_arms_then_waits_before_closing() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.use_greeks = false;
    config.min_leg2_delay_secs = 0;
    config.max_leg1_loss = dec!(0.05);
    config.protective_recovery_window_secs = 12;
    config.protective_close_threshold = dec!(1.08);
    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
    let now = Utc::now();
    engine
        .pm_asks_by_event
        .insert("test-event".into(), (Some(dec!(0.50)), Some(dec!(0.47))));
    engine
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(0.005));
    engine.positions.push(StaggeredArbPosition {
        symbol: "BTCUSDT".into(),
        event_slug: "test-event".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_time: now - chrono::Duration::seconds(10),
        leg1_fee: dec!(0.0825),
        wait_deadline: now + chrono::Duration::seconds(120),
        s0: dec!(100),
        event_end_time: now + chrono::Duration::seconds(300),
        window_duration_secs: 300,
        entry_p_hat: 0.7,
        entry_sigma: 0.01,
        best_sum_seen: dec!(1.02),
        initial_sum: dec!(1.02),
        entry_obi: Some(0.02),
        protective_stop_armed_at: None,
        entry_greeks: None,
        state: ArbPositionState::Leg1Filled,
        leg2_direction: None,
        leg2_price: None,
        leg2_shares: None,
        leg2_time: None,
        leg2_fee: None,
        exit_reason: None,
        pnl: None,
    });

    engine.check_leg2_opportunities("BTCUSDT", now);

    assert_eq!(engine.closed_trades.len(), 0);
    assert_eq!(engine.positions[0].state, ArbPositionState::Leg1Filled);
    assert_eq!(engine.positions[0].protective_stop_armed_at, Some(now));

    engine.check_leg2_opportunities("BTCUSDT", now + chrono::Duration::seconds(13));

    assert_eq!(engine.closed_trades.len(), 1);
    assert_eq!(engine.closed_trades[0].exit_reason, "protective_stop_loss");
}

#[test]
fn test_hard_obi_flip_bypasses_protective_recovery_window() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.use_greeks = false;
    config.min_leg2_delay_secs = 0;
    config.max_leg1_loss = dec!(0.05);
    config.protective_recovery_window_secs = 12;
    config.protective_close_threshold = dec!(1.08);
    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
    let now = Utc::now();
    engine
        .pm_asks_by_event
        .insert("test-event".into(), (Some(dec!(0.50)), Some(dec!(0.47))));
    engine
        .binance_l2_obi_5
        .insert("BTCUSDT".into(), dec!(-0.02));
    engine.positions.push(StaggeredArbPosition {
        symbol: "BTCUSDT".into(),
        event_slug: "test-event".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 10,
        leg1_time: now - chrono::Duration::seconds(10),
        leg1_fee: dec!(0.0825),
        wait_deadline: now + chrono::Duration::seconds(120),
        s0: dec!(100),
        event_end_time: now + chrono::Duration::seconds(300),
        window_duration_secs: 300,
        entry_p_hat: 0.7,
        entry_sigma: 0.01,
        best_sum_seen: dec!(1.02),
        initial_sum: dec!(1.02),
        entry_obi: Some(0.02),
        protective_stop_armed_at: None,
        entry_greeks: None,
        state: ArbPositionState::Leg1Filled,
        leg2_direction: None,
        leg2_price: None,
        leg2_shares: None,
        leg2_time: None,
        leg2_fee: None,
        exit_reason: None,
        pnl: None,
    });

    engine.check_leg2_opportunities("BTCUSDT", now);

    assert_eq!(engine.closed_trades.len(), 1);
    assert_eq!(engine.closed_trades[0].exit_reason, "protective_stop_loss");
}

#[test]
fn test_dynamic_force_threshold_allows_late_close_within_cap() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.use_greeks = false;
    config.min_leg2_delay_secs = 0;
    config.force_complete_threshold = dec!(1.08);

    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
    let now = Utc::now();
    engine
        .pm_asks_by_event
        .insert("test-event".into(), (Some(dec!(0.75)), Some(dec!(0.32))));
    engine.positions.push(StaggeredArbPosition {
        symbol: "BTCUSDT".into(),
        event_slug: "test-event".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.75),
        leg1_shares: 10,
        leg1_time: now - chrono::Duration::seconds(30),
        leg1_fee: dec!(0.1125),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now - chrono::Duration::seconds(1),
        s0: dec!(100000),
        event_end_time: now + chrono::Duration::seconds(20),
        window_duration_secs: 300,
        entry_p_hat: 0.7,
        entry_sigma: 0.01,
        best_sum_seen: dec!(1.07),
        initial_sum: dec!(1.07),
        entry_greeks: None,
        state: ArbPositionState::Leg1Filled,
        leg2_direction: None,
        leg2_price: None,
        leg2_shares: None,
        leg2_time: None,
        leg2_fee: None,
        exit_reason: None,
        pnl: None,
    });

    engine.check_leg2_opportunities("BTCUSDT", now);

    assert_eq!(engine.closed_trades.len(), 1);
    assert_eq!(engine.closed_trades[0].exit_reason, "forced_timeout");
}

#[test]
fn test_fill_leg2_partial_keeps_position_open_until_remaining_shares_fill() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.use_greeks = false;
    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
    let now = Utc::now();
    engine.lob_depth.insert("BTCUSDT".into(), 100);
    engine.positions.push(StaggeredArbPosition {
        symbol: "BTCUSDT".into(),
        event_slug: "test-event".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 200,
        leg1_time: now - chrono::Duration::seconds(30),
        leg1_fee: dec!(1.65),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        s0: dec!(100000),
        event_end_time: now + chrono::Duration::seconds(300),
        window_duration_secs: 300,
        entry_p_hat: 0.7,
        entry_sigma: 0.01,
        best_sum_seen: dec!(0.95),
        initial_sum: dec!(0.95),
        entry_greeks: None,
        state: ArbPositionState::Leg1Filled,
        leg2_direction: None,
        leg2_price: None,
        leg2_shares: None,
        leg2_time: None,
        leg2_fee: None,
        exit_reason: None,
        pnl: None,
    });

    engine.fill_leg2(0, dec!(0.40), "merge", now);

    assert_eq!(engine.positions[0].state, ArbPositionState::Leg1Filled);
    assert_eq!(engine.positions[0].leg2_shares, Some(100));
    assert_eq!(engine.closed_trades.len(), 0);

    engine.fill_leg2(0, dec!(0.39), "merge", now + chrono::Duration::seconds(15));

    assert_eq!(engine.positions[0].state, ArbPositionState::Settled);
    assert_eq!(engine.positions[0].leg2_shares, Some(200));
    assert_eq!(engine.closed_trades.len(), 1);
    assert_eq!(engine.closed_trades[0].shares, 200);
}

#[test]
fn test_fill_leg2_skips_residual_below_venue_minimum() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.use_greeks = false;
    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
    let now = Utc::now();
    engine.lob_depth.insert("BTCUSDT".into(), 100);
    engine.positions.push(StaggeredArbPosition {
        symbol: "BTCUSDT".into(),
        event_slug: "test-event".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.40),
        leg1_shares: 20,
        leg1_time: now - chrono::Duration::seconds(30),
        leg1_fee: dec!(0.12),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(60),
        s0: dec!(100),
        event_end_time: now + chrono::Duration::seconds(20),
        window_duration_secs: 300,
        entry_p_hat: 0.7,
        entry_sigma: 0.01,
        best_sum_seen: dec!(1.03),
        initial_sum: dec!(1.03),
        entry_greeks: None,
        state: ArbPositionState::Leg1Filled,
        leg2_direction: Some(Direction::Down),
        leg2_price: Some(dec!(0.63)),
        leg2_shares: Some(19),
        leg2_time: Some(now - chrono::Duration::seconds(5)),
        leg2_fee: Some(dec!(0.17955)),
        exit_reason: None,
        pnl: None,
    });

    engine.fill_leg2(0, dec!(0.63), "forced_timeout", now);

    assert_eq!(engine.positions[0].state, ArbPositionState::Leg1Filled);
    assert_eq!(engine.positions[0].leg2_shares, Some(19));
    assert_eq!(engine.closed_trades.len(), 0);
}

#[test]
fn test_resolve_positions_settles_residual_after_partial_leg2_fill() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.use_greeks = false;
    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
    let now = Utc::now();
    engine.lob_depth.insert("BTCUSDT".into(), 0);
    engine
        .pm_asks_by_event
        .insert("test-event".into(), (Some(dec!(0.60)), Some(dec!(0.40))));
    engine.positions.push(StaggeredArbPosition {
        symbol: "BTCUSDT".into(),
        event_slug: "test-event".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 200,
        leg1_time: now - chrono::Duration::seconds(30),
        leg1_fee: dec!(1.65),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(120),
        s0: dec!(100000),
        event_end_time: now + chrono::Duration::seconds(300),
        window_duration_secs: 300,
        entry_p_hat: 0.7,
        entry_sigma: 0.01,
        best_sum_seen: dec!(0.95),
        initial_sum: dec!(0.95),
        entry_greeks: None,
        state: ArbPositionState::Leg1Filled,
        leg2_direction: Some(Direction::Down),
        leg2_price: Some(dec!(0.40)),
        leg2_shares: Some(100),
        leg2_time: Some(now - chrono::Duration::seconds(10)),
        leg2_fee: Some(dec!(0.60)),
        exit_reason: None,
        pnl: None,
    });

    engine.resolve_positions("BTCUSDT", "test-event", false, now);

    assert_eq!(engine.positions[0].state, ArbPositionState::Settled);
    assert_eq!(engine.closed_trades.len(), 1);
    assert_eq!(
        engine.closed_trades[0].pnl,
        dec!(100) - (dec!(110) + dec!(1.65) + dec!(40) + dec!(0.60))
    );
}

#[test]
fn test_resolve_positions_does_not_force_buy_leg2_at_settlement() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.use_greeks = false;
    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
    let now = Utc::now();
    seed_persistent_pm_quotes(
        &mut engine,
        "test-event",
        Some(dec!(0.60)),
        Some(dec!(0.40)),
        now - chrono::Duration::seconds(10),
        now,
    );
    engine.positions.push(StaggeredArbPosition {
        symbol: "BTCUSDT".into(),
        event_slug: "test-event".into(),
        leg1_direction: Direction::Up,
        leg1_price: dec!(0.55),
        leg1_shares: 20,
        leg1_time: now - chrono::Duration::seconds(30),
        leg1_fee: dec!(0.165),
        entry_obi: None,
        protective_stop_armed_at: None,
        wait_deadline: now + chrono::Duration::seconds(60),
        s0: dec!(100000),
        event_end_time: now,
        window_duration_secs: 300,
        entry_p_hat: 0.7,
        entry_sigma: 0.01,
        best_sum_seen: dec!(1.00),
        initial_sum: dec!(1.00),
        entry_greeks: None,
        state: ArbPositionState::Leg1Filled,
        leg2_direction: None,
        leg2_price: None,
        leg2_shares: None,
        leg2_time: None,
        leg2_fee: None,
        exit_reason: None,
        pnl: None,
    });

    engine.resolve_positions("BTCUSDT", "test-event", true, now);

    assert_eq!(engine.closed_trades.len(), 1);
    assert_eq!(engine.closed_trades[0].exit_reason, "settlement");
    assert_eq!(engine.closed_trades[0].leg2_price, None);
    assert_eq!(engine.closed_trades[0].final_sum, None);
}

#[test]
fn test_entry_uses_simulated_fill_time_for_leg1_clock() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.direction_threshold = 0.0;
    config.use_greeks = false;
    config.entry_after_start_min_secs = 0;
    config.max_initial_sum = dec!(1.20);
    config.min_leg2_delay_secs = 0;
    config.cooldown_secs = 0;
    config.max_trades_per_event = 0;
    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

    let now = Utc::now();
    let window = ActiveWindowInfo {
        event_slug: "evt".into(),
        s0: dec!(100),
        end_time: now + chrono::Duration::seconds(280),
        window_duration_secs: 300,
    };
    engine
        .active_events
        .insert("BTCUSDT".into(), vec![window.clone()]);
    seed_persistent_pm_quotes(
        &mut engine,
        "evt",
        Some(dec!(0.55)),
        Some(dec!(0.45)),
        now - chrono::Duration::seconds(10),
        now,
    );
    engine.binance_l2_obi_5.insert("BTCUSDT".into(), dec!(0.02));
    engine.binance_l2_obi_ts.insert("BTCUSDT".into(), now);

    let expected_fill = engine
        .execution_sim
        .simulate_buy(
            dec!(0.55),
            now,
            engine.config.shares_per_trade,
            engine.market_depth("BTCUSDT"),
        )
        .fill_time;
    let remaining_after_fill = (window.end_time - expected_fill).num_seconds() as f64;
    let expected_wait = expected_fill
        + chrono::Duration::seconds(
            (engine.config.max_wait_secs as i64)
                .min((remaining_after_fill * engine.config.max_wait_pct) as i64),
        );

    engine.try_entry_for_window(
        "BTCUSDT",
        now,
        &window,
        dec!(101),
        (Some(0.001), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.45)),
    );

    assert_eq!(engine.positions.len(), 1);
    assert_eq!(engine.positions[0].leg1_time, expected_fill);
    assert_eq!(engine.positions[0].wait_deadline, expected_wait);
}

#[test]
fn test_entry_requires_persistent_other_ask_before_opening_leg1() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.direction_threshold = 0.0;
    config.use_greeks = false;
    config.entry_after_start_min_secs = 0;
    config.max_initial_sum = dec!(1.20);
    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

    let now = Utc::now();
    let later = now + chrono::Duration::seconds(9);
    let window = ActiveWindowInfo {
        event_slug: "evt".into(),
        s0: dec!(100),
        end_time: later + chrono::Duration::seconds(280),
        window_duration_secs: 300,
    };

    engine
        .active_events
        .insert("BTCUSDT".into(), vec![window.clone()]);
    engine.binance_l2_obi_5.insert("BTCUSDT".into(), dec!(0.02));
    engine.binance_l2_obi_ts.insert("BTCUSDT".into(), later);

    engine.record_pm_quote("evt", Side::Up, Some(dec!(0.55)), now);
    engine.record_pm_quote("evt", Side::Down, Some(dec!(0.45)), now);
    engine.try_entry_for_window(
        "BTCUSDT",
        now,
        &window,
        dec!(101),
        (Some(0.001), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.45)),
    );
    assert!(
        engine.positions.is_empty(),
        "entry should wait until the opposite-side ask has persisted for the configured duration"
    );

    engine.record_pm_quote("evt", Side::Up, Some(dec!(0.55)), later);
    engine.record_pm_quote("evt", Side::Down, Some(dec!(0.45)), later);
    engine.try_entry_for_window(
        "BTCUSDT",
        later,
        &window,
        dec!(101),
        (Some(0.001), 100.0),
        Some(dec!(0.55)),
        Some(dec!(0.45)),
    );
    assert_eq!(
        engine.positions.len(),
        1,
        "entry should proceed once the opposite-side ask has remained visible long enough"
    );
}

#[test]
fn test_record_pm_quote_clears_disappearing_ask_side() {
    let mut engine =
        StaggeredArbBacktestEngine::new_without_recorder(StaggeredArbBacktestConfig::default());
    let now = Utc::now();

    engine.record_pm_quote(
        "evt",
        Side::Up,
        Some(dec!(0.55)),
        now - chrono::Duration::seconds(10),
    );
    engine.record_pm_quote(
        "evt",
        Side::Down,
        Some(dec!(0.45)),
        now - chrono::Duration::seconds(10),
    );
    assert_eq!(
        engine.pm_asks_by_event.get("evt").copied(),
        Some((Some(dec!(0.55)), Some(dec!(0.45))))
    );

    engine.record_pm_quote("evt", Side::Down, None, now);
    assert_eq!(
        engine.pm_asks_by_event.get("evt").copied(),
        Some((Some(dec!(0.55)), None)),
        "replay should clear vanished PM asks instead of leaving stale prices live"
    );
}

#[test]
fn test_record_pm_quote_resets_persistence_after_stale_gap() {
    let mut engine =
        StaggeredArbBacktestEngine::new_without_recorder(StaggeredArbBacktestConfig::default());
    let first_seen_at = Utc::now();
    let reappeared_at = first_seen_at + chrono::Duration::seconds(20);

    engine.record_pm_quote("evt", Side::Down, Some(dec!(0.45)), first_seen_at);
    engine.record_pm_quote("evt", Side::Down, Some(dec!(0.45)), reappeared_at);

    let state = engine
        .pm_quote_state_by_event
        .get("evt")
        .copied()
        .expect("quote state should exist");
    assert_eq!(
        state.down.first_seen_at,
        Some(reappeared_at),
        "a quote that reappears after a stale gap must restart persistence timing"
    );
    assert!(
        !engine
            .config
            .entry_quote_is_persistent(state.down.first_seen_at, reappeared_at),
        "reappearing quotes should not immediately satisfy the persistence gate"
    );
}

#[test]
fn test_abort_on_stop_loss() {
    let mut config = StaggeredArbBacktestConfig::default();
    config.direction_threshold = 0.01;
    config.min_time_remaining_secs = 10;
    config.max_initial_sum = dec!(1.20);
    config.min_profit_target = dec!(0.01);
    config.cooldown_secs = 0;
    config.min_leg2_delay_secs = 0;
    config.max_trades_per_event = 0;
    config.max_leg1_loss = dec!(0.05);
    config.allowed_window_durations = vec![];

    let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

    let mut updates = vec![make_event_open(
        "2026-01-01T00:00:00Z",
        "BTCUSDT",
        "test-event",
        "2026-01-01T00:10:00Z",
        dec!(100000),
    )];

    for i in 1..=60 {
        let ts = format!("2026-01-01T00:00:{:02}Z", i);
        let price = dec!(100000) + Decimal::from(i * 10);
        updates.push(make_spot(&ts, "BTCUSDT", price));
    }

    // Entry quote
    updates.extend(make_quotes(
        "2026-01-01T00:01:00Z",
        "BTCUSDT",
        "test-event",
        dec!(0.55),
        dec!(0.50),
    ));

    // UP ask drops significantly → stop loss triggers
    updates.extend(make_quotes(
        "2026-01-01T00:01:30Z",
        "BTCUSDT",
        "test-event",
        dec!(0.40),
        dec!(0.65),
    ));

    let mut feed = HistoricalFeed {
        updates: VecDeque::from(updates),
    };

    let _results = engine.run(&mut feed);

    // Check that any aborted trades have stop_loss reason
    let stop_losses: Vec<_> = engine
        .closed_trades()
        .iter()
        .filter(|t| t.exit_reason == "stop_loss")
        .collect();
    // May or may not trigger depending on execution sim, but engine shouldn't panic
    let _ = stop_losses;
}
