use super::*;
use crate::adapters::PolymarketClient;
use crate::agent_runtime::AgentStatus;
use crate::config::ExecutionConfig;
use crate::coordinator::{QueueStats, QueueStatsSnapshot};
use crate::coordinator::OrderPriority;
use crate::platform::Domain;
use crate::strategy::executor::OrderExecutor;
use rust_decimal_macros::dec;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::time::{timeout, Duration};

fn mock_snapshot(agent_id: &str) -> AgentSnapshot {
    AgentSnapshot {
        agent_id: agent_id.into(),
        name: agent_id.into(),
        domain: Domain::Crypto,
        status: AgentStatus::Running,
        position_count: 1,
        exposure: dec!(100),
        daily_pnl: dec!(5),
        unrealized_pnl: dec!(2),
        metrics: HashMap::new(),
        last_heartbeat: Utc::now(),
        error_message: None,
    }
}

fn make_test_handle() -> (CoordinatorHandle, Coordinator) {
    let client = PolymarketClient::new("https://clob.polymarket.com", true)
        .expect("build dry-run polymarket client");
    let executor = Arc::new(OrderExecutor::new(client, ExecutionConfig::default()));
    let allowed_domains = HashSet::from([Domain::Crypto, Domain::Sports]);
    let coordinator = Coordinator::new(
        CoordinatorConfig::default(),
        executor,
        "acct-test".to_string(),
        allowed_domains,
    );
    let handle = coordinator.handle();
    (handle, coordinator)
}

#[test]
fn test_global_state_defaults() {
    let state = GlobalState::new();
    assert_eq!(state.active_agent_count(), 0);
    assert_eq!(state.total_exposure(), Decimal::ZERO);
    assert_eq!(state.total_unrealized_pnl(), Decimal::ZERO);
}

#[test]
fn test_global_state_active_count() {
    let mut state = GlobalState::new();
    state.agents.insert("a".into(), mock_snapshot("a"));
    state.agents.insert("b".into(), {
        let mut s = mock_snapshot("b");
        s.status = AgentStatus::Paused;
        s
    });
    assert_eq!(state.active_agent_count(), 1);
}

#[test]
fn test_queue_stats_snapshot_from() {
    let qs = QueueStats {
        current_size: 5,
        max_size: 100,
        enqueued_total: 50,
        dequeued_total: 45,
        expired_total: 3,
        critical_count: 1,
        high_count: 2,
        normal_count: 1,
        low_count: 1,
    };
    let snap = QueueStatsSnapshot::from(qs);
    assert_eq!(snap.current_size, 5);
    assert_eq!(snap.enqueued_total, 50);
}

fn make_intent(is_buy: bool, priority: OrderPriority) -> OrderIntent {
    let mut intent = OrderIntent::new(
        "crypto_lob_ml",
        Domain::Crypto,
        "btc-updown-5m-123",
        "token-up-123",
        crate::domain::Side::Up,
        is_buy,
        100,
        dec!(0.42),
    );
    intent.priority = priority;
    intent
}

#[test]
fn test_buy_intent_requires_deployment_id_metadata() {
    let intent = make_intent(true, OrderPriority::Normal);
    let reason = buy_intent_missing_deployment_reason(&intent);
    assert_eq!(
        reason.as_deref(),
        Some("BUY intent missing required metadata field 'deployment_id'")
    );
}

#[test]
fn test_sell_intent_does_not_require_deployment_id_metadata() {
    let intent = make_intent(false, OrderPriority::Normal);
    assert!(buy_intent_missing_deployment_reason(&intent).is_none());
}

#[test]
fn test_sell_reduce_only_violation_when_no_tracked_shares() {
    let intent = make_intent(false, OrderPriority::Normal);
    let reason = sell_reduce_only_violation_reason(&intent, 0, 0);
    assert!(reason
        .unwrap_or_default()
        .contains("no tracked open shares"));
}

#[test]
fn test_sell_reduce_only_violation_when_requested_exceeds_tracked() {
    let intent = make_intent(false, OrderPriority::Normal);
    let reason = sell_reduce_only_violation_reason(&intent, 30, 0);
    assert!(reason
        .unwrap_or_default()
        .contains("requested shares 100 exceeds available reduce-only shares 30"));
}

