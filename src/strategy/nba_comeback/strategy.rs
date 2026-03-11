use crate::ai_clients::PolymarketSportsClient;
use crate::domain::{OrderStatus, Side};
use crate::error::Result;
use crate::strategy::nba_comeback::core::{ComebackOpportunity, NbaComebackCore, NbaComebackState};
use crate::strategy::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyEvent, StrategyEventType, StrategyStateInfo,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;

mod config_loader;
mod opportunity_flow;
#[cfg(test)]
mod tests;

const NBA_COMEBACK_STRATEGY_NAME: &str = "nba_comeback";
const NBA_COMEBACK_PRIORITY: u8 = 8;

#[derive(Debug, Clone)]
pub struct NbaComebackMarketRegistration {
    pub game_id: Option<String>,
    pub market_slug: String,
    pub condition_id: Option<String>,
    pub home_team: String,
    pub away_team: String,
    pub home_abbrev: String,
    pub away_abbrev: String,
    pub home_token_id: String,
    pub away_token_id: String,
    pub home_price: Decimal,
    pub away_price: Decimal,
}

#[derive(Debug, Clone)]
struct PendingNbaComebackOrder {
    client_order_id: String,
    game_id: String,
    trailing_abbrev: String,
    token_id: String,
    market_slug: String,
    condition_id: Option<String>,
    requested_shares: u64,
    accounted_filled_shares: u64,
    limit_price: Decimal,
    reserved_notional_usd: Decimal,
}

pub type NbaComebackAdapter = NbaComebackStrategy;

pub struct NbaComebackStrategy {
    id: String,
    dry_run: bool,
    enabled: bool,
    core: NbaComebackCore,
    pm_sports: Option<PolymarketSportsClient>,
    market_registrations: Vec<NbaComebackMarketRegistration>,
    pending_orders: HashMap<String, PendingNbaComebackOrder>,
    positions: HashMap<String, PositionInfo>,
    reserved_notional_usd: Decimal,
    last_scan_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    stats_loaded: bool,
}

impl std::fmt::Debug for NbaComebackStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NbaComebackStrategy")
            .field("id", &self.id)
            .field("dry_run", &self.dry_run)
            .field("enabled", &self.enabled)
            .field("market_registrations", &self.market_registrations.len())
            .field("pending_orders", &self.pending_orders.len())
            .field("positions", &self.positions.len())
            .field("stats_loaded", &self.stats_loaded)
            .finish()
    }
}

impl NbaComebackStrategy {
    pub fn new(id: String, core: NbaComebackCore, dry_run: bool) -> Self {
        let enabled = core.cfg.enabled;
        Self {
            id,
            dry_run,
            enabled,
            core,
            pm_sports: PolymarketSportsClient::new().ok(),
            market_registrations: Vec::new(),
            pending_orders: HashMap::new(),
            positions: HashMap::new(),
            reserved_notional_usd: Decimal::ZERO,
            last_scan_at: None,
            last_error: None,
            stats_loaded: false,
        }
    }

    pub fn with_market_registration(mut self, registration: NbaComebackMarketRegistration) -> Self {
        self.market_registrations.push(registration);
        self
    }

    pub fn build_actions_for_opportunity_for_test(
        &mut self,
        opp: &ComebackOpportunity,
        condition_id: Option<String>,
        now: DateTime<Utc>,
    ) -> Vec<StrategyAction> {
        self.build_actions_for_opportunity_inner(opp, condition_id, now)
    }
}

