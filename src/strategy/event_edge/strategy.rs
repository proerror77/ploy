use crate::adapters::PolymarketClient;
use crate::config::EventEdgeAgentConfig;
use crate::domain::{OrderRequest, OrderStatus, Side};
use crate::error::Result;
use crate::strategy::event_edge::core::{EventEdgeCore, EventEdgeState, TradeDecision};
use crate::strategy::event_edge::data_source::{ArenaTextSource, EventDataSource};
use crate::strategy::event_edge::EventEdgeScan;
use crate::strategy::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyEvent, StrategyEventType, StrategyStateInfo,
};
use anyhow::anyhow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;

const EVENT_EDGE_STRATEGY_NAME: &str = "event_edge";
const DEFAULT_PM_REST_URL: &str = "https://clob.polymarket.com";
const EVENT_EDGE_PRIORITY: u8 = 8;

#[derive(Debug, Clone)]
struct DiscoveredEventTarget {
    event_id: String,
    series_id: String,
    up_token: String,
    down_token: String,
    end_time: DateTime<Utc>,
    price_to_beat: Option<Decimal>,
    title: Option<String>,
    condition_id: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingEventEdgeOrder {
    client_order_id: String,
    event_id: String,
    outcome: String,
    token_id: String,
    condition_id: Option<String>,
    market_slug: String,
    side: Side,
    shares: u64,
    limit_price: Decimal,
    reserved_notional_usd: Decimal,
}

pub struct EventEdgeStrategy {
    id: String,
    dry_run: bool,
    enabled: bool,
    core: EventEdgeCore,
    data_source: Box<dyn EventDataSource>,
    discovered_events: HashMap<String, DiscoveredEventTarget>,
    pending_orders: HashMap<String, PendingEventEdgeOrder>,
    positions: HashMap<String, PositionInfo>,
    reserved_notional_usd: Decimal,
    last_scan_at: Option<DateTime<Utc>>,
    last_signal_event_id: Option<String>,
    last_error: Option<String>,
}

impl std::fmt::Debug for EventEdgeStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventEdgeStrategy")
            .field("id", &self.id)
            .field("dry_run", &self.dry_run)
            .field("enabled", &self.enabled)
            .field("discovered_events", &self.discovered_events.len())
            .field("pending_orders", &self.pending_orders.len())
            .field("positions", &self.positions.len())
            .finish()
    }
}

