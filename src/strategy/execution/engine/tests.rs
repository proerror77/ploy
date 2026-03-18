use super::*;
use crate::adapters::OrderResponse;
use crate::config::AppConfig;
use crate::domain::{Order, OrderStatus, Round};
use crate::exchange::{ExchangeClient, ExchangeKind};
use crate::strategy::execution::engine_store::mock::MockStore;
use async_trait::async_trait;
use chrono::{Duration, NaiveDate, Utc};
use rust_decimal_macros::dec;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

struct MockExchangeClient;

#[async_trait]
impl ExchangeClient for MockExchangeClient {
    fn kind(&self) -> ExchangeKind {
        ExchangeKind::Polymarket
    }

    fn is_dry_run(&self) -> bool {
        true
    }

    async fn submit_order_gateway(
        &self,
        _request: &crate::domain::OrderRequest,
    ) -> Result<OrderResponse> {
        Ok(OrderResponse {
            id: "mock-order-1".to_string(),
            status: "live".to_string(),
            owner: None,
            market: None,
            asset_id: None,
            side: None,
            original_size: None,
            size_matched: None,
            price: None,
            associate_trades: None,
            created_at: None,
            expiration: None,
            order_type: None,
        })
    }

    async fn get_order(&self, _order_id: &str) -> Result<OrderResponse> {
        Ok(OrderResponse {
            id: "mock-order-1".to_string(),
            status: "matched".to_string(),
            owner: None,
            market: None,
            asset_id: None,
            side: None,
            original_size: Some("100".to_string()),
            size_matched: Some("100".to_string()),
            price: Some("0.50".to_string()),
            associate_trades: None,
            created_at: None,
            expiration: None,
            order_type: None,
        })
    }

    async fn cancel_order(&self, _order_id: &str) -> Result<bool> {
        Ok(true)
    }

    async fn get_best_prices(&self, _token_id: &str) -> Result<(Option<Decimal>, Option<Decimal>)> {
        Ok((Some(dec!(0.48)), Some(dec!(0.52))))
    }

    fn infer_order_status(&self, _order: &OrderResponse) -> OrderStatus {
        OrderStatus::Filled
    }

    fn calculate_fill(&self, _order: &OrderResponse) -> (u64, Option<Decimal>) {
        (100, Some(dec!(0.50)))
    }
}

fn test_config() -> AppConfig {
    toml::from_str(
        r#"
        [market]
        ws_url = "wss://test"
        rest_url = "https://test"
        market_slug = "test-market"

        [strategy]
        shares = 100
        window_min = 5
        move_pct = "0.15"
        sum_target = "0.95"
        fee_buffer = "0.005"
        slippage_buffer = "0.02"
        profit_buffer = "0.01"

        [execution]
        order_timeout_ms = 5000
        max_retries = 3
        max_spread_bps = 500
        confirm_fills = false

        [risk]
        max_single_exposure_usd = "500"
        min_remaining_seconds = 60
        max_consecutive_failures = 3
        daily_loss_limit_usd = "100"
        leg2_force_close_seconds = 30

        [database]
        url = "postgres://test:test@localhost/test"

        [dry_run]
        enabled = true
        "#,
    )
    .expect("test config should parse")
}

fn test_round(minutes_from_now: i64) -> Round {
    let now = Utc::now();
    Round {
        id: None,
        slug: "test-btc-15m".to_string(),
        up_token_id: "up-token-123".to_string(),
        down_token_id: "down-token-456".to_string(),
        start_time: now - Duration::minutes(1),
        end_time: now + Duration::minutes(minutes_from_now),
        outcome: None,
    }
}

fn expired_round() -> Round {
    let now = Utc::now();
    Round {
        id: None,
        slug: "test-btc-expired".to_string(),
        up_token_id: "up-token-exp".to_string(),
        down_token_id: "down-token-exp".to_string(),
        start_time: now - Duration::minutes(20),
        end_time: now - Duration::minutes(5),
        outcome: None,
    }
}

