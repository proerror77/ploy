//! Two-Leg Arbitrage Strategy
//!
//! Implements two-leg arbitrage on prediction markets:
//! 1. Detect price dump on one side
//! 2. Buy the dumped side (Leg1)
//! 3. Wait for combined price opportunity
//! 4. Buy opposite side (Leg2) to lock in arbitrage

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, error, info, warn};

use crate::domain::{OrderRequest, OrderStatus, Quote, Side};
use crate::error::Result;

use crate::strategy::detectors::{DumpDetector, DumpDetectorConfig, DumpSignal};
use crate::strategy::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, RiskLevel, Strategy,
    StrategyAction, StrategyConfig, StrategyEvent, StrategyEventType, StrategyStateInfo,
};

/// Two-leg strategy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwoLegConfig {
    /// Strategy ID
    pub id: String,
    /// Is strategy enabled
    pub enabled: bool,
    /// Number of shares per trade
    pub shares: u64,
    /// Maximum position size
    pub max_position_size: u64,
    /// Maximum exposure (USD)
    pub max_exposure: Decimal,
    /// Dry run mode
    pub dry_run: bool,
    /// Watch window in minutes
    pub window_min: u64,
    /// Minimum time remaining to enter (seconds)
    pub min_time_remaining_secs: u64,
    /// Dump detector configuration
    pub dump_config: DumpDetectorConfig,
}

impl Default for TwoLegConfig {
    fn default() -> Self {
        Self {
            id: "two-leg".to_string(),
            enabled: true,
            shares: 20,
            max_position_size: 100,
            max_exposure: Decimal::from(100),
            dry_run: true,
            window_min: 2,
            min_time_remaining_secs: 30,
            dump_config: DumpDetectorConfig::default(),
        }
    }
}

/// Strategy state machine states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TwoLegState {
    /// Waiting for event start
    Idle,
    /// Watching for dump signal
    WatchWindow,
    /// Leg1 order pending
    Leg1Pending,
    /// Leg1 filled, waiting for Leg2 opportunity
    Leg1Filled,
    /// Leg2 order pending
    Leg2Pending,
    /// Cycle complete
    CycleComplete,
    /// Cycle aborted
    Abort,
}

impl Default for TwoLegState {
    fn default() -> Self {
        TwoLegState::Idle
    }
}

impl std::fmt::Display for TwoLegState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TwoLegState::Idle => write!(f, "IDLE"),
            TwoLegState::WatchWindow => write!(f, "WATCH_WINDOW"),
            TwoLegState::Leg1Pending => write!(f, "LEG1_PENDING"),
            TwoLegState::Leg1Filled => write!(f, "LEG1_FILLED"),
            TwoLegState::Leg2Pending => write!(f, "LEG2_PENDING"),
            TwoLegState::CycleComplete => write!(f, "CYCLE_COMPLETE"),
            TwoLegState::Abort => write!(f, "ABORT"),
        }
    }
}

/// Active event context
#[derive(Debug, Clone)]
struct EventContext {
    event_id: String,
    up_token_id: String,
    down_token_id: String,
    end_time: DateTime<Utc>,
    start_time: DateTime<Utc>,
}

/// Active cycle context
#[derive(Debug, Clone)]
struct CycleContext {
    leg1_side: Side,
    leg1_price: Decimal,
    leg1_shares: u64,
    leg1_order_id: String,
    leg2_order_id: Option<String>,
}

/// Two-leg arbitrage strategy
pub struct TwoLegStrategy {
    config: TwoLegConfig,
    state: TwoLegState,
    detector: DumpDetector,
    current_event: Option<EventContext>,
    current_cycle: Option<CycleContext>,
    pending_orders: HashMap<String, PendingOrder>,
    positions: Vec<PositionInfo>,
    last_up_quote: Option<Quote>,
    last_down_quote: Option<Quote>,
    realized_pnl: Decimal,
}