#[async_trait]
impl Strategy for NbaComebackStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        NBA_COMEBACK_STRATEGY_NAME
    }

    fn description(&self) -> &str {
        "NBA comeback strategy using ESPN Q3/Q4 game state"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![DataFeed::Tick {
            interval_ms: self.core.cfg.espn_poll_interval_secs.saturating_mul(1000),
        }]
    }

    async fn on_market_update(&mut self, _update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        Ok(Vec::new())
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        let Some(client_order_id) = update.client_order_id.as_deref() else {
            return Ok(Vec::new());
        };
        let Some(pending) = self.pending_orders.get_mut(client_order_id) else {
            return Ok(Vec::new());
        };

        let mut actions = Vec::new();
        if update.filled_qty > pending.accounted_filled_shares {
            let delta = update.filled_qty - pending.accounted_filled_shares;
            pending.accounted_filled_shares = update.filled_qty;
            let fill_price = update.avg_fill_price.unwrap_or(pending.limit_price);

            self.core.record_initial_entry_submission(
                &pending.game_id,
                &pending.token_id,
                fill_price * Decimal::from(delta),
            );
            self.core.record_position_entry_with_market_and_team(
                &pending.game_id,
                &pending.trailing_abbrev,
                &pending.market_slug,
                &pending.token_id,
                fill_price,
                delta,
                0.0,
            );

            let position = self
                .positions
                .entry(pending.token_id.clone())
                .or_insert_with(|| {
                    let mut info = PositionInfo::new(
                        pending.token_id.clone(),
                        Side::Up,
                        0,
                        fill_price,
                        self.id.clone(),
                    );
                    info.metadata
                        .insert("game_id".to_string(), pending.game_id.clone());
                    info.metadata
                        .insert("trailing_team".to_string(), pending.trailing_abbrev.clone());
                    info.metadata
                        .insert("market_slug".to_string(), pending.market_slug.clone());
                    if let Some(condition_id) = pending.condition_id.clone() {
                        info.metadata
                            .insert("condition_id".to_string(), condition_id);
                    }
                    info
                });
            let total_cost = position.entry_price * Decimal::from(position.shares)
                + fill_price * Decimal::from(delta);
            position.shares += delta;
            if position.shares > 0 {
                position.entry_price = total_cost / Decimal::from(position.shares);
            }
            position.current_price = Some(fill_price);

            actions.push(StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::OrderFilled,
                    format!(
                        "nba_comeback fill game={} token={} shares={} price={}",
                        pending.game_id, pending.token_id, delta, fill_price
                    ),
                )
                .with_data("game_id", &pending.game_id)
                .with_data("token_id", &pending.token_id)
                .with_data("filled_qty", delta.to_string())
                .with_data("fill_price", fill_price.to_string()),
            });
        }

        if matches!(
            update.status,
            OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Expired
                | OrderStatus::Failed
        ) {
            self.release_pending_order(client_order_id);
        }

        Ok(actions)
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        if !self.enabled {
            return Ok(Vec::new());
        }

        match self.collect_opportunities().await {
            Ok(opps) => {
                self.last_error = None;
                let Some((best, condition_id)) = opps
                    .iter()
                    .max_by(|(a, _), (b, _)| {
                        a.edge
                            .partial_cmp(&b.edge)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .cloned()
                else {
                    self.last_scan_at = Some(now);
                    return Ok(Vec::new());
                };

                Ok(self.build_actions_for_opportunity_inner(&best, condition_id, now))
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                Ok(vec![StrategyAction::Alert {
                    level: AlertLevel::Warning,
                    message: format!("{} scan failed: {}", self.id, error),
                }])
            }
        }
    }

    fn state(&self) -> StrategyStateInfo {
        let mut metrics = HashMap::new();
        metrics.insert(
            "tracked_markets".to_string(),
            self.market_registrations.len().to_string(),
        );
        metrics.insert(
            "reserved_notional_usd".to_string(),
            self.reserved_notional_usd.to_string(),
        );
        metrics.insert("stats_loaded".to_string(), self.stats_loaded.to_string());
        if let Some(last_scan_at) = self.last_scan_at {
            metrics.insert("last_scan_at".to_string(), last_scan_at.to_rfc3339());
        }
        if let Some(last_error) = self.last_error.as_ref() {
            metrics.insert("last_error".to_string(), last_error.clone());
        }

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: if self.enabled {
                "running".to_string()
            } else {
                "disabled".to_string()
            },
            enabled: self.enabled,
            active: !self.positions.is_empty() || !self.pending_orders.is_empty(),
            position_count: self.positions.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure: self
                .positions
                .values()
                .map(|position| position.entry_price * Decimal::from(position.shares))
                .sum(),
            unrealized_pnl: self
                .positions
                .values()
                .map(|position| position.unrealized_pnl)
                .sum(),
            realized_pnl_today: self.core.state.daily_realized_pnl_usd,
            last_update: self.last_scan_at.unwrap_or_else(Utc::now),
            metrics,
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.positions.values().cloned().collect()
    }

    fn is_active(&self) -> bool {
        self.enabled && (!self.positions.is_empty() || !self.pending_orders.is_empty())
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.enabled = false;
        Ok(vec![StrategyAction::Alert {
            level: AlertLevel::Info,
            message: format!("{} shutdown (dry_run={})", self.id, self.dry_run),
        }])
    }

    fn reset(&mut self) {
        self.pending_orders.clear();
        self.positions.clear();
        self.reserved_notional_usd = Decimal::ZERO;
        self.last_scan_at = None;
        self.last_error = None;
        self.stats_loaded = false;
        self.core.state = NbaComebackState::default();
    }
}