async fn test_engine() -> StrategyEngine {
    let config = test_config();
    let executor =
        OrderExecutor::new_with_exchange(Arc::new(MockExchangeClient), config.execution.clone());
    let quote_cache = QuoteCache::new();
    StrategyEngine::new(config, MockStore::new(), executor, quote_cache)
        .await
        .expect("engine should construct")
}

async fn test_engine_with_store(store: impl EngineStore + 'static) -> StrategyEngine {
    let config = test_config();
    let executor =
        OrderExecutor::new_with_exchange(Arc::new(MockExchangeClient), config.execution.clone());
    let quote_cache = QuoteCache::new();
    StrategyEngine::new(config, store, executor, quote_cache)
        .await
        .expect("engine should construct")
}

fn seed_quote(cache: &QuoteCache, token_id: &str, side: Side) {
    cache.update(
        token_id,
        side,
        Some(dec!(0.49)),
        Some(dec!(0.51)),
        Some(dec!(1000)),
        Some(dec!(1000)),
    );
}

#[derive(Clone, Default)]
struct RecordingStore {
    inner: Arc<RecordingStoreInner>,
}

#[derive(Default)]
struct RecordingStoreInner {
    next_id: AtomicI32,
    fail_cycle_state: AtomicBool,
    fail_leg1: AtomicBool,
    fail_leg2: AtomicBool,
    cycle_state_expected_versions: Mutex<Vec<i32>>,
    leg1_expected_versions: Mutex<Vec<i32>>,
    leg2_expected_versions: Mutex<Vec<i32>>,
    abort_reasons: Mutex<Vec<String>>,
}

impl RecordingStore {
    fn new() -> Self {
        Self {
            inner: Arc::new(RecordingStoreInner {
                next_id: AtomicI32::new(1),
                ..Default::default()
            }),
        }
    }

    fn with_cycle_state_conflict(self) -> Self {
        self.inner.fail_cycle_state.store(true, Ordering::SeqCst);
        self
    }

    fn with_leg1_conflict(self) -> Self {
        self.inner.fail_leg1.store(true, Ordering::SeqCst);
        self
    }

    fn with_leg2_conflict(self) -> Self {
        self.inner.fail_leg2.store(true, Ordering::SeqCst);
        self
    }

    fn next_id(&self) -> i32 {
        self.inner.next_id.fetch_add(1, Ordering::SeqCst)
    }

    fn cycle_state_expected_versions(&self) -> Vec<i32> {
        self.inner
            .cycle_state_expected_versions
            .lock()
            .expect("cycle_state_expected_versions lock poisoned")
            .clone()
    }

    fn leg1_expected_versions(&self) -> Vec<i32> {
        self.inner
            .leg1_expected_versions
            .lock()
            .expect("leg1_expected_versions lock poisoned")
            .clone()
    }

    fn leg2_expected_versions(&self) -> Vec<i32> {
        self.inner
            .leg2_expected_versions
            .lock()
            .expect("leg2_expected_versions lock poisoned")
            .clone()
    }

    fn abort_reasons(&self) -> Vec<String> {
        self.inner
            .abort_reasons
            .lock()
            .expect("abort_reasons lock poisoned")
            .clone()
    }
}

#[async_trait]
impl EngineStore for RecordingStore {
    async fn upsert_round(&self, _round: &Round) -> Result<i32> {
        Ok(self.next_id())
    }

    async fn create_cycle(&self, _round_id: i32, _state: StrategyState) -> Result<i32> {
        Ok(self.next_id())
    }

    async fn update_cycle_state(
        &self,
        _cycle_id: i32,
        _state: StrategyState,
        expected_version: i32,
    ) -> Result<bool> {
        self.inner
            .cycle_state_expected_versions
            .lock()
            .expect("cycle_state_expected_versions lock poisoned")
            .push(expected_version);
        Ok(!self.inner.fail_cycle_state.load(Ordering::SeqCst))
    }