impl EventEdgeStrategy {
    pub fn new(id: String, core: EventEdgeCore, dry_run: bool) -> Self {
        let enabled = core.cfg.enabled;
        Self {
            id,
            dry_run,
            enabled,
            core,
            data_source: Box::new(ArenaTextSource::default()),
            discovered_events: HashMap::new(),
            pending_orders: HashMap::new(),
            positions: HashMap::new(),
            reserved_notional_usd: Decimal::ZERO,
            last_scan_at: None,
            last_signal_event_id: None,
            last_error: None,
        }
    }

    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        use toml::Value;

        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow!("Invalid TOML: {}", e))?;

        let strategy_section = config
            .get("strategy")
            .ok_or_else(|| anyhow!("Missing [strategy] section"))?;
        let strategy_name = strategy_section
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing strategy.name"))?;
        if strategy_name != EVENT_EDGE_STRATEGY_NAME {
            return Err(anyhow!(
                "strategy.name must be \"{}\", got \"{}\"",
                EVENT_EDGE_STRATEGY_NAME,
                strategy_name
            )
            .into());
        }

        let event_edge = config
            .get("event_edge")
            .ok_or_else(|| anyhow!("Missing [event_edge] section"))?;

        let mut cfg = EventEdgeAgentConfig {
            enabled: strategy_section
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            ..EventEdgeAgentConfig::default()
        };

        if let Some(enabled) = event_edge.get("enabled").and_then(|v| v.as_bool()) {
            cfg.enabled = enabled;
        }
        if let Some(framework) = event_edge.get("framework").and_then(|v| v.as_str()) {
            cfg.framework = framework.trim().to_string();
        }
        if let Some(event_ids) = string_list_from_toml(event_edge, "event_ids") {
            cfg.event_ids = event_ids;
        }
        if let Some(titles) = string_list_from_toml(event_edge, "titles") {
            cfg.titles = titles;
        }
        if let Some(interval_secs) = event_edge.get("interval_secs").and_then(|v| v.as_integer()) {
            cfg.interval_secs = interval_secs.max(1) as u64;
        }
        if let Some(min_edge) = decimal_from_toml(event_edge, "min_edge") {
            cfg.min_edge = min_edge;
        }
        if let Some(max_entry) = decimal_from_toml(event_edge, "max_entry") {
            cfg.max_entry = max_entry;
        }
        if let Some(shares) = event_edge.get("shares").and_then(|v| v.as_integer()) {
            cfg.shares = shares.max(0) as u64;
        }
        if let Some(trade) = event_edge.get("trade").and_then(|v| v.as_bool()) {
            cfg.trade = trade;
        }
        if let Some(cooldown_secs) = event_edge.get("cooldown_secs").and_then(|v| v.as_integer()) {
            cfg.cooldown_secs = cooldown_secs.max(0) as u64;
        }
        if let Some(max_daily_spend_usd) = decimal_from_toml(event_edge, "max_daily_spend_usd") {
            cfg.max_daily_spend_usd = max_daily_spend_usd;
        }
        if let Some(model) = event_edge.get("model").and_then(|v| v.as_str()) {
            cfg.model = Some(model.trim().to_string());
        }
        if let Some(turns) = event_edge
            .get("claude_max_turns")
            .and_then(|v| v.as_integer())
        {
            cfg.claude_max_turns = turns.max(0) as u32;
        }

        let problems = cfg.validate();
        if !problems.is_empty() {
            return Err(anyhow!("Invalid [event_edge] config: {}", problems.join("; ")).into());
        }

        let client = PolymarketClient::new(DEFAULT_PM_REST_URL, dry_run)?;
        Ok(Self::new(id, EventEdgeCore::new(client, cfg), dry_run))
    }

    pub fn with_data_source(mut self, data_source: Box<dyn EventDataSource>) -> Self {
        self.data_source = data_source;
        self
    }

    async fn resolve_target_event_ids(&mut self) -> Result<Vec<String>> {
        let mut event_ids = if self.core.cfg.titles.is_empty() {
            self.core.cfg.event_ids.clone()
        } else {
            self.core.resolve_event_ids().await?
        };

        event_ids.extend(self.discovered_events.keys().cloned());
        event_ids.sort();
        event_ids.dedup();
        self.core.state.resolved_event_ids = event_ids.clone();
        Ok(event_ids)
    }

    fn has_live_interest_in_token(&self, token_id: &str) -> bool {
        self.positions.contains_key(token_id)
            || self
                .pending_orders
                .values()
                .any(|pending| pending.token_id == token_id)
    }

    fn allowed_notional_with_reservations(&self, amount: Decimal) -> bool {
        self.core.state.daily_spend_usd + self.reserved_notional_usd + amount
            <= self.core.cfg.max_daily_spend_usd
    }

    fn reserve_pending_order(
        &mut self,
        decision: &TradeDecision,
        client_order_id: String,
    ) -> PendingEventEdgeOrder {
        let reserved_notional_usd = decision.limit_price * Decimal::from(decision.shares);
        let pending = PendingEventEdgeOrder {
            client_order_id: client_order_id.clone(),
            event_id: decision.event_id.clone(),
            outcome: decision.outcome.clone(),
            token_id: decision.token_id.clone(),
            condition_id: decision.condition_id.clone(),
            market_slug: decision.market_slug.clone(),
            side: decision.side,
            shares: decision.shares,
            limit_price: decision.limit_price,
            reserved_notional_usd,
        };
        self.reserved_notional_usd += reserved_notional_usd;
        self.pending_orders.insert(client_order_id, pending.clone());
        pending
    }

    fn release_pending_order(&mut self, client_order_id: &str) -> Option<PendingEventEdgeOrder> {
        let pending = self.pending_orders.remove(client_order_id)?;
        self.reserved_notional_usd =
            (self.reserved_notional_usd - pending.reserved_notional_usd).max(Decimal::ZERO);
        Some(pending)
    }

    fn build_signal_event(
        &self,
        decision: &TradeDecision,
        scan: &EventEdgeScan,
        now: DateTime<Utc>,
    ) -> StrategyEvent {
        StrategyEvent::new(
            StrategyEventType::SignalDetected,
            format!(
                "event_edge signal event={} outcome={} ask={:.4} edge={:.4} p_true={:.4}",
                decision.event_id,
                decision.outcome,
                decision.limit_price,
                decision.edge,
                decision.p_true
            ),
        )
        .with_data("event_id", &decision.event_id)
        .with_data("event_title", &scan.event_title)
        .with_data("outcome", &decision.outcome)
        .with_data("token_id", &decision.token_id)
        .with_data("market_slug", &decision.market_slug)
        .with_data("limit_price", decision.limit_price.to_string())
        .with_data("edge", decision.edge.to_string())
        .with_data("p_true", decision.p_true.to_string())
        .with_data("net_ev", decision.net_ev.to_string())
        .with_data("detected_at", now.to_rfc3339())
    }

    fn client_order_id(&self, decision: &TradeDecision, now: DateTime<Utc>) -> String {
        format!(
            "intent:event_edge:{}:{}:{}",
            sanitize_component(&decision.event_id),
            sanitize_component(&decision.token_id),
            now.timestamp_millis()
        )
    }

    fn build_actions_for_scan(
        &mut self,
        scan: &EventEdgeScan,
        now: DateTime<Utc>,
    ) -> Vec<StrategyAction> {
        if !self.enabled || !self.core.cfg.trade || self.core.daily_cap_reached() {
            return Vec::new();
        }

        let Some(decision) = self.core.pick_best_trade(scan) else {
            return Vec::new();
        };

        if self.has_live_interest_in_token(&decision.token_id) {
            return Vec::new();
        }

        let notional = decision.limit_price * Decimal::from(decision.shares);
        if !self.allowed_notional_with_reservations(notional) {
            return Vec::new();
        }

        let client_order_id = self.client_order_id(&decision, now);
        let mut order = OrderRequest::buy_limit(
            decision.token_id.clone(),
            decision.side,
            decision.shares,
            decision.limit_price,
        );
        order.client_order_id = client_order_id.clone();
        order.idempotency_key = Some(client_order_id.clone());

        self.reserve_pending_order(&decision, client_order_id.clone());
        self.last_signal_event_id = Some(decision.event_id.clone());

        vec![
            StrategyAction::LogEvent {
                event: self.build_signal_event(&decision, scan, now),
            },
            StrategyAction::SubmitOrder {
                client_order_id,
                order,
                priority: EVENT_EDGE_PRIORITY,
            },
        ]
    }

    fn update_position_from_fill(
        &mut self,
        pending: &PendingEventEdgeOrder,
        shares: u64,
        fill_price: Decimal,
        now: DateTime<Utc>,
    ) {
        let strategy_id = self.id.clone();
        let position = self
            .positions
            .entry(pending.token_id.clone())
            .or_insert_with(|| {
                PositionInfo::new(
                    pending.token_id.clone(),
                    pending.side,
                    shares,
                    fill_price,
                    strategy_id.clone(),
                )
            });
        position.side = pending.side;
        position.shares = shares;
        position.entry_price = fill_price;
        position.opened_at = now;
        position.current_price = Some(fill_price);
        position.unrealized_pnl = Decimal::ZERO;
        position
            .metadata
            .insert("event_id".to_string(), pending.event_id.clone());
        position
            .metadata
            .insert("outcome".to_string(), pending.outcome.clone());
        position
            .metadata
            .insert("market_slug".to_string(), pending.market_slug.clone());
        position.metadata.insert(
            "client_order_id".to_string(),
            pending.client_order_id.clone(),
        );
        if let Some(condition_id) = pending.condition_id.as_ref() {
            position
                .metadata
                .insert("condition_id".to_string(), condition_id.clone());
        }
    }

    fn state_metrics(&self) -> HashMap<String, String> {
        let mut metrics = HashMap::new();
        metrics.insert(
            "tracked_events".to_string(),
            self.discovered_events.len().to_string(),
        );
        metrics.insert(
            "resolved_events".to_string(),
            self.core.state.resolved_event_ids.len().to_string(),
        );
        metrics.insert(
            "reserved_notional_usd".to_string(),
            self.reserved_notional_usd.to_string(),
        );
        metrics.insert(
            "daily_spend_usd".to_string(),
            self.core.state.daily_spend_usd.to_string(),
        );
        metrics.insert("dry_run".to_string(), self.dry_run.to_string());
        metrics.insert("trade".to_string(), self.core.cfg.trade.to_string());
        metrics.insert(
            "interval_secs".to_string(),
            self.core.cfg.interval_secs.to_string(),
        );
        if let Some(last_scan_at) = self.last_scan_at {
            metrics.insert("last_scan_at".to_string(), last_scan_at.to_rfc3339());
        }
        if let Some(event_id) = self.last_signal_event_id.as_ref() {
            metrics.insert("last_signal_event_id".to_string(), event_id.clone());
        }
        if let Some(last_error) = self.last_error.as_ref() {
            metrics.insert("last_error".to_string(), last_error.clone());
        }
        metrics
    }

    #[cfg(test)]
    fn build_actions_for_scan_for_test(
        &mut self,
        scan: &EventEdgeScan,
        now: DateTime<Utc>,
    ) -> Vec<StrategyAction> {
        self.build_actions_for_scan(scan, now)
    }
}