#[derive(Debug, Clone)]
struct PendingOrder {
    client_order_id: String,
    order_id: Option<String>,
    side: Side,
    is_leg1: bool,
}

impl TwoLegStrategy {
    /// Create a new two-leg strategy
    pub fn new(config: TwoLegConfig) -> Self {
        let detector = DumpDetector::new(config.dump_config.clone());

        Self {
            config,
            state: TwoLegState::Idle,
            detector,
            current_event: None,
            current_cycle: None,
            pending_orders: HashMap::new(),
            positions: Vec::new(),
            last_up_quote: None,
            last_down_quote: None,
            realized_pnl: Decimal::ZERO,
        }
    }

    /// Get token ID for a side
    fn token_id(&self, side: Side) -> Option<&str> {
        self.current_event.as_ref().map(|e| match side {
            Side::Up => e.up_token_id.as_str(),
            Side::Down => e.down_token_id.as_str(),
        })
    }

    /// Get seconds remaining in current event
    fn seconds_remaining(&self) -> Option<i64> {
        self.current_event
            .as_ref()
            .map(|e| (e.end_time - Utc::now()).num_seconds().max(0))
    }

    /// Get minutes elapsed since event start
    fn minutes_elapsed(&self) -> Option<i64> {
        self.current_event
            .as_ref()
            .map(|e| (Utc::now() - e.start_time).num_minutes())
    }

    /// Check if in an active cycle
    fn is_in_cycle(&self) -> bool {
        matches!(
            self.state,
            TwoLegState::Leg1Pending
                | TwoLegState::Leg1Filled
                | TwoLegState::Leg2Pending
        )
    }

    /// Process dump signal
    fn handle_dump_signal(&mut self, signal: DumpSignal) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        // Validate signal
        if !signal.spread_ok(self.config.dump_config.max_spread_bps) {
            debug!(
                "Signal rejected: spread {} > max {}",
                signal.spread_bps, self.config.dump_config.max_spread_bps
            );
            return actions;
        }

        // Check time remaining
        if let Some(remaining) = self.seconds_remaining() {
            if remaining < self.config.min_time_remaining_secs as i64 {
                debug!("Signal rejected: only {}s remaining", remaining);
                return actions;
            }
        }

        // Get token ID
        let Some(token_id) = self.token_id(signal.side) else {
            return actions;
        };

        // Create Leg1 order
        let client_order_id = format!("{}-leg1-{}", self.config.id, Utc::now().timestamp_millis());

        let order = OrderRequest::buy_limit(
            token_id.to_string(),
            signal.side,
            self.config.shares,
            signal.trigger_price,
        );

        // Track pending order
        self.pending_orders.insert(
            client_order_id.clone(),
            PendingOrder {
                client_order_id: client_order_id.clone(),
                order_id: None,
                side: signal.side,
                is_leg1: true,
            },
        );

        // Update state
        self.state = TwoLegState::Leg1Pending;

        info!(
            "Entering Leg1: {} {} shares @ {}",
            signal.side, self.config.shares, signal.trigger_price
        );