    async fn update_cycle_leg1(
        &self,
        _cycle_id: i32,
        _side: Side,
        _entry_price: Decimal,
        _shares: u64,
        expected_version: i32,
    ) -> Result<bool> {
        self.inner
            .leg1_expected_versions
            .lock()
            .expect("leg1_expected_versions lock poisoned")
            .push(expected_version);
        Ok(!self.inner.fail_leg1.load(Ordering::SeqCst))
    }

    async fn update_cycle_leg2(
        &self,
        _cycle_id: i32,
        _entry_price: Decimal,
        _shares: u64,
        _pnl: Decimal,
        expected_version: i32,
    ) -> Result<bool> {
        self.inner
            .leg2_expected_versions
            .lock()
            .expect("leg2_expected_versions lock poisoned")
            .push(expected_version);
        Ok(!self.inner.fail_leg2.load(Ordering::SeqCst))
    }

    async fn abort_cycle(&self, _cycle_id: i32, reason: &str) -> Result<()> {
        self.inner
            .abort_reasons
            .lock()
            .expect("abort_reasons lock poisoned")
            .push(reason.to_string());
        Ok(())
    }

    async fn insert_order(&self, _order: &Order) -> Result<i32> {
        Ok(self.next_id())
    }

    async fn update_order_status(
        &self,
        _client_order_id: &str,
        _status: OrderStatus,
        _exchange_order_id: Option<&str>,
    ) -> Result<()> {
        Ok(())
    }

    async fn update_order_fill(
        &self,
        _client_order_id: &str,
        _filled_shares: u64,
        _avg_fill_price: Decimal,
        _status: OrderStatus,
    ) -> Result<()> {
        Ok(())
    }

    async fn update_strategy_state(
        &self,
        _state: StrategyState,
        _round_id: Option<i32>,
        _cycle_id: Option<i32>,
    ) -> Result<()> {
        Ok(())
    }

    async fn increment_cycle_count(&self, _date: NaiveDate) -> Result<()> {
        Ok(())
    }

    async fn record_cycle_completion(&self, _date: NaiveDate, _pnl: Decimal) -> Result<()> {
        Ok(())
    }

    async fn record_cycle_abort(&self, _date: NaiveDate) -> Result<()> {
        Ok(())
    }

    async fn record_cycle_abort_neutral(&self, _date: NaiveDate) -> Result<()> {
        Ok(())
    }

