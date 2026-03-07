use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;

use crate::adapters::PolymarketClient;
use crate::config::EventEdgeAgentConfig;
use crate::domain::{OrderRequest, OrderStatus};
use crate::error::Result;
use crate::strategy::event_edge::core::{EventEdgeCore, TradeDecision};
use crate::strategy::event_edge::data_source::{ArenaTextSource, EventDataSource};
use crate::strategy::traits::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction, StrategyEvent,
    StrategyEventType, StrategyStateInfo,
};

struct PendingEventEdgeOrder {
    decision: TradeDecision,
    spend: Decimal,
    cooldown_recorded: bool,
}

pub struct EventEdgeStrategy {
    id: String,
    core: EventEdgeCore,
    data_source: Box<dyn EventDataSource>,
    enabled: bool,
    poll_interval_secs: u64,
    pending_orders: HashMap<String, PendingEventEdgeOrder>,
    positions: HashMap<String, PositionInfo>,
    last_update: DateTime<Utc>,
}

impl EventEdgeStrategy {
    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        use toml::Value;

        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow::anyhow!("Invalid TOML: {}", e))?;

        let empty_table = Value::Table(Default::default());
        let strategy = config.get("strategy").unwrap_or(&empty_table);
        let events = config.get("events").unwrap_or(&empty_table);
        let entry = config.get("entry").unwrap_or(&empty_table);
        let timing = config.get("timing").unwrap_or(&empty_table);
        let risk = config.get("risk").unwrap_or(&empty_table);
        let polymarket = config.get("polymarket").unwrap_or(&empty_table);

        let rest_url = polymarket
            .get("rest_url")
            .and_then(|v| v.as_str())
            .unwrap_or("https://clob.polymarket.com");
        let client = PolymarketClient::new(rest_url, dry_run)?;

        let event_ids = events
            .get("event_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let titles = events
            .get("titles")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let cfg = EventEdgeAgentConfig {
            enabled: strategy
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            framework: "deterministic".to_string(),
            event_ids,
            titles,
            interval_secs: timing
                .get("poll_interval_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(300) as u64,
            min_edge: entry
                .get("min_edge")
                .and_then(|v| v.as_float())
                .and_then(Decimal::from_f64_retain)
                .unwrap_or_else(|| Decimal::new(8, 2)),
            max_entry: entry
                .get("max_entry")
                .and_then(|v| v.as_float())
                .and_then(Decimal::from_f64_retain)
                .unwrap_or_else(|| Decimal::new(75, 2)),
            shares: entry
                .get("shares")
                .and_then(|v| v.as_integer())
                .unwrap_or(100) as u64,
            trade: strategy
                .get("trade")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            cooldown_secs: risk
                .get("cooldown_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(300) as u64,
            max_daily_spend_usd: risk
                .get("max_daily_spend_usd")
                .and_then(|v| v.as_float())
                .and_then(Decimal::from_f64_retain)
                .unwrap_or_else(|| Decimal::from(100)),
            model: None,
            claude_max_turns: 0,
        };

        Ok(Self {
            id,
            poll_interval_secs: cfg.interval_secs,
            enabled: cfg.enabled,
            core: EventEdgeCore::new(client, cfg),
            data_source: Box::new(ArenaTextSource::default()),
            pending_orders: HashMap::new(),
            positions: HashMap::new(),
            last_update: Utc::now(),
        })
    }

    #[cfg(test)]
    fn new_for_tests(id: &str, core: EventEdgeCore, data_source: Box<dyn EventDataSource>) -> Self {
        let poll_interval_secs = core.cfg.interval_secs;
        let enabled = core.cfg.enabled;
        Self {
            id: id.to_string(),
            core,
            data_source,
            enabled,
            poll_interval_secs,
            pending_orders: HashMap::new(),
            positions: HashMap::new(),
            last_update: Utc::now(),
        }
    }

    fn build_submit_action(&mut self, decision: TradeDecision) -> StrategyAction {
        let client_order_id = format!(
            "event_edge_{}_{}_{}",
            decision.event_id,
            decision.token_id,
            Utc::now().timestamp_millis()
        );
        let mut order = OrderRequest::buy_limit(
            decision.token_id.clone(),
            decision.side,
            decision.shares,
            decision.limit_price,
        );
        order.client_order_id = client_order_id.clone();
        order.idempotency_key = Some(client_order_id.clone());

        let spend = Decimal::from(decision.shares) * decision.limit_price;
        self.pending_orders.insert(
            client_order_id.clone(),
            PendingEventEdgeOrder {
                decision,
                spend,
                cooldown_recorded: false,
            },
        );

        StrategyAction::SubmitOrder {
            client_order_id,
            purpose: crate::strategy::OrderPurpose::from_order_request(&order),
            order,
            priority: 5,
        }
    }

    fn record_pending_trade_if_needed(&mut self, client_order_id: &str) {
        if let Some(pending) = self.pending_orders.get_mut(client_order_id) {
            if !pending.cooldown_recorded {
                self.core
                    .record_trade(&pending.decision.token_id, pending.spend);
                pending.cooldown_recorded = true;
            }
        }
    }
}

#[async_trait]
impl Strategy for EventEdgeStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Event Edge Strategy"
    }

    fn description(&self) -> &str {
        "External-data mispricing scanner for event markets"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![DataFeed::Tick {
            interval_ms: self.poll_interval_secs.saturating_mul(1000),
        }]
    }

    async fn on_market_update(&mut self, _update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        Ok(Vec::new())
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        self.last_update = update.timestamp;
        let Some(client_order_id) = update.client_order_id.as_deref() else {
            return Ok(Vec::new());
        };

        if matches!(
            update.status,
            OrderStatus::Submitted | OrderStatus::PartiallyFilled | OrderStatus::Filled
        ) {
            self.record_pending_trade_if_needed(client_order_id);
        }

        match update.status {
            OrderStatus::PartiallyFilled | OrderStatus::Filled => {
                if let Some(pending) = self.pending_orders.get(client_order_id) {
                    let price = update
                        .avg_fill_price
                        .unwrap_or(pending.decision.limit_price);
                    let shares = if update.filled_qty > 0 {
                        update.filled_qty
                    } else {
                        pending.decision.shares
                    };
                    self.positions.insert(
                        pending.decision.token_id.clone(),
                        PositionInfo::new(
                            pending.decision.token_id.clone(),
                            pending.decision.side,
                            shares,
                            price,
                            self.id.clone(),
                        ),
                    );
                }
                if matches!(update.status, OrderStatus::Filled) {
                    self.pending_orders.remove(client_order_id);
                }
            }
            OrderStatus::Rejected
            | OrderStatus::Cancelled
            | OrderStatus::Expired
            | OrderStatus::Failed => {
                self.pending_orders.remove(client_order_id);
            }
            OrderStatus::Pending | OrderStatus::Submitted => {}
        }

        Ok(Vec::new())
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        self.last_update = now;
        if !self.enabled || self.core.targets_empty() {
            return Ok(Vec::new());
        }

        let snapshot = self.data_source.fetch_snapshot().await?;
        if !self
            .data_source
            .has_changed(&snapshot, &self.core.state.last_arena_updated)
        {
            return Ok(Vec::new());
        }

        let event_ids = self.core.resolve_event_ids().await?;
        let mut actions = Vec::new();
        for event_id in &event_ids {
            if let Some(decision) = self
                .core
                .scan_and_decide(event_id, snapshot.arena.clone())
                .await?
            {
                if self.positions.contains_key(&decision.token_id)
                    || self
                        .pending_orders
                        .values()
                        .any(|pending| pending.decision.token_id == decision.token_id)
                {
                    continue;
                }
                actions.push(StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::SignalDetected,
                        format!(
                            "event_edge signal {} outcome={} edge={}",
                            decision.event_id, decision.outcome, decision.edge
                        ),
                    ),
                });
                actions.push(self.build_submit_action(decision));
            }
        }
        self.core.state.last_arena_updated = snapshot.last_updated;
        Ok(actions)
    }

    fn state(&self) -> StrategyStateInfo {
        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: "monitoring".to_string(),
            enabled: self.enabled,
            active: self.is_active(),
            position_count: self.positions.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure: self.positions.values().fold(Decimal::ZERO, |acc, pos| {
                acc + Decimal::from(pos.shares) * pos.entry_price
            }),
            unrealized_pnl: Decimal::ZERO,
            realized_pnl_today: Decimal::ZERO,
            last_update: self.last_update,
            metrics: HashMap::from([
                (
                    "resolved_event_ids".to_string(),
                    self.core.state.resolved_event_ids.len().to_string(),
                ),
                (
                    "daily_spend_usd".to_string(),
                    self.core.state.daily_spend_usd.to_string(),
                ),
            ]),
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.positions.values().cloned().collect()
    }

    fn is_active(&self) -> bool {
        !self.positions.is_empty() || !self.pending_orders.is_empty()
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.enabled = false;
        Ok(Vec::new())
    }

    fn reset(&mut self) {
        self.pending_orders.clear();
        self.positions.clear();
        self.core.state = Default::default();
        self.last_update = Utc::now();
        self.enabled = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Side;
    use crate::strategy::event_edge::data_source::EventSnapshot;
    use crate::strategy::traits::StrategyAction;
    use async_trait::async_trait;
    use chrono::NaiveDate;

    struct StaticEventDataSource {
        snapshot: EventSnapshot,
    }

    #[async_trait]
    impl EventDataSource for StaticEventDataSource {
        async fn fetch_snapshot(&self) -> Result<EventSnapshot> {
            Ok(self.snapshot.clone())
        }
    }

    fn test_core() -> EventEdgeCore {
        let client =
            PolymarketClient::new("https://clob.polymarket.com", true).expect("test client");
        let cfg = EventEdgeAgentConfig {
            enabled: true,
            framework: "deterministic".to_string(),
            event_ids: vec!["evt-1".to_string()],
            titles: Vec::new(),
            interval_secs: 300,
            min_edge: Decimal::new(8, 2),
            max_entry: Decimal::new(75, 2),
            shares: 42,
            trade: true,
            cooldown_secs: 300,
            max_daily_spend_usd: Decimal::from(100),
            model: None,
            claude_max_turns: 0,
        };
        EventEdgeCore::new(client, cfg)
    }

    #[test]
    fn from_toml_parses_event_targets() {
        let toml = r#"
[strategy]
name = "event_edge"
enabled = true
trade = true

[events]
event_ids = ["evt-1"]
titles = ["Best AI model"]

[entry]
min_edge = 0.08
max_entry = 0.70
shares = 25

[timing]
poll_interval_secs = 180

[risk]
cooldown_secs = 120
max_daily_spend_usd = 55.0

[polymarket]
rest_url = "https://clob.polymarket.com"
"#;
        let strategy = EventEdgeStrategy::from_toml("event_edge_test".to_string(), toml, true)
            .expect("strategy should parse");

        assert_eq!(
            strategy.required_feeds(),
            vec![DataFeed::Tick {
                interval_ms: 180_000
            }]
        );
        assert_eq!(strategy.core.cfg.event_ids, vec!["evt-1".to_string()]);
        assert_eq!(strategy.core.cfg.titles, vec!["Best AI model".to_string()]);
        assert_eq!(strategy.core.cfg.shares, 25);
        assert_eq!(strategy.core.cfg.cooldown_secs, 120);
    }

    #[test]
    fn build_submit_action_normalizes_trade_decision() {
        let snapshot = EventSnapshot {
            last_updated: Some(NaiveDate::from_ymd_opt(2026, 3, 6).unwrap()),
            fetched_at: Utc::now(),
            scores: HashMap::new(),
            arena: None,
        };
        let mut strategy = EventEdgeStrategy::new_for_tests(
            "event_edge_test",
            test_core(),
            Box::new(StaticEventDataSource { snapshot }),
        );
        let decision = TradeDecision {
            event_id: "evt-1".to_string(),
            outcome: "OpenAI".to_string(),
            token_id: "token-1".to_string(),
            condition_id: None,
            market_slug: "evt-1".to_string(),
            side: Side::Up,
            shares: 42,
            limit_price: Decimal::new(55, 2),
            edge: Decimal::new(12, 2),
            p_true: Decimal::new(67, 2),
            net_ev: Decimal::new(8, 2),
        };

        let action = strategy.build_submit_action(decision);
        match action {
            StrategyAction::SubmitOrder {
                client_order_id,
                order,
                ..
            } => {
                assert!(client_order_id.starts_with("event_edge_evt-1_token-1_"));
                assert_eq!(order.client_order_id, client_order_id);
                assert_eq!(
                    order.idempotency_key.as_deref(),
                    Some(client_order_id.as_str())
                );
                assert_eq!(order.token_id, "token-1");
                assert_eq!(order.shares, 42);
            }
            other => panic!("expected submit order action, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn order_update_filled_records_position() {
        let snapshot = EventSnapshot {
            last_updated: Some(NaiveDate::from_ymd_opt(2026, 3, 6).unwrap()),
            fetched_at: Utc::now(),
            scores: HashMap::new(),
            arena: None,
        };
        let mut strategy = EventEdgeStrategy::new_for_tests(
            "event_edge_test",
            test_core(),
            Box::new(StaticEventDataSource { snapshot }),
        );
        let action = strategy.build_submit_action(TradeDecision {
            event_id: "evt-1".to_string(),
            outcome: "OpenAI".to_string(),
            token_id: "token-1".to_string(),
            condition_id: None,
            market_slug: "evt-1".to_string(),
            side: Side::Up,
            shares: 42,
            limit_price: Decimal::new(55, 2),
            edge: Decimal::new(12, 2),
            p_true: Decimal::new(67, 2),
            net_ev: Decimal::new(8, 2),
        });

        let StrategyAction::SubmitOrder {
            client_order_id, ..
        } = action
        else {
            panic!("expected submit order");
        };
        strategy
            .on_order_update(&OrderUpdate {
                order_id: client_order_id.clone(),
                client_order_id: Some(client_order_id),
                status: OrderStatus::Filled,
                filled_qty: 42,
                avg_fill_price: Some(Decimal::new(54, 2)),
                timestamp: Utc::now(),
                error: None,
            })
            .await
            .expect("filled order should update strategy");

        assert_eq!(strategy.positions.len(), 1);
        assert_eq!(strategy.core.state.daily_spend_usd, Decimal::new(2310, 2));
    }
}