        actions.push(StrategyAction::SubmitOrder {
            client_order_id,
            order,
            priority: 10,
        });

        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(StrategyEventType::EntryTriggered, "Leg1 entry triggered")
                .with_data("side", signal.side.to_string())
                .with_data("price", signal.trigger_price.to_string())
                .with_data("drop_pct", (signal.drop_pct * Decimal::from(100)).to_string()),
        });

        actions
    }

    /// Check for Leg2 opportunity
    fn check_leg2_opportunity(&self) -> Option<(Side, Decimal)> {
        let ctx = self.current_cycle.as_ref()?;
        let opposite_side = ctx.leg1_side.opposite();

        // Get opposite side quote
        let opposite_quote = match opposite_side {
            Side::Up => self.last_up_quote.as_ref(),
            Side::Down => self.last_down_quote.as_ref(),
        };

        let ask = opposite_quote?.best_ask?;

        // Check sum condition
        if self.detector.check_leg2_condition(ctx.leg1_price, ask) {
            Some((opposite_side, ask))
        } else {
            None
        }
    }

    /// Enter Leg2
    fn enter_leg2(&mut self, side: Side, price: Decimal) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        let Some(ctx) = &self.current_cycle else {
            return actions;
        };

        let Some(token_id) = self.token_id(side) else {
            return actions;
        };

        let client_order_id = format!("{}-leg2-{}", self.config.id, Utc::now().timestamp_millis());

        let order = OrderRequest::buy_limit(
            token_id.to_string(),
            side,
            ctx.leg1_shares,
            price,
        );

        self.pending_orders.insert(
            client_order_id.clone(),
            PendingOrder {
                client_order_id: client_order_id.clone(),
                order_id: None,
                side,
                is_leg1: false,
            },
        );

        self.state = TwoLegState::Leg2Pending;

        info!("Entering Leg2: {} {} shares @ {}", side, ctx.leg1_shares, price);

        actions.push(StrategyAction::SubmitOrder {
            client_order_id,
            order,
            priority: 10,
        });

        actions
    }

    /// Force Leg2 or abort
    fn force_leg2_or_abort(&mut self) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        let Some(ctx) = &self.current_cycle else {
            return actions;
        };

        let opposite_side = ctx.leg1_side.opposite();

        // Try to get opposite side quote with slippage
        let opposite_quote = match opposite_side {
            Side::Up => self.last_up_quote.as_ref(),
            Side::Down => self.last_down_quote.as_ref(),
        };

        if let Some(quote) = opposite_quote {
            if let Some(ask) = quote.best_ask {
                // Use higher price with slippage
                let forced_price = ask * (Decimal::ONE + self.config.dump_config.slippage_buffer);
                warn!("Forcing Leg2 at {}", forced_price);
                return self.enter_leg2(opposite_side, forced_price);
            }
        }

        // Can't force, must abort
        self.abort_cycle("No quote for forced Leg2");
        actions.push(StrategyAction::Alert {
            level: AlertLevel::Warning,
            message: "Cycle aborted: No quote for forced Leg2".to_string(),
        });

        actions
    }

    /// Abort current cycle
    fn abort_cycle(&mut self, reason: &str) {
        warn!("Aborting cycle: {}", reason);

        self.state = TwoLegState::Abort;
        self.current_cycle = None;
        self.positions.clear();
    }

    /// Transition to idle
    fn transition_to_idle(&mut self) {
        self.state = TwoLegState::Idle;
        self.current_cycle = None;
        self.current_event = None;
        self.positions.clear();
        self.detector.reset(None);
        debug!("Transitioned to IDLE");
    }
}

#[async_trait]
impl Strategy for TwoLegStrategy {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn name(&self) -> &str {
        "Two-Leg Arbitrage"
    }