#[async_trait]
impl Strategy for EventEdgeStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        EVENT_EDGE_STRATEGY_NAME
    }

    fn description(&self) -> &str {
        "Arena-driven event edge strategy on Polymarket events"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![DataFeed::Tick {
            interval_ms: self.core.cfg.interval_secs.saturating_mul(1000),
        }]
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        match update {
            MarketUpdate::EventDiscovered {
                event_id,
                series_id,
                up_token,
                down_token,
                end_time,
                price_to_beat,
                title,
                condition_id,
            } => {
                self.discovered_events.insert(
                    event_id.clone(),
                    DiscoveredEventTarget {
                        event_id: event_id.clone(),
                        series_id: series_id.clone(),
                        up_token: up_token.clone(),
                        down_token: down_token.clone(),
                        end_time: *end_time,
                        price_to_beat: *price_to_beat,
                        title: title.clone(),
                        condition_id: condition_id.clone(),
                    },
                );
            }
            MarketUpdate::EventExpired { event_id } => {
                self.discovered_events.remove(event_id);
            }
            MarketUpdate::PolymarketQuote {
                token_id, quote, ..
            } => {
                if let Some(position) = self.positions.get_mut(token_id) {
                    if let Some(mark_price) =
                        quote.mid_price().or(quote.best_bid).or(quote.best_ask)
                    {
                        position.update_price(mark_price);
                    }
                }
            }
            MarketUpdate::BinancePrice { .. }
            | MarketUpdate::BinanceL2 { .. }
            | MarketUpdate::BinanceKline { .. } => {}
        }

        Ok(Vec::new())
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        let Some(client_order_id) = update.client_order_id.as_deref() else {
            return Ok(Vec::new());
        };

        match update.status {
            OrderStatus::Filled => {
                if let Some(pending) = self.release_pending_order(client_order_id) {
                    let filled_shares = if update.filled_qty > 0 {
                        update.filled_qty.min(pending.shares)
                    } else {
                        pending.shares
                    };
                    let fill_price = update.avg_fill_price.unwrap_or(pending.limit_price);
                    self.core.record_trade_at(
                        &pending.token_id,
                        fill_price * Decimal::from(filled_shares),
                        update.timestamp,
                    );
                    self.update_position_from_fill(
                        &pending,
                        filled_shares,
                        fill_price,
                        update.timestamp,
                    );
                }
            }
            OrderStatus::PartiallyFilled => {
                if let Some(pending) = self.pending_orders.get(client_order_id).cloned() {
                    let filled_shares = if update.filled_qty > 0 {
                        update.filled_qty.min(pending.shares)
                    } else {
                        pending.shares
                    };
                    let fill_price = update.avg_fill_price.unwrap_or(pending.limit_price);
                    self.update_position_from_fill(
                        &pending,
                        filled_shares,
                        fill_price,
                        update.timestamp,
                    );
                }
            }
            OrderStatus::Rejected
            | OrderStatus::Cancelled
            | OrderStatus::Expired
            | OrderStatus::Failed => {
                self.release_pending_order(client_order_id);
                self.last_error = update.error.clone();
            }
            OrderStatus::Pending | OrderStatus::Submitted => {}
        }

        Ok(Vec::new())
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        if !self.enabled {
            return Ok(Vec::new());
        }

        self.core.reset_daily_if_needed_at(now);

        let event_ids = self.resolve_target_event_ids().await?;
        if event_ids.is_empty() {
            self.last_scan_at = Some(now);
            return Ok(Vec::new());
        }

        let snapshot = match self.data_source.fetch_snapshot().await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                let message = format!("event_edge data fetch failed: {}", err);
                self.last_error = Some(message.clone());
                self.last_scan_at = Some(now);
                return Ok(vec![StrategyAction::Alert {
                    level: AlertLevel::Warning,
                    message,
                }]);
            }
        };

        if !self
            .data_source
            .has_changed(&snapshot, &self.core.state.last_arena_updated)
        {
            self.last_scan_at = Some(now);
            return Ok(Vec::new());
        }

        let Some(arena) = snapshot.arena.clone() else {
            self.last_scan_at = Some(now);
            return Ok(Vec::new());
        };

        let mut actions = Vec::new();
        for event_id in event_ids {
            match self.core.scan_event(&event_id, Some(arena.clone())).await {
                Ok(scan) => actions.extend(self.build_actions_for_scan(&scan, now)),
                Err(err) => {
                    let message = format!("event_edge scan failed for {}: {}", event_id, err);
                    self.last_error = Some(message.clone());
                    actions.push(StrategyAction::Alert {
                        level: AlertLevel::Warning,
                        message,
                    });
                }
            }
        }

        self.core.state.last_arena_updated = snapshot.last_updated;
        self.last_scan_at = Some(now);
        Ok(actions)
    }

    fn state(&self) -> StrategyStateInfo {
        let total_exposure: Decimal = self
            .positions
            .values()
            .map(|position| position.entry_price * Decimal::from(position.shares))
            .sum();
        let unrealized_pnl: Decimal = self.positions.values().map(|p| p.unrealized_pnl).sum();
        let active = self.is_active();

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: if !self.enabled {
                "disabled".to_string()
            } else if active {
                "active".to_string()
            } else {
                "idle".to_string()
            },
            enabled: self.enabled,
            active,
            position_count: self.positions.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure,
            unrealized_pnl,
            realized_pnl_today: Decimal::ZERO,
            last_update: self.last_scan_at.unwrap_or_else(Utc::now),
            metrics: self.state_metrics(),
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
        Ok(vec![StrategyAction::Alert {
            level: AlertLevel::Info,
            message: format!("{} shutdown (dry_run={})", self.id, self.dry_run),
        }])
    }

    fn reset(&mut self) {
        self.discovered_events.clear();
        self.pending_orders.clear();
        self.positions.clear();
        self.reserved_notional_usd = Decimal::ZERO;
        self.last_scan_at = None;
        self.last_signal_event_id = None;
        self.last_error = None;
        self.core.state = EventEdgeState::default();
    }
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn string_list_from_toml(config: &toml::Value, key: &str) -> Option<Vec<String>> {
    config.get(key).and_then(|value| {
        value.as_array().map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(|raw| raw.trim().to_string()))
                .filter(|raw| !raw.is_empty())
                .collect::<Vec<_>>()
        })
    })
}

