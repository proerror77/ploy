use super::*;
use crate::coordinator::OrderPriority;
use crate::domain::{Domain, Side};
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{Barrier, Mutex};
use tokio::time::{Duration, timeout};

fn make_intent(agent: &str, priority: OrderPriority) -> OrderIntent {
    OrderIntent::new(
        agent,
        Domain::Crypto,
        "test-market",
        "token-123",
        Side::Up,
        true,
        100,
        Decimal::from_str_exact("0.50").unwrap(),
    )
    .with_priority(priority)
}

#[test]
fn test_priority_ordering() {
    let mut queue = OrderQueue::new(100);

    queue
        .enqueue(make_intent("a1", OrderPriority::Normal))
        .unwrap();
    queue
        .enqueue(make_intent("a2", OrderPriority::Low))
        .unwrap();
    queue
        .enqueue(make_intent("a3", OrderPriority::Critical))
        .unwrap();
    queue
        .enqueue(make_intent("a4", OrderPriority::High))
        .unwrap();

    assert_eq!(queue.dequeue().unwrap().agent_id, "a3");
    assert_eq!(queue.dequeue().unwrap().agent_id, "a4");
    assert_eq!(queue.dequeue().unwrap().agent_id, "a1");
    assert_eq!(queue.dequeue().unwrap().agent_id, "a2");
}

#[test]
fn test_fifo_same_priority() {
    let mut queue = OrderQueue::new(100);

    queue
        .enqueue(make_intent("first", OrderPriority::Normal))
        .unwrap();
    queue
        .enqueue(make_intent("second", OrderPriority::Normal))
        .unwrap();
    queue
        .enqueue(make_intent("third", OrderPriority::Normal))
        .unwrap();

    assert_eq!(queue.dequeue().unwrap().agent_id, "first");
    assert_eq!(queue.dequeue().unwrap().agent_id, "second");
    assert_eq!(queue.dequeue().unwrap().agent_id, "third");
}

#[test]
fn test_queue_full() {
    let mut queue = OrderQueue::new(2);

    queue
        .enqueue(make_intent("a1", OrderPriority::Normal))
        .unwrap();
    queue
        .enqueue(make_intent("a2", OrderPriority::Normal))
        .unwrap();

    let result = queue.enqueue(make_intent("a3", OrderPriority::Low));
    assert!(result.is_err());

    queue
        .enqueue(make_intent("a4", OrderPriority::Critical))
        .unwrap();
    assert_eq!(queue.len(), 2);
}

#[test]
fn test_enqueue_with_eviction_returns_dropped_intent() {
    let mut queue = OrderQueue::new(2);
    queue
        .enqueue(make_intent("a1", OrderPriority::Normal))
        .unwrap();
    queue
        .enqueue(make_intent("a2", OrderPriority::Low))
        .unwrap();

    let dropped = queue
        .enqueue_with_eviction(make_intent("a3", OrderPriority::Critical))
        .unwrap()
        .expect("expected an evicted intent");

    assert_eq!(dropped.agent_id, "a2");
    assert_eq!(queue.len(), 2);
}

#[test]
fn test_stats() {
    let mut queue = OrderQueue::new(100);

    queue
        .enqueue(make_intent("a1", OrderPriority::Critical))
        .unwrap();
    queue
        .enqueue(make_intent("a2", OrderPriority::High))
        .unwrap();
    queue
        .enqueue(make_intent("a3", OrderPriority::Normal))
        .unwrap();
    queue
        .enqueue(make_intent("a4", OrderPriority::Low))
        .unwrap();

    let stats = queue.stats();
    assert_eq!(stats.current_size, 4);
    assert_eq!(stats.critical_count, 1);
    assert_eq!(stats.high_count, 1);
    assert_eq!(stats.normal_count, 1);
    assert_eq!(stats.low_count, 1);
}

#[test]
fn test_pending_buy_notional_excluding_domains() {
    let mut queue = OrderQueue::new(100);

    let crypto_buy = OrderIntent::new(
        "a1",
        Domain::Crypto,
        "btc-up",
        "token-btc",
        Side::Up,
        true,
        10,
        Decimal::from_str_exact("0.50").unwrap(),
    );
    let politics_buy = OrderIntent::new(
        "a2",
        Domain::Politics,
        "election-yes",
        "token-pol",
        Side::Up,
        true,
        20,
        Decimal::from_str_exact("0.50").unwrap(),
    );
    let mut politics_sell = OrderIntent::new(
        "a3",
        Domain::Politics,
        "election-yes",
        "token-pol",
        Side::Up,
        false,
        30,
        Decimal::from_str_exact("0.50").unwrap(),
    );
    politics_sell.priority = OrderPriority::Low;

    queue.enqueue(crypto_buy).unwrap();
    queue.enqueue(politics_buy).unwrap();
    queue.enqueue(politics_sell).unwrap();

    let notional = queue.pending_buy_notional_excluding_domains(&[Domain::Crypto, Domain::Sports]);
    assert_eq!(notional, Decimal::from_str("10.00").unwrap());
}