#[test]
fn test_sell_reduce_only_allows_with_sufficient_tracked_shares() {
    let intent = make_intent(false, OrderPriority::Normal);
    assert!(sell_reduce_only_violation_reason(&intent, 100, 0).is_none());
    assert!(sell_reduce_only_violation_reason(&intent, 150, 0).is_none());
}

#[test]
fn test_sell_reduce_only_violation_when_pending_sells_exhaust_available() {
    let intent = make_intent(false, OrderPriority::Normal);
    let reason = sell_reduce_only_violation_reason(&intent, 100, 100);
    assert!(reason
        .unwrap_or_default()
        .contains("fully reserved by pending SELL intents 100"));
}

#[test]
fn test_sell_reduce_only_violation_when_requested_exceeds_available_after_pending() {
    let intent = make_intent(false, OrderPriority::Normal);
    let reason = sell_reduce_only_violation_reason(&intent, 100, 40);
    assert!(reason
        .unwrap_or_default()
        .contains("requested shares 100 exceeds available reduce-only shares 60"));
}

#[tokio::test]
async fn test_drain_and_execute_records_single_success_for_buy_fill() {
    let (_handle, coordinator) = make_test_handle();
    coordinator
        .risk_gate
        .register_agent_with_domain("crypto_lob_ml", Domain::Crypto, AgentRiskParams::default())
        .await;

    let intent =
        make_intent(true, OrderPriority::Normal).with_metadata("deployment_id", "deploy.test");

    coordinator.handle_order_intent(intent).await;
    coordinator.drain_and_execute().await;

    let (total_pnl, success_count, failure_count) = coordinator.risk_gate.daily_stats().await;
    assert_eq!(total_pnl, Decimal::ZERO);
    assert_eq!(success_count, 1);
    assert_eq!(failure_count, 0);
}

#[tokio::test]
async fn test_handle_order_intent_emits_rejected_update_for_missing_deployment() {
    let (_handle, mut coordinator) = make_test_handle();
    let mut order_updates = coordinator.register_order_updates("crypto_lob_ml".to_string());

    let intent = make_intent(true, OrderPriority::Normal);
    let client_order_id = intent.client_order_id.clone();

    coordinator.handle_order_intent(intent).await;

    let update = timeout(Duration::from_secs(1), order_updates.recv())
        .await
        .expect("receive rejected order update")
        .expect("order update available");
    assert_eq!(
        update.client_order_id.as_deref(),
        Some(client_order_id.as_str())
    );
    assert_eq!(update.status, crate::domain::OrderStatus::Rejected);
    assert!(update
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("deployment_id"));
}

#[tokio::test]
async fn test_drain_and_execute_emits_pending_and_fill_updates() {
    let (_handle, mut coordinator) = make_test_handle();
    coordinator
        .risk_gate
        .register_agent_with_domain("crypto_lob_ml", Domain::Crypto, AgentRiskParams::default())
        .await;
    let mut order_updates = coordinator.register_order_updates("crypto_lob_ml".to_string());

    let intent =
        make_intent(true, OrderPriority::Normal).with_metadata("deployment_id", "deploy.test");
    let client_order_id = intent.client_order_id.clone();

    coordinator.handle_order_intent(intent).await;

    let pending = timeout(Duration::from_secs(1), order_updates.recv())
        .await
        .expect("receive pending order update")
        .expect("pending update available");
    assert_eq!(
        pending.client_order_id.as_deref(),
        Some(client_order_id.as_str())
    );
    assert_eq!(pending.status, crate::domain::OrderStatus::Pending);

    coordinator.drain_and_execute().await;

    let executed = timeout(Duration::from_secs(1), order_updates.recv())
        .await
        .expect("receive execution order update")
        .expect("execution update available");
    assert_eq!(
        executed.client_order_id.as_deref(),
        Some(client_order_id.as_str())
    );
    assert!(matches!(
        executed.status,
        crate::domain::OrderStatus::Submitted
            | crate::domain::OrderStatus::PartiallyFilled
            | crate::domain::OrderStatus::Filled
    ));
}