    fn description(&self) -> &str {
        "Two-leg arbitrage strategy for prediction markets"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![
            DataFeed::Tick { interval_ms: 1000 },
        ]
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        match update {
            MarketUpdate::PolymarketQuote {
                token_id,
                side,
                quote,
                ..
            } => {
                // Update cached quotes
                match side {
                    Side::Up => self.last_up_quote = Some(quote.clone()),
                    Side::Down => self.last_down_quote = Some(quote.clone()),
                }

                // Process based on state
                match self.state {
                    TwoLegState::WatchWindow => {
                        // Check for dump signal
                        let event_id = self.current_event.as_ref().map(|e| e.event_id.as_str());
                        if let Some(signal) = self.detector.update(quote, event_id) {
                            actions.extend(self.handle_dump_signal(signal));
                        }
                    }
                    TwoLegState::Leg1Filled => {
                        // Check for Leg2 opportunity
                        if let Some((opp_side, price)) = self.check_leg2_opportunity() {
                            actions.extend(self.enter_leg2(opp_side, price));
                        }

                        // Check for force close
                        if let Some(remaining) = self.seconds_remaining() {
                            if remaining <= self.config.dump_config.force_close_secs as i64 {
                                actions.extend(self.force_leg2_or_abort());
                            }
                        }
                    }
                    _ => {}
                }
            }
            MarketUpdate::EventDiscovered {
                event_id,
                up_token,
                down_token,
                end_time,
                ..
            } => {
                if self.state == TwoLegState::Idle {
                    // Start monitoring new event
                    self.current_event = Some(EventContext {
                        event_id: event_id.clone(),
                        up_token_id: up_token.clone(),
                        down_token_id: down_token.clone(),
                        end_time: *end_time,
                        start_time: Utc::now(),
                    });

                    self.detector.reset(Some(event_id));
                    self.state = TwoLegState::WatchWindow;

                    info!("Started monitoring event: {}", event_id);

                    // Subscribe to token feeds
                    actions.push(StrategyAction::SubscribeFeed {
                        feed: DataFeed::PolymarketQuotes {
                            tokens: vec![up_token.clone(), down_token.clone()],
                        },
                    });
                }
            }
            MarketUpdate::EventExpired { event_id } => {
                if self
                    .current_event
                    .as_ref()
                    .map(|e| e.event_id == *event_id)
                    .unwrap_or(false)
                {
                    if self.is_in_cycle() {
                        self.abort_cycle("Event expired");
                        actions.push(StrategyAction::UpdateRisk {
                            level: RiskLevel::Elevated,
                            reason: "Cycle aborted due to event expiration".to_string(),
                        });
                    }
                    self.transition_to_idle();
                }
            }
            _ => {}
        }

        Ok(actions)
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        // Find pending order
        let pending = self
            .pending_orders
            .iter()
            .find(|(_, p)| {
                p.order_id.as_ref() == Some(&update.order_id)
                    || p.client_order_id == update.client_order_id.as_deref().unwrap_or("")
            })
            .map(|(k, v)| (k.clone(), v.clone()));

        let Some((client_id, pending)) = pending else {
            return Ok(actions);
        };

        match update.status {
            OrderStatus::Filled => {
                let fill_price = update.avg_fill_price.unwrap_or(Decimal::ZERO);

                if pending.is_leg1 {
                    // Leg1 filled
                    self.current_cycle = Some(CycleContext {
                        leg1_side: pending.side,
                        leg1_price: fill_price,
                        leg1_shares: update.filled_qty,
                        leg1_order_id: update.order_id.clone(),
                        leg2_order_id: None,
                    });

                    self.state = TwoLegState::Leg1Filled;
                    self.detector.mark_triggered(pending.side);

                    // Add position
                    self.positions.push(PositionInfo::new(
                        self.token_id(pending.side).unwrap_or("").to_string(),
                        pending.side,
                        update.filled_qty,
                        fill_price,
                        self.config.id.clone(),
                    ));

                    info!("Leg1 filled: {} shares @ {}", update.filled_qty, fill_price);

                    actions.push(StrategyAction::LogEvent {
                        event: StrategyEvent::new(StrategyEventType::OrderFilled, "Leg1 filled")
                            .with_data("price", fill_price.to_string())
                            .with_data("shares", update.filled_qty.to_string()),
                    });
                } else {
                    // Leg2 filled - cycle complete
                    if let Some(ctx) = &self.current_cycle {
                        // Calculate PnL
                        let gross_pnl = Decimal::from(update.filled_qty)
                            * (Decimal::ONE - ctx.leg1_price - fill_price);
                        let fee_rate = self.config.dump_config.fee_buffer;
                        let fees = Decimal::from(update.filled_qty)
                            * (ctx.leg1_price + fill_price)
                            * fee_rate;
                        let net_pnl = gross_pnl - fees;

                        self.realized_pnl += net_pnl;
                        self.state = TwoLegState::CycleComplete;
                        self.positions.clear();

                        info!(
                            "Cycle complete! Leg2 filled: {} shares @ {}. PnL: {}",
                            update.filled_qty, fill_price, net_pnl
                        );

                        actions.push(StrategyAction::LogEvent {
                            event: StrategyEvent::new(
                                StrategyEventType::CycleCompleted,
                                "Arbitrage cycle completed",
                            )
                            .with_data("pnl", net_pnl.to_string())
                            .with_data("leg1_price", ctx.leg1_price.to_string())
                            .with_data("leg2_price", fill_price.to_string()),
                        });
                    }
                }

                self.pending_orders.remove(&client_id);
            }
            OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired => {
                if pending.is_leg1 {
                    self.abort_cycle(&format!("Leg1 order {}", update.status));
                } else {
                    // Leg2 failed - open exposure
                    error!("Leg2 order failed: {:?}", update.status);
                    actions.push(StrategyAction::Alert {
                        level: AlertLevel::Critical,
                        message: format!("Leg2 order failed: {:?} - OPEN EXPOSURE", update.status),
                    });
                    actions.push(StrategyAction::UpdateRisk {
                        level: RiskLevel::Critical,
                        reason: "Leg2 failed with open position".to_string(),
                    });
                    self.state = TwoLegState::Abort;
                }

                self.pending_orders.remove(&client_id);
            }
            _ => {}
        }

        Ok(actions)
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        // Check for state transitions
        match self.state {
            TwoLegState::WatchWindow => {
                // Check if watch window expired
                if let Some(elapsed) = self.minutes_elapsed() {
                    if elapsed >= self.config.window_min as i64 {
                        info!("Watch window expired after {} minutes", elapsed);
                        self.transition_to_idle();
                    }
                }

                // Check if event ended
                if let Some(remaining) = self.seconds_remaining() {
                    if remaining <= 0 {
                        self.transition_to_idle();
                    }
                }
            }
            TwoLegState::CycleComplete | TwoLegState::Abort => {
                self.transition_to_idle();
            }
            _ => {}
        }

        Ok(actions)
    }

