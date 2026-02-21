//! Shared state and decision logic for EventEdge strategies.
//!
//! `EventEdgeCore` owns the scan→filter→cooldown→spend-cap pipeline that was
//! previously duplicated across `event_edge_agent`, `event_edge_event_driven`,
//! and `event_edge_claude_framework`.

use crate::adapters::PolymarketClient;
use crate::config::EventEdgeAgentConfig;
use crate::error::Result;
use crate::strategy::event_edge::{
    discover_best_event_id_by_title, scan_event_edge_once, EdgeRow, EventEdgeScan,
};
use crate::strategy::event_models::arena_text::ArenaTextSnapshot;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use tracing::{info, warn};

/// A single actionable trade opportunity produced by `scan_and_decide`.
#[derive(Debug, Clone)]
pub struct TradeDecision {
    pub event_id: String,
    pub outcome: String,
    pub token_id: String,
    pub condition_id: Option<String>,
    pub market_slug: String,
    pub side: crate::domain::Side,
    pub shares: u64,
    pub limit_price: Decimal,
    pub edge: Decimal,
    pub p_true: Decimal,
    pub net_ev: Decimal,
}

/// Mutable state shared across scan cycles.
#[derive(Debug, Clone)]
pub struct EventEdgeState {
    pub last_trade_at: HashMap<String, DateTime<Utc>>,
    pub daily_spend_usd: Decimal,
    pub daily_spend_day: NaiveDate,
    pub last_arena_updated: Option<NaiveDate>,
    pub resolved_event_ids: Vec<String>,
}

impl Default for EventEdgeState {
    fn default() -> Self {
        Self {
            last_trade_at: HashMap::new(),
            daily_spend_usd: Decimal::ZERO,
            daily_spend_day: Utc::now().date_naive(),
            last_arena_updated: None,
            resolved_event_ids: Vec::new(),
        }
    }
}

/// Core logic shared by all EventEdge execution modes.
pub struct EventEdgeCore {
    pub client: PolymarketClient,
    pub cfg: EventEdgeAgentConfig,
    pub state: EventEdgeState,
}

impl EventEdgeCore {
    pub fn new(client: PolymarketClient, cfg: EventEdgeAgentConfig) -> Self {
        Self {
            client,
            cfg,
            state: EventEdgeState::default(),
        }
    }

    pub fn with_state(
        client: PolymarketClient,
        cfg: EventEdgeAgentConfig,
        state: EventEdgeState,
    ) -> Self {
        Self { client, cfg, state }
    }

    // ── Guards ───────────────────────────────────────────────────────

    pub fn reset_daily_if_needed(&mut self) {
        let today = Utc::now().date_naive();
        if today != self.state.daily_spend_day {
            self.state.daily_spend_day = today;
            self.state.daily_spend_usd = Decimal::ZERO;
        }
    }

    pub fn is_on_cooldown(&self, token_id: &str) -> bool {
        let now = Utc::now();
        if let Some(last) = self.state.last_trade_at.get(token_id) {
            (now - *last).num_seconds() < self.cfg.cooldown_secs as i64
        } else {
            false
        }
    }

    pub fn can_spend(&self, amount: Decimal) -> bool {
        self.state.daily_spend_usd + amount <= self.cfg.max_daily_spend_usd
    }

    pub fn daily_cap_reached(&self) -> bool {
        self.state.daily_spend_usd >= self.cfg.max_daily_spend_usd
    }

    pub fn record_trade(&mut self, token_id: &str, spend: Decimal) {
        self.state
            .last_trade_at
            .insert(token_id.to_string(), Utc::now());
        self.state.daily_spend_usd += spend;
    }

    pub fn targets_empty(&self) -> bool {
        self.cfg.event_ids.is_empty() && self.cfg.titles.is_empty()
    }

    // ── Event ID resolution ──────────────────────────────────────────

    pub async fn resolve_event_ids(&mut self) -> Result<Vec<String>> {
        let mut event_ids: Vec<String> = self.cfg.event_ids.clone();
        for title in &self.cfg.titles {
            match discover_best_event_id_by_title(title).await {
                Ok(id) => event_ids.push(id),
                Err(e) => warn!(
                    "EventEdgeCore: title discovery failed (\"{}\"): {}",
                    title, e
                ),
            }
        }
        event_ids.sort();
        event_ids.dedup();
        self.state.resolved_event_ids = event_ids.clone();
        Ok(event_ids)
    }

