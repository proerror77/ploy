use super::NbaComebackStrategy;
use super::config_loader::default_nba_comeback_config;
use crate::domain::OrderStatus;
use crate::strategy::nba_comeback::core::ComebackOpportunity;
use crate::strategy::nba_comeback::espn::{GameStatus, LiveGame};
use crate::strategy::traits::{DataFeed, OrderUpdate, Strategy, StrategyAction};
use chrono::Utc;
use rust_decimal_macros::dec;

fn sample_game() -> LiveGame {
    LiveGame {
        espn_game_id: "401584701".to_string(),
        home_team: "Boston Celtics".to_string(),
        away_team: "Los Angeles Lakers".to_string(),
        home_abbrev: "BOS".to_string(),
        away_abbrev: "LAL".to_string(),
        home_score: 90,
        away_score: 80,
        quarter: 3,
        clock: "05:00".to_string(),
        time_remaining_mins: 17.0,
        status: GameStatus::InProgress,
        home_quarter_scores: Vec::new(),
        away_quarter_scores: Vec::new(),
    }
}

fn sample_opportunity() -> ComebackOpportunity {
    ComebackOpportunity {
        game: sample_game(),
        trailing_team: "Los Angeles Lakers".to_string(),
        trailing_abbrev: "LAL".to_string(),
        deficit: 10,
        comeback_rate: 0.20,
        adjusted_win_prob: 0.35,
        market_price: dec!(0.30),
        edge: 0.05,
        market_slug: "nba-lal-vs-bos".to_string(),
        token_id: "lal-win-yes".to_string(),
    }
}

fn strategy_from_toml(toml: &str) -> NbaComebackStrategy {
    NbaComebackStrategy::from_toml("nba-test".to_string(), toml, true).expect("strategy")
}

#[tokio::test]
async fn strategy_from_config_builds_strategy() {
    let strategy = NbaComebackStrategy::from_config(
        "nba-test".to_string(),
        default_nba_comeback_config(),
        true,
        Some("postgres://localhost/unused"),
    )
    .expect("strategy");

    assert_eq!(strategy.name(), "nba_comeback");
    assert_eq!(strategy.id(), "nba-test");
}

#[tokio::test]
async fn from_toml_builds_nba_strategy_and_overrides_config() {
    let toml = r#"
[strategy]
name = "nba_comeback"

[nba_comeback]
min_edge = 0.12
max_entry_price = 0.63
shares = 55
cooldown_secs = 900
max_daily_spend_usd = 125
min_deficit = 4
max_deficit = 12
target_quarter = 3
espn_poll_interval_secs = 45
min_comeback_rate = 0.18
season = "2025-26"
database_url = "postgres://localhost/unused"
"#;

    let strategy = strategy_from_toml(toml);

    assert_eq!(strategy.name(), "nba_comeback");
    assert!(matches!(
        strategy.required_feeds().as_slice(),
        [DataFeed::Tick {
            interval_ms: 45_000
        }]
    ));
    assert_eq!(strategy.core.cfg.min_edge, dec!(0.12));
    assert_eq!(strategy.core.cfg.max_entry_price, dec!(0.63));
    assert_eq!(strategy.core.cfg.shares, 55);
    assert_eq!(strategy.core.cfg.cooldown_secs, 900);
    assert_eq!(strategy.core.cfg.max_daily_spend_usd, dec!(125));
    assert_eq!(strategy.core.cfg.min_deficit, 4);
    assert_eq!(strategy.core.cfg.max_deficit, 12);
    assert_eq!(strategy.core.cfg.target_quarter, 3);
    assert!((strategy.core.cfg.min_comeback_rate - 0.18).abs() < f64::EPSILON);
    assert_eq!(strategy.core.cfg.season, "2025-26");
}

#[test]
fn from_toml_rejects_non_nba_strategy_name() {
    let toml = r#"
[strategy]
name = "event_edge"

[nba_comeback]
database_url = "postgres://localhost/unused"
"#;

    let err = NbaComebackStrategy::from_toml("nba-test".to_string(), toml, true)
        .err()
        .expect("wrong strategy name should fail");
    assert!(err.to_string().contains("nba_comeback"));
}

#[tokio::test]
async fn emits_canonical_submit_order_and_tracks_fill_into_position() {
    let toml = r#"
[strategy]
name = "nba_comeback"

[nba_comeback]
shares = 25
database_url = "postgres://localhost/unused"
"#;
    let mut strategy = strategy_from_toml(toml);
    let opp = sample_opportunity();
    let now = Utc::now();

    let actions = strategy.build_actions_for_opportunity_for_test(&opp, Some("cond-1".into()), now);

    let client_order_id = actions
        .iter()
        .find_map(|action| match action {
            StrategyAction::SubmitIntent { intent } => {
                let order = crate::domain::order_request_from_strategy_intent(&intent);
                assert_eq!(order.client_order_id, intent.client_order_id);
                assert_eq!(
                    order.idempotency_key.as_deref(),
                    Some(intent.client_order_id.as_str())
                );
                Some(intent.client_order_id.clone())
            }
            _ => None,
        })
        .expect("submit order action");

    assert_eq!(strategy.pending_orders.len(), 1);

    strategy
        .on_order_update(&OrderUpdate {
            order_id: "exchange-1".to_string(),
            client_order_id: Some(client_order_id),
            status: OrderStatus::Filled,
            filled_qty: 25,
            avg_fill_price: Some(dec!(0.31)),
            timestamp: now,
            error: None,
        })
        .await
        .expect("fill update");

    assert!(strategy.pending_orders.is_empty());
    let positions = strategy.positions();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].token_id, "lal-win-yes");
    assert_eq!(positions[0].entry_price, dec!(0.31));
    assert_eq!(positions[0].shares, 25);
    assert_eq!(
        positions[0].metadata.get("game_id"),
        Some(&"401584701".to_string())
    );
    assert_eq!(
        positions[0].metadata.get("condition_id"),
        Some(&"cond-1".to_string())
    );
}
