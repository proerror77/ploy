use super::*;
use chrono::{Duration as ChronoDuration, Utc};
use rust_decimal_macros::dec;

#[test]
fn test_direction_opposite() {
    assert_eq!(Direction::Up.opposite(), Direction::Down);
    assert_eq!(Direction::Down.opposite(), Direction::Up);
}

#[test]
fn test_momentum_signal_valid() {
    let config = MomentumConfig::default();
    let signal = MomentumSignal {
        symbol: "BTCUSDT".into(),
        direction: Direction::Up,
        cex_move_pct: dec!(0.01),
        pm_price: dec!(0.30),
        edge: dec!(0.10),
        confidence: 0.8,
        timestamp: Utc::now(),
    };

    assert!(signal.is_valid(&config));
}

#[test]
fn test_position_pnl() {
    let pos = Position {
        token_id: "test".into(),
        symbol: "BTCUSDT".into(),
        direction: Direction::Up,
        entry_price: dec!(0.50),
        entry_notional: dec!(50),
        shares: 100,
        entry_time: Utc::now(),
        highest_price: dec!(0.50),
        event_end_time: Utc::now() + ChronoDuration::minutes(10),
        event_slug: "test".into(),
        condition_id: "test_condition".into(),
        entry_p_hat: None,
        window_open_price: None,
    };

    assert_eq!(pos.pnl_pct(dec!(0.55)), dec!(0.10));
    assert_eq!(pos.pnl_pct(dec!(0.45)), dec!(-0.10));
}

#[test]
fn test_exit_manager_take_profit() {
    let config = ExitConfig {
        take_profit_pct: dec!(0.20),
        stop_loss_pct: dec!(0.15),
        trailing_stop_pct: dec!(0.10),
        exit_before_resolution_secs: 30,
    };

    let manager = ExitManager::new(config);

    let pos = Position {
        token_id: "test".into(),
        symbol: "BTCUSDT".into(),
        direction: Direction::Up,
        entry_price: dec!(0.50),
        entry_notional: dec!(50),
        shares: 100,
        entry_time: Utc::now(),
        highest_price: dec!(0.50),
        event_end_time: Utc::now() + ChronoDuration::minutes(10),
        event_slug: "test".into(),
        condition_id: "test_condition".into(),
        entry_p_hat: None,
        window_open_price: None,
    };

    let exit = manager.check_exit(&pos, dec!(0.625));
    assert!(matches!(exit, Some(ExitReason::TakeProfit { .. })));
}

#[test]
fn test_exit_manager_stop_loss() {
    let config = ExitConfig::default();
    let manager = ExitManager::new(config);

    let pos = Position {
        token_id: "test".into(),
        symbol: "BTCUSDT".into(),
        direction: Direction::Up,
        entry_price: dec!(0.50),
        entry_notional: dec!(50),
        shares: 100,
        entry_time: Utc::now(),
        highest_price: dec!(0.50),
        event_end_time: Utc::now() + ChronoDuration::minutes(10),
        event_slug: "test".into(),
        condition_id: "test_condition".into(),
        entry_p_hat: None,
        window_open_price: None,
    };

    let exit = manager.check_exit(&pos, dec!(0.40));
    assert!(matches!(exit, Some(ExitReason::StopLoss { .. })));
}

#[test]
fn test_parse_price_from_question() {
    assert_eq!(
        EventInfo::parse_price_from_question("Will BTC be above $94,000 at 9:15 PM?"),
        Some(dec!(94000))
    );
    assert_eq!(
        EventInfo::parse_price_from_question("Will ETH be above $3,500.50 at 10:00 AM?"),
        Some(dec!(3500.50))
    );
    assert_eq!(
        EventInfo::parse_price_from_question("↑ 94,000"),
        Some(dec!(94000))
    );
    assert_eq!(
        EventInfo::parse_price_from_question("↓ 86,000"),
        Some(dec!(86000))
    );
    assert_eq!(
        EventInfo::parse_price_from_question("Will BTC be above $100,000?"),
        Some(dec!(100000))
    );
    assert_eq!(
        EventInfo::parse_price_from_question("Will SOL be above $150.25?"),
        Some(dec!(150.25))
    );
    assert_eq!(
        EventInfo::parse_price_from_question("Will it rain tomorrow?"),
        None
    );
    assert_eq!(EventInfo::parse_price_from_question(""), None);
}

#[test]
fn test_event_matcher_includes_btc_5m_series() {
    let client = PolymarketClient::new("https://clob.polymarket.com", true).unwrap();
    let matcher = EventMatcher::new(client);

    let btc_series = matcher
        .symbol_to_series
        .get("BTCUSDT")
        .expect("BTCUSDT mapping should exist");

    assert!(
        btc_series.iter().any(|id| id == "10684"),
        "BTCUSDT series should include 5m series id 10684"
    );
}

#[tokio::test]
async fn test_find_event_with_timing_prefers_best_across_all_series() {
    let client = PolymarketClient::new("https://clob.polymarket.com", true).unwrap();
    let mut matcher = EventMatcher::new(client);

    matcher
        .symbol_to_series
        .insert("BTCUSDT".into(), vec!["series-a".into(), "series-b".into()]);

    let now = Utc::now();
    let mk_event = |slug: &str, seconds_remaining: i64| EventInfo {
        slug: slug.to_string(),
        title: slug.to_string(),
        up_token_id: format!("{slug}-up"),
        down_token_id: format!("{slug}-down"),
        start_time: now,
        end_time: now + ChronoDuration::seconds(seconds_remaining),
        condition_id: format!("{slug}-condition"),
        series_id: "test".to_string(),
        horizon: "other".to_string(),
        price_to_beat: None,
    };

    {
        let mut events = matcher.active_events.write().await;
        events.insert("series-a".into(), vec![mk_event("event-a", 600)]);
        events.insert("series-b".into(), vec![mk_event("event-b", 120)]);
    }

    let best = matcher
        .find_event_with_timing("BTCUSDT", 60, 900, true)
        .await
        .expect("expected event");

    assert_eq!(best.slug, "event-b");
}