#[tokio::test]
async fn test_drain_and_execute_sell_fill_reduces_position_and_realizes_pnl() {
    let (_handle, coordinator) = make_test_handle();
    coordinator
        .risk_gate
        .register_agent_with_domain("crypto_lob_ml", Domain::Crypto, AgentRiskParams::default())
        .await;

    let buy_intent =
        make_intent(true, OrderPriority::Normal).with_metadata("deployment_id", "deploy.test");
    coordinator.handle_order_intent(buy_intent).await;
    coordinator.drain_and_execute().await;

    let mut sell_intent = make_intent(false, OrderPriority::Normal);
    sell_intent.shares = 60;
    sell_intent.limit_price = dec!(0.60);

    coordinator.handle_order_intent(sell_intent).await;
    coordinator.drain_and_execute().await;

    let remaining_shares = coordinator
        .positions
        .agent_open_shares_for_token_side(
            "crypto_lob_ml",
            Domain::Crypto,
            "token-up-123",
            crate::domain::Side::Up,
        )
        .await;
    assert_eq!(remaining_shares, 40);
    assert_eq!(coordinator.positions.total_realized_pnl().await, dec!(10.8));

    let (total_pnl, success_count, failure_count) = coordinator.risk_gate.daily_stats().await;
    assert_eq!(total_pnl, dec!(10.8));
    assert_eq!(success_count, 2);
    assert_eq!(failure_count, 0);
}

#[tokio::test]
async fn test_handle_force_close_domain_blocks_new_buy_immediately() {
    let (handle, _coordinator) = make_test_handle();
    handle
        .force_close_domain(Domain::Sports)
        .await
        .expect("force-close domain command accepted");

    let intent = OrderIntent::new(
        "sports",
        Domain::Sports,
        "nba-game-1",
        "sports-token-yes",
        crate::domain::Side::Up,
        true,
        10,
        dec!(0.45),
    )
    .with_deployment_id("deploy.sports.nba.test");

    let err = handle
        .submit_order(intent)
        .await
        .expect_err("buy intent should be blocked once domain is force-closed");
    assert!(err.to_string().contains("new intents are blocked"));
}

#[tokio::test]
async fn test_handle_shutdown_domain_blocks_new_buy_immediately() {
    let (handle, _coordinator) = make_test_handle();
    handle
        .shutdown_domain(Domain::Sports)
        .await
        .expect("shutdown domain command accepted");

    let intent = OrderIntent::new(
        "sports",
        Domain::Sports,
        "nba-game-2",
        "sports-token-yes",
        crate::domain::Side::Up,
        true,
        10,
        dec!(0.40),
    )
    .with_deployment_id("deploy.sports.nba.test");

    let err = handle
        .submit_order(intent)
        .await
        .expect_err("buy intent should be blocked once domain is shut down");
    assert!(err.to_string().contains("new intents are blocked"));
}

#[tokio::test]
async fn test_governance_status_includes_domain_ingress_and_agents() {
    let (handle, _coordinator) = make_test_handle();
    handle
        .pause_domain(Domain::Sports)
        .await
        .expect("pause domain command accepted");
    {
        let mut state = handle.global_state.write().await;
        state.agents.insert(
            "sports_agent".to_string(),
            AgentSnapshot {
                agent_id: "sports_agent".to_string(),
                name: "sports_agent".to_string(),
                domain: Domain::Sports,
                status: AgentStatus::Running,
                position_count: 0,
                exposure: dec!(12.5),
                daily_pnl: dec!(1.2),
                unrealized_pnl: dec!(0.3),
                metrics: HashMap::new(),
                last_heartbeat: Utc::now(),
                error_message: None,
            },
        );
    }

    let snapshot = handle.governance_status().await;

    assert!(snapshot
        .domain_ingress_modes
        .iter()
        .any(|row| row.domain == "sports" && row.mode == "paused"));
    assert!(snapshot
        .agents
        .iter()
        .any(|agent| agent.agent_id == "sports_agent"
            && agent.domain == "sports"
            && agent.status == "running"));
}
