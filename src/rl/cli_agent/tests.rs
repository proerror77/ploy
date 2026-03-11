use super::{RLCryptoAgent, RLCryptoAgentConfig};
use crate::rl::{CryptoEvent, DomainEvent, ExecutionReport, ExecutionStatus, QuoteData};
use crate::{AgentStatus, Domain};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn make_crypto_event(
    symbol: &str,
    spot: Decimal,
    up_ask: Decimal,
    down_ask: Decimal,
) -> CryptoEvent {
    CryptoEvent {
        symbol: symbol.to_string(),
        spot_price: spot,
        round_slug: None,
        quotes: Some(QuoteData {
            up_bid: up_ask - dec!(0.01),
            up_ask,
            down_bid: down_ask - dec!(0.01),
            down_ask,
            timestamp: Utc::now(),
        }),
        momentum: Some([0.002, 0.001, 0.0005, 0.0001]),
    }
}

#[tokio::test]
async fn test_rl_agent_creation() {
    let agent = RLCryptoAgent::with_defaults();
    assert_eq!(agent.id(), "rl-crypto-agent-1");
    assert_eq!(agent.status(), AgentStatus::Initializing);
    assert_eq!(agent.domain(), Domain::Crypto);
}

#[tokio::test]
async fn test_rl_agent_lifecycle() {
    let mut agent = RLCryptoAgent::with_defaults();

    agent.start().await.unwrap();
    assert_eq!(agent.status(), AgentStatus::Running);
    assert!(agent.status().can_trade());

    agent.pause();
    assert_eq!(agent.status(), AgentStatus::Paused);
    assert!(!agent.status().can_trade());

    agent.resume();
    assert_eq!(agent.status(), AgentStatus::Running);

    agent.stop().await.unwrap();
    assert_eq!(agent.status(), AgentStatus::Stopped);
}

#[tokio::test]
async fn test_rl_signal_on_good_sum() {
    let config = RLCryptoAgentConfig {
        coins: vec!["BTC".to_string()],
        exploration_rate: 0.0,
        ..Default::default()
    };
    let mut agent = RLCryptoAgent::new(config);
    agent.start().await.unwrap();

    let event = make_crypto_event("BTCUSDT", dec!(50000), dec!(0.47), dec!(0.48));
    let domain_event = DomainEvent::Crypto(event);

    let intents = agent.on_event(domain_event).await.unwrap();

    assert!(!intents.is_empty(), "Should generate intent on good sum");
    assert!(intents[0].is_buy);
    assert_eq!(intents[0].domain, Domain::Crypto);
}

#[tokio::test]
async fn test_rl_no_signal_on_high_sum() {
    let config = RLCryptoAgentConfig {
        coins: vec!["BTC".to_string()],
        exploration_rate: 0.0,
        ..Default::default()
    };
    let mut agent = RLCryptoAgent::new(config);
    agent.start().await.unwrap();

    let event = make_crypto_event("BTCUSDT", dec!(50000), dec!(0.50), dec!(0.50));
    let domain_event = DomainEvent::Crypto(event);

    let intents = agent.on_event(domain_event).await.unwrap();

    assert!(intents.is_empty());
}

#[tokio::test]
async fn test_exploration_decay() {
    let mut config = RLCryptoAgentConfig::default();
    config.exploration_rate = 0.5;
    config.rl_config.training.exploration_decay = 0.9;
    config.rl_config.training.exploration_min = 0.01;

    let mut agent = RLCryptoAgent::new(config);

    assert_eq!(agent.exploration_rate, 0.5);

    for _ in 0..10 {
        agent.decay_exploration();
    }

    assert!(agent.exploration_rate < 0.5);
    assert!(agent.exploration_rate >= 0.01);
}

#[tokio::test]
async fn test_position_tracking() {
    let config = RLCryptoAgentConfig {
        up_token_id: "up-token".to_string(),
        down_token_id: "down-token".to_string(),
        ..Default::default()
    };
    let mut agent = RLCryptoAgent::new(config);
    agent.start().await.unwrap();

    assert_eq!(agent.position_count(), 0);
    assert_eq!(agent.total_exposure(), Decimal::ZERO);

    let report = ExecutionReport {
        intent_id: uuid::Uuid::new_v4(),
        agent_id: agent.id().to_string(),
        order_id: Some("order-1".to_string()),
        status: ExecutionStatus::Filled,
        filled_shares: 100,
        avg_fill_price: Some(dec!(0.50)),
        fees: Decimal::ZERO,
        error_message: None,
        executed_at: Utc::now(),
        latency_ms: 50,
    };

    agent.on_execution(report).await;

    assert_eq!(agent.position_count(), 1);
    assert!(agent.total_exposure() > Decimal::ZERO);
}

#[tokio::test]
async fn test_submitted_execution_does_not_pause_agent() {
    let mut agent = RLCryptoAgent::with_defaults();
    agent.start().await.unwrap();

    for _ in 0..3 {
        agent
            .on_execution(ExecutionReport {
                intent_id: uuid::Uuid::new_v4(),
                agent_id: agent.id().to_string(),
                order_id: Some("intent-123".to_string()),
                status: ExecutionStatus::Submitted,
                filled_shares: 0,
                avg_fill_price: None,
                fees: Decimal::ZERO,
                error_message: None,
                executed_at: Utc::now(),
                latency_ms: 5,
            })
            .await;
    }

    assert_eq!(agent.status(), AgentStatus::Running);
}