    /// Resolve event IDs by merging registry (monitoring) events with TOML config.
    /// Falls back to config-only if store is None.
    pub async fn resolve_event_ids_with_registry(
        &mut self,
        store: Option<&crate::adapters::postgres::PostgresStore>,
    ) -> Result<Vec<String>> {
        let mut event_ids: Vec<String> = Vec::new();

        // 1. Pull from registry: status=monitoring, strategy_hint=event_edge
        if let Some(store) = store {
            match store.get_monitoring_events("event_edge").await {
                Ok(events) => {
                    let count = events.len();
                    for ev in events {
                        if let Some(eid) = ev.event_id {
                            event_ids.push(eid);
                        }
                    }
                    if count > 0 {
                        info!(
                            "EventEdgeCore: loaded {} monitoring events from registry ({} with event_id)",
                            count,
                            event_ids.len()
                        );
                    }
                }
                Err(e) => {
                    warn!("EventEdgeCore: registry query failed, falling back to config: {e}")
                }
            }
        }

        // 2. Merge with TOML config event_ids
        event_ids.extend(self.cfg.event_ids.clone());

        // 3. Resolve TOML titles
        for title in &self.cfg.titles {
            match discover_best_event_id_by_title(title).await {
                Ok(id) => event_ids.push(id),
                Err(e) => warn!(
                    "EventEdgeCore: title discovery failed (\"{}\"): {}",
                    title, e
                ),
            }
        }

        event_ids.sort();
        event_ids.dedup();
        self.state.resolved_event_ids = event_ids.clone();
        Ok(event_ids)
    }

    // ── Scan + decide ────────────────────────────────────────────────

    /// Scan a single event and return filtered trade decisions.
    pub async fn scan_event(
        &self,
        event_id: &str,
        arena: Option<ArenaTextSnapshot>,
    ) -> Result<EventEdgeScan> {
        scan_event_edge_once(&self.client, event_id, arena).await
    }

    /// Run the full scan→filter pipeline for one event, returning at most
    /// one `TradeDecision` (best +EV row that clears all thresholds).
    pub async fn scan_and_decide(
        &mut self,
        event_id: &str,
        arena: Option<ArenaTextSnapshot>,
    ) -> Result<Option<TradeDecision>> {
        self.reset_daily_if_needed();

        let scan = self.scan_event(event_id, arena).await?;

        if !self.cfg.trade {
            return Ok(None);
        }
        if self.daily_cap_reached() {
            warn!(
                "EventEdgeCore: daily spend cap reached (${:.2} >= ${:.2}); skipping",
                self.state.daily_spend_usd, self.cfg.max_daily_spend_usd
            );
            return Ok(None);
        }

        Ok(self.pick_best_trade(&scan))
    }

    /// Filter scan rows by edge/entry/cooldown/spend and return the best one.
    pub fn pick_best_trade(&self, scan: &EventEdgeScan) -> Option<TradeDecision> {
        for r in &scan.rows {
            if let Some(decision) = self.evaluate_row(r, scan) {
                return Some(decision);
            }
        }
        None
    }

    fn evaluate_row(&self, r: &EdgeRow, scan: &EventEdgeScan) -> Option<TradeDecision> {
        let ask = r.market_ask?;
        let edge = r.edge?;
        let ev = r.ev.as_ref()?;

        if ask > self.cfg.max_entry {
            return None;
        }
        if edge < self.cfg.min_edge {
            return None;
        }
        if !ev.is_positive_ev {
            return None;
        }
        if self.is_on_cooldown(&r.yes_token_id) {
            return None;
        }

        let notional = Decimal::from(self.cfg.shares) * ask;
        if !self.can_spend(notional) {
            warn!(
                "EventEdgeCore: would exceed daily cap (spent ${:.2} + ${:.2} > ${:.2}); skipping",
                self.state.daily_spend_usd, notional, self.cfg.max_daily_spend_usd
            );
            return None;
        }

        Some(TradeDecision {
            event_id: scan.event_id.clone(),
            outcome: r.outcome.clone(),
            token_id: r.yes_token_id.clone(),
            condition_id: r.condition_id.clone(),
            // Event-level strategies may not expose per-outcome market slugs directly.
            // Use a stable non-empty market key for routing/risk/position grouping.
            market_slug: scan.event_id.clone(),
            side: crate::domain::Side::Up,
            shares: self.cfg.shares,
            limit_price: ask,
            edge,
            p_true: r.p_true,
            net_ev: ev.net_ev,
        })
    }
}
