use crate::ai_clients::PolymarketSportsClient;
use crate::error::Result;
use crate::strategy::nba_comeback::core::{ComebackOpportunity, NbaComebackCore};
use crate::strategy::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyStateInfo,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;

mod config_loader;
mod opportunity_flow;
mod state_flow;
#[cfg(test)]
mod tests;

const NBA_COMEBACK_STRATEGY_NAME: &str = "nba_comeback";
const NBA_COMEBACK_PRIORITY: u8 = 8;

#[cfg(test)]
pub(crate) use config_loader::default_nba_comeback_config;

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
        self.handle_order_update(update)
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
        self.build_state_info()
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.tracked_positions()
    }

    fn is_active(&self) -> bool {
        self.runtime_active()
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        Ok(self.shutdown_actions())
    }

    fn reset(&mut self) {
        self.reset_runtime_state();
    }
}