fn decimal_from_toml(config: &toml::Value, key: &str) -> Option<Decimal> {
    let value = config.get(key)?;
    if let Some(raw) = value.as_float() {
        Decimal::try_from(raw).ok()
    } else if let Some(raw) = value.as_integer() {
        Some(Decimal::from(raw))
    } else if let Some(raw) = value.as_str() {
        raw.parse::<Decimal>().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::event_edge::{EdgeRow, EventEdgeScan};
    use crate::strategy::multi_outcome::ExpectedValue;
    use chrono::Duration;
    use rust_decimal_macros::dec;

    fn dry_run_client() -> PolymarketClient {
        PolymarketClient::new(DEFAULT_PM_REST_URL, true).expect("dry-run client")
    }

    #[test]
    fn from_toml_builds_event_edge_strategy_and_overrides_config() {
        let toml = r#"
[strategy]
name = "event_edge"

[event_edge]
event_ids = ["event-1"]
titles = ["Who wins?"]
interval_secs = 45
min_edge = 0.12
max_entry = 0.63
shares = 55
trade = true
cooldown_secs = 900
max_daily_spend_usd = 125
"#;

        let strategy =
            EventEdgeStrategy::from_toml("ee-test".to_string(), toml, true).expect("strategy");

        assert_eq!(strategy.name(), "event_edge");
        assert!(matches!(
            strategy.required_feeds().as_slice(),
            [DataFeed::Tick {
                interval_ms: 45_000
            }]
        ));
        assert_eq!(strategy.core.cfg.event_ids, vec!["event-1"]);
        assert_eq!(strategy.core.cfg.titles, vec!["Who wins?"]);
        assert_eq!(strategy.core.cfg.min_edge, dec!(0.12));
        assert_eq!(strategy.core.cfg.max_entry, dec!(0.63));
        assert_eq!(strategy.core.cfg.shares, 55);
        assert!(strategy.core.cfg.trade);
        assert_eq!(strategy.core.cfg.cooldown_secs, 900);
        assert_eq!(strategy.core.cfg.max_daily_spend_usd, dec!(125));
    }

    #[test]
    fn from_toml_rejects_non_event_edge_strategy_name() {
        let toml = r#"
[strategy]
name = "momentum"

[event_edge]
event_ids = ["event-1"]
"#;

        let err = EventEdgeStrategy::from_toml("ee-test".to_string(), toml, true)
            .err()
            .expect("wrong strategy name should fail");
        assert!(err.to_string().contains("event_edge"));
    }

    #[tokio::test]
    async fn on_market_update_tracks_discovered_events_and_expiry() {
        let core = EventEdgeCore::new(dry_run_client(), EventEdgeAgentConfig::default());
        let mut strategy = EventEdgeStrategy::new("ee-test".to_string(), core, true)
            .with_data_source(Box::new(ArenaTextSource::default()));

        strategy
            .on_market_update(&MarketUpdate::EventDiscovered {
                event_id: "event-1".to_string(),
                series_id: "series-1".to_string(),
                up_token: "token-up".to_string(),
                down_token: "token-down".to_string(),
                end_time: Utc::now() + Duration::hours(2),
                price_to_beat: Some(dec!(1)),
                title: Some("event".to_string()),
                condition_id: Some("cond-1".to_string()),
            })
            .await
            .expect("discovered update");

        assert_eq!(strategy.discovered_events.len(), 1);
        let tracked = strategy
            .state()
            .metrics
            .get("tracked_events")
            .cloned()
            .expect("tracked_events metric");
        assert_eq!(tracked, "1");

        strategy
            .on_market_update(&MarketUpdate::EventExpired {
                event_id: "event-1".to_string(),
            })
            .await
            .expect("expired update");

        assert!(strategy.discovered_events.is_empty());
        let tracked = strategy
            .state()
            .metrics
            .get("tracked_events")
            .cloned()
            .expect("tracked_events metric");
        assert_eq!(tracked, "0");
    }

    #[tokio::test]
    async fn emits_canonical_submit_order_and_tracks_fill_into_position() {
        let core = EventEdgeCore::new(
            dry_run_client(),
            EventEdgeAgentConfig {
                enabled: true,
                trade: true,
                shares: 25,
                ..EventEdgeAgentConfig::default()
            },
        );
        let mut strategy = EventEdgeStrategy::new("ee-test".to_string(), core, true);

        let scan = EventEdgeScan {
            event_id: "event-1".to_string(),
            event_title: "Test event".to_string(),
            end_time: Utc::now() + Duration::hours(6),
            confidence: 0.95,
            arena_last_updated: None,
            arena_staleness_days: None,
            rows: vec![EdgeRow {
                outcome: "OpenAI".to_string(),
                yes_token_id: "token-up".to_string(),
                condition_id: Some("cond-1".to_string()),
                market_ask: Some(dec!(0.40)),
                market_mid: Some(dec!(0.39)),
                p_true: dec!(0.67),
                edge: Some(dec!(0.27)),
                ev: Some(ExpectedValue::calculate(dec!(0.40), dec!(0.67), None)),
            }],
        };

        let now = Utc::now();
        let actions = strategy.build_actions_for_scan_for_test(&scan, now);

        assert!(actions.iter().any(|action| matches!(
            action,
            StrategyAction::LogEvent { event }
                if matches!(event.event_type, StrategyEventType::SignalDetected)
        )));

        let client_order_id = actions
            .iter()
            .find_map(|action| match action {
                StrategyAction::SubmitOrder {
                    client_order_id,
                    order,
                    ..
                } => {
                    assert_eq!(order.client_order_id, *client_order_id);
                    assert_eq!(
                        order.idempotency_key.as_deref(),
                        Some(client_order_id.as_str())
                    );
                    Some(client_order_id.clone())
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
                avg_fill_price: Some(dec!(0.41)),
                timestamp: now,
                error: None,
            })
            .await
            .expect("fill update");

        assert!(strategy.pending_orders.is_empty());
        let positions = strategy.positions();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].token_id, "token-up");
        assert_eq!(positions[0].entry_price, dec!(0.41));
        assert_eq!(positions[0].shares, 25);
        assert_eq!(
            positions[0].metadata.get("event_id"),
            Some(&"event-1".to_string())
        );
        assert_eq!(
            positions[0].metadata.get("condition_id"),
            Some(&"cond-1".to_string())
        );
    }
}