    fn state(&self) -> StrategyStateInfo {
        let exposure = self
            .positions
            .iter()
            .map(|p| {
                p.current_price.unwrap_or(p.entry_price) * Decimal::from(p.shares)
            })
            .sum();

        let unrealized_pnl = self.positions.iter().map(|p| p.unrealized_pnl).sum();

        StrategyStateInfo {
            strategy_id: self.config.id.clone(),
            phase: self.state.to_string(),
            enabled: self.config.enabled,
            active: self.is_in_cycle(),
            position_count: self.positions.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure: exposure,
            unrealized_pnl,
            realized_pnl_today: self.realized_pnl,
            last_update: Utc::now(),
            metrics: HashMap::new(),
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.positions.clone()
    }

    fn is_active(&self) -> bool {
        self.is_in_cycle() || !self.pending_orders.is_empty()
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        // Cancel all pending orders
        for (client_id, _) in &self.pending_orders {
            actions.push(StrategyAction::CancelOrder {
                order_id: client_id.clone(),
            });
        }

        if self.is_in_cycle() {
            self.abort_cycle("Strategy shutdown");
        }

        self.transition_to_idle();

        Ok(actions)
    }

    fn reset(&mut self) {
        self.state = TwoLegState::Idle;
        self.current_event = None;
        self.current_cycle = None;
        self.pending_orders.clear();
        self.positions.clear();
        self.last_up_quote = None;
        self.last_down_quote = None;
        self.realized_pnl = Decimal::ZERO;
        self.detector.reset(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_transitions() {
        let config = TwoLegConfig::default();
        let strategy = TwoLegStrategy::new(config);

        assert_eq!(strategy.state, TwoLegState::Idle);
        assert!(!strategy.is_active());
    }
}