    async fn halt_trading(&self, _date: NaiveDate, _reason: &str) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn initial_state_is_idle() {
    let engine = test_engine().await;
    assert_eq!(engine.state().await, StrategyState::Idle);
}

#[tokio::test]
async fn set_round_transitions_to_watch_window() {
    let engine = test_engine().await;
    let round = test_round(15);
    engine.set_round(round).await.unwrap();
    assert_eq!(engine.state().await, StrategyState::WatchWindow);
}

#[tokio::test]
async fn set_round_dedup_same_slug() {
    let engine = test_engine().await;
    let round = test_round(15);
    engine.set_round(round.clone()).await.unwrap();
    let v1 = engine.state.read().await.version;

    engine.set_round(round).await.unwrap();
    let v2 = engine.state.read().await.version;
    assert_eq!(v1, v2, "version should not change on duplicate round");
}

#[tokio::test]
async fn set_round_expired_stays_idle() {
    let engine = test_engine().await;
    let round = expired_round();
    engine.set_round(round).await.unwrap();
    let state = engine.state().await;
    assert!(
        state == StrategyState::Idle,
        "expired round should not enter WatchWindow, got {:?}",
        state
    );
}

#[tokio::test]
async fn shutdown_sets_flag() {
    let engine = test_engine().await;
    assert!(!engine.state.read().await.shutdown);
    engine.shutdown().await;
    assert!(engine.state.read().await.shutdown);
}

#[tokio::test]
async fn transition_to_idle_clears_state() {
    let engine = test_engine().await;
    let round = test_round(15);
    engine.set_round(round).await.unwrap();
    assert_eq!(engine.state().await, StrategyState::WatchWindow);

    engine.transition_to_idle().await.unwrap();
    assert_eq!(engine.state().await, StrategyState::Idle);
    let state = engine.state.read().await;
    assert!(state.current_round.is_none(), "round should be cleared");
    assert!(state.current_cycle.is_none(), "cycle should be cleared");
}

#[tokio::test]
async fn version_increments_on_state_change() {
    let engine = test_engine().await;
    let v0 = engine.state.read().await.version;
    assert_eq!(v0, 0);

    engine.set_round(test_round(15)).await.unwrap();
    let v1 = engine.state.read().await.version;
    assert!(v1 > v0, "version should increment after set_round");

    engine.transition_to_idle().await.unwrap();
    let v2 = engine.state.read().await.version;
    assert!(v2 > v1, "version should increment after transition_to_idle");
}

#[tokio::test]
async fn abort_cycle_without_active_cycle() {
    let engine = test_engine().await;
    engine.abort_cycle("test reason").await.unwrap();
    assert_eq!(engine.state().await, StrategyState::Abort);
}

#[tokio::test]
async fn abort_cycle_neutral_without_active_cycle() {
    let engine = test_engine().await;
    engine.abort_cycle_neutral("neutral test").await.unwrap();
    assert_eq!(engine.state().await, StrategyState::Abort);
}

#[tokio::test]
async fn set_round_blocked_mid_cycle() {
    let engine = test_engine().await;
    let round = test_round(15);
    engine.set_round(round).await.unwrap();

    {
        let mut state = engine.state.write().await;
        state.strategy_state = StrategyState::Leg1Filled;
        state.current_cycle = Some(CycleContext {
            cycle_id: 42,
            leg1_side: Side::Up,
            leg1_price: dec!(0.45),
            leg1_shares: 100,
            leg1_order_id: "test-order".to_string(),
            leg2_order_id: None,
            force_leg2_attempted: false,
            cycle_version: 0,
        });
    }

    let new_round = Round {
        slug: "test-btc-different".to_string(),
        ..test_round(15)
    };
    engine.set_round(new_round).await.unwrap();

    assert_eq!(engine.state().await, StrategyState::Leg1Filled);
}

#[tokio::test]
async fn abort_cycle_with_active_cycle_clears_context() {
    let engine = test_engine().await;
    let round = test_round(15);
    engine.set_round(round).await.unwrap();

    {
        let mut state = engine.state.write().await;
        state.strategy_state = StrategyState::Leg1Filled;
        state.current_cycle = Some(CycleContext {
            cycle_id: 99,
            leg1_side: Side::Down,
            leg1_price: dec!(0.55),
            leg1_shares: 50,
            leg1_order_id: "leg1-order".to_string(),
            leg2_order_id: None,
            force_leg2_attempted: false,
            cycle_version: 0,
        });
    }

    engine.abort_cycle("test abort with cycle").await.unwrap();
    assert_eq!(engine.state().await, StrategyState::Abort);
    assert!(
        engine.state.read().await.current_cycle.is_none(),
        "cycle should be cleared after abort"
    );
}

#[tokio::test]
async fn dry_run_safety_guard_rejects_live_mode_without_confirm_fills() {
    let mut config = test_config();
    config.dry_run.enabled = false;
    config.execution.confirm_fills = false;

    let executor =
        OrderExecutor::new_with_exchange(Arc::new(MockExchangeClient), config.execution.clone());
    let result = StrategyEngine::new(config, MockStore::new(), executor, QuoteCache::new()).await;
    assert!(
        result.is_err(),
        "should reject live mode without confirm_fills"
    );
}

#[tokio::test]
async fn leg_updates_should_use_incrementing_cycle_versions() {
    let store = RecordingStore::new();
    let engine = test_engine_with_store(store.clone()).await;

    let round = test_round(15);
    let up_token = round.up_token_id.clone();
    let down_token = round.down_token_id.clone();
    engine.set_round(round).await.unwrap();

    seed_quote(&engine.quote_cache, &up_token, Side::Up);
    engine.enter_leg1(Side::Up, dec!(0.50)).await.unwrap();

    seed_quote(&engine.quote_cache, &down_token, Side::Down);
    engine.enter_leg2(Side::Down, dec!(0.50)).await.unwrap();

    assert_eq!(
        store.leg1_expected_versions(),
        vec![1],
        "Leg1 update must use initial DB version"
    );
    assert_eq!(
        store.cycle_state_expected_versions(),
        vec![2],
        "LEG2_PENDING transition must use post-Leg1 version"
    );
    assert_eq!(
        store.leg2_expected_versions(),
        vec![3],
        "Leg2 fill update must use post-LEG2_PENDING version"
    );
}

#[tokio::test]
async fn leg1_cycle_version_conflict_should_abort_and_error() {
    let store = RecordingStore::new().with_leg1_conflict();
    let engine = test_engine_with_store(store.clone()).await;

    let round = test_round(15);
    let up_token = round.up_token_id.clone();
    engine.set_round(round).await.unwrap();

    seed_quote(&engine.quote_cache, &up_token, Side::Up);
    let result = engine.enter_leg1(Side::Up, dec!(0.50)).await;

    assert!(result.is_err(), "Leg1 version conflict must fail the cycle");
    assert_eq!(engine.state().await, StrategyState::Abort);
    assert!(
        !store.abort_reasons().is_empty(),
        "Leg1 version conflict should persist abort reason"
    );
}

#[tokio::test]
async fn leg2_pending_cycle_version_conflict_should_abort_and_error() {
    let store = RecordingStore::new().with_cycle_state_conflict();
    let engine = test_engine_with_store(store.clone()).await;

    let round = test_round(15);
    let up_token = round.up_token_id.clone();
    let down_token = round.down_token_id.clone();
    engine.set_round(round).await.unwrap();

    seed_quote(&engine.quote_cache, &up_token, Side::Up);
    engine.enter_leg1(Side::Up, dec!(0.50)).await.unwrap();

    seed_quote(&engine.quote_cache, &down_token, Side::Down);
    let result = engine.enter_leg2(Side::Down, dec!(0.50)).await;

    assert!(
        result.is_err(),
        "LEG2_PENDING version conflict must fail the cycle"
    );
    assert_eq!(engine.state().await, StrategyState::Abort);
    assert!(
        !store.abort_reasons().is_empty(),
        "LEG2_PENDING version conflict should persist abort reason"
    );
}

#[tokio::test]
async fn leg2_cycle_version_conflict_should_abort_and_error() {
    let store = RecordingStore::new().with_leg2_conflict();
    let engine = test_engine_with_store(store.clone()).await;

    let round = test_round(15);
    let up_token = round.up_token_id.clone();
    let down_token = round.down_token_id.clone();
    engine.set_round(round).await.unwrap();

    seed_quote(&engine.quote_cache, &up_token, Side::Up);
    engine.enter_leg1(Side::Up, dec!(0.50)).await.unwrap();

    seed_quote(&engine.quote_cache, &down_token, Side::Down);
    let result = engine.enter_leg2(Side::Down, dec!(0.50)).await;

    assert!(result.is_err(), "Leg2 version conflict must fail the cycle");
    assert_eq!(engine.state().await, StrategyState::Abort);
    assert!(
        !store.abort_reasons().is_empty(),
        "Leg2 version conflict should persist abort reason"
    );
}