#[test]
fn test_remove_buy_orders_with_domain_filter() {
    let mut queue = OrderQueue::new(100);
    let crypto_buy = make_intent("a1", OrderPriority::Normal);
    let sports_buy = OrderIntent::new(
        "a2",
        Domain::Sports,
        "nba",
        "token-s",
        Side::Up,
        true,
        10,
        Decimal::from_str_exact("0.50").unwrap(),
    );
    let mut crypto_sell = make_intent("a3", OrderPriority::Low);
    crypto_sell.is_buy = false;

    queue.enqueue(crypto_buy).unwrap();
    queue.enqueue(sports_buy).unwrap();
    queue.enqueue(crypto_sell).unwrap();

    let removed = queue.remove_buy_orders(Some(Domain::Crypto));
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].domain, Domain::Crypto);
    assert_eq!(queue.len(), 2);

    let removed_all = queue.remove_buy_orders(None);
    assert_eq!(removed_all.len(), 1);
    assert_eq!(removed_all[0].domain, Domain::Sports);
    assert_eq!(queue.len(), 1);
}

#[test]
fn test_pending_sell_shares_for_filters_bucket() {
    let mut queue = OrderQueue::new(100);

    let mut sell_a = OrderIntent::new(
        "agent1",
        Domain::Crypto,
        "btc-up",
        "TOKEN-UP",
        Side::Up,
        false,
        40,
        Decimal::from_str_exact("0.50").unwrap(),
    );
    sell_a.priority = OrderPriority::High;

    let mut sell_b = OrderIntent::new(
        "agent1",
        Domain::Crypto,
        "btc-up",
        "token-up",
        Side::Up,
        false,
        35,
        Decimal::from_str_exact("0.50").unwrap(),
    );
    sell_b.priority = OrderPriority::Normal;

    let mut other_side = OrderIntent::new(
        "agent1",
        Domain::Crypto,
        "btc-up",
        "token-up",
        Side::Down,
        false,
        20,
        Decimal::from_str_exact("0.50").unwrap(),
    );
    other_side.priority = OrderPriority::Low;

    queue.enqueue(sell_a).unwrap();
    queue.enqueue(sell_b).unwrap();
    queue.enqueue(other_side).unwrap();

    assert_eq!(
        queue.pending_sell_shares_for("agent1", Domain::Crypto, "token-up", Side::Up),
        75
    );
}

#[test]
fn test_cleanup_expired_intents_returns_removed_items() {
    let mut queue = OrderQueue::new(100);
    let mut expired = make_intent("agent1", OrderPriority::Normal);
    expired.is_buy = false;
    expired.expires_at = Some(chrono::Utc::now() + chrono::Duration::milliseconds(10));

    let active = make_intent("agent2", OrderPriority::High);

    queue.enqueue(expired).unwrap();
    queue.enqueue(active).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(20));
    let removed = queue.cleanup_expired_intents();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].agent_id, "agent1");
    assert_eq!(queue.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_enqueue_dequeue_pressure() {
    let producer_count = 4usize;
    let intents_per_producer = 25usize;
    let total_intents = producer_count * intents_per_producer;
    let queue = Arc::new(Mutex::new(OrderQueue::new(total_intents + 8)));
    let start = Arc::new(Barrier::new(producer_count + 1));

    let mut producers = Vec::with_capacity(producer_count);
    for producer_idx in 0..producer_count {
        let queue = queue.clone();
        let start = start.clone();
        producers.push(tokio::spawn(async move {
            start.wait().await;
            for intent_idx in 0..intents_per_producer {
                let agent_id = format!("p{}-{}", producer_idx, intent_idx);
                let priority = match intent_idx % 4 {
                    0 => OrderPriority::Critical,
                    1 => OrderPriority::High,
                    2 => OrderPriority::Normal,
                    _ => OrderPriority::Low,
                };
                queue
                    .lock()
                    .await
                    .enqueue(make_intent(&agent_id, priority))
                    .expect("enqueue under concurrent pressure");
                tokio::task::yield_now().await;
            }
        }));
    }

    let consumer_queue = queue.clone();
    let consumer_start = start.clone();
    let consumer = tokio::spawn(async move {
        consumer_start.wait().await;
        let mut seen = HashSet::with_capacity(total_intents);

        while seen.len() < total_intents {
            let maybe_intent = { consumer_queue.lock().await.dequeue() };
            if let Some(intent) = maybe_intent {
                assert!(
                    seen.insert(intent.agent_id.clone()),
                    "duplicate dequeue for {}",
                    intent.agent_id
                );
            } else {
                tokio::task::yield_now().await;
            }
        }

        seen
    });

    for producer in producers {
        producer.await.expect("producer task should finish");
    }

    let seen = timeout(Duration::from_secs(5), consumer)
        .await
        .expect("consumer should drain queue under concurrent pressure")
        .expect("consumer task should finish");

    assert_eq!(seen.len(), total_intents);
    assert!(
        queue.lock().await.is_empty(),
        "queue should drain completely"
    );
}
