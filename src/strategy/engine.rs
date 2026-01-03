use crate::adapters::{PolymarketWebSocket, PostgresStore, QuoteCache, QuoteUpdate};
use crate::config::AppConfig;
use crate::domain::{Cycle, MarketSnapshot, Order, Quote, Round, Side, StrategyState};
use crate::error::{PloyError, Result};
use crate::strategy::{OrderExecutor, RiskManager, SignalDetector, TradingCalculator};
use chrono::Utc;
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

/// Main strategy engine orchestrating all components
pub struct StrategyEngine {
    config: AppConfig,
    store: PostgresStore,
    executor: OrderExecutor,
    risk_manager: Arc<RiskManager>,
    signal_detector: Arc<RwLock<SignalDetector>>,
    quote_cache: QuoteCache,
    state: Arc<RwLock<EngineState>>,
    calculator: TradingCalculator,
}

/// Internal engine state
#[derive(Debug, Clone)]
struct EngineState {
    /// Current strategy state
    strategy_state: StrategyState,
    /// Current round being traded
    current_round: Option<Round>,
    /// Current cycle
    current_cycle: Option<CycleContext>,
    /// Whether we should stop
    shutdown: bool,
}

/// Context for an active cycle
#[derive(Debug, Clone)]
struct CycleContext {
    cycle_id: i32,
    leg1_side: Side,
    leg1_price: Decimal,
    leg1_shares: u64,
    leg1_order_id: String,
    leg2_order_id: Option<String>,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            strategy_state: StrategyState::Idle,
            current_round: None,
            current_cycle: None,
            shutdown: false,
        }
    }
}

impl StrategyEngine {
    /// Create a new strategy engine
    pub async fn new(
        config: AppConfig,
        store: PostgresStore,
        executor: OrderExecutor,
        quote_cache: QuoteCache,
    ) -> Result<Self> {
        let risk_manager = Arc::new(RiskManager::new(config.risk.clone()));
        let signal_detector = SignalDetector::new(config.strategy.clone());

        // Create calculator from config buffers
        let calculator = TradingCalculator::with_buffers(
            config.strategy.fee_buffer,
            config.strategy.slippage_buffer,
            config.strategy.profit_buffer,
        );

        Ok(Self {
            config,
            store,
            executor,
            risk_manager,
            signal_detector: Arc::new(RwLock::new(signal_detector)),
            quote_cache,
            state: Arc::new(RwLock::new(EngineState::default())),
            calculator,
        })
    }

    /// Get current state
    pub async fn state(&self) -> StrategyState {
        self.state.read().await.strategy_state
    }

    /// Signal shutdown
    pub async fn shutdown(&self) {
        info!("Shutdown requested");
        self.state.write().await.shutdown = true;
    }

    /// Main run loop
    pub async fn run(&self, mut updates: broadcast::Receiver<QuoteUpdate>) -> Result<()> {
        info!("Strategy engine starting");

        loop {
            // Check for shutdown
            if self.state.read().await.shutdown {
                info!("Shutting down strategy engine");
                break;
            }

            // Receive quote update with timeout
            match tokio::time::timeout(
                std::time::Duration::from_secs(1),
                updates.recv(),
            )
            .await
            {
                Ok(Ok(update)) => {
                    if let Err(e) = self.on_quote_update(update).await {
                        error!("Error processing quote update: {}", e);
                    }
                }
                Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                    warn!("Missed {} quote updates", n);
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    error!("Quote update channel closed");
                    break;
                }
                Err(_) => {
                    // Timeout - check round status
                    self.check_round_transition().await?;
                }
            }
        }

        Ok(())
    }

    /// Handle a quote update
    async fn on_quote_update(&self, update: QuoteUpdate) -> Result<()> {
        let mut state = self.state.write().await;
        let round = state.current_round.as_ref();

        // Process based on current strategy state
        match state.strategy_state {
            StrategyState::Idle => {
                // Nothing to do, waiting for round start
            }
            StrategyState::WatchWindow => {
                // Check for dump signal
                let mut detector = self.signal_detector.write().await;
                let round_slug = round.map(|r| r.slug.as_str());

                if let Some(signal) = detector.update(&update.quote, round_slug) {
                    // Validate signal
                    if signal.is_valid(self.config.execution.max_spread_bps) {
                        drop(detector);
                        drop(state);

                        // Try to enter Leg1
                        if let Err(e) = self.enter_leg1(signal.side, signal.trigger_price).await {
                            warn!("Failed to enter Leg1: {}", e);
                        }
                    } else {
                        debug!(
                            "Signal rejected: spread {} > max {}",
                            signal.spread_bps, self.config.execution.max_spread_bps
                        );
                    }
                }
            }
            StrategyState::Leg1Pending => {
                // Waiting for Leg1 fill (handled by executor)
            }
            StrategyState::Leg1Filled => {
                // Check for Leg2 opportunity
                let should_enter_leg2 = if let Some(ctx) = &state.current_cycle {
                    let opposite_side = ctx.leg1_side.opposite();
                    if update.side == opposite_side {
                        if let Some(ask) = update.quote.best_ask {
                            let detector = self.signal_detector.read().await;
                            if detector.check_leg2_condition(ctx.leg1_price, ask) {
                                Some((opposite_side, ask))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Check for force Leg2
                let should_force = state
                    .current_round
                    .as_ref()
                    .map(|r| self.risk_manager.must_force_leg2(r))
                    .unwrap_or(false);

                drop(state);

                if let Some((opposite_side, ask)) = should_enter_leg2 {
                    if let Err(e) = self.enter_leg2(opposite_side, ask).await {
                        warn!("Failed to enter Leg2: {}", e);
                    }
                } else if should_force {
                    self.force_leg2_or_abort().await?;
                }
            }
            StrategyState::Leg2Pending => {
                // Waiting for Leg2 fill (handled by executor)
            }
            StrategyState::CycleComplete | StrategyState::Abort => {
                // Cleanup and return to idle
                drop(state);
                self.transition_to_idle().await?;
            }
        }

        Ok(())
    }

    /// Check for round transitions
    async fn check_round_transition(&self) -> Result<()> {
        let state = self.state.read().await;

        if let Some(round) = &state.current_round {
            if round.has_ended() {
                // Round ended
                info!("Round {} has ended", round.slug);

                // If we're in the middle of a cycle, abort it
                if state.strategy_state.is_in_cycle() {
                    drop(state);
                    self.abort_cycle("Round ended").await?;
                } else {
                    drop(state);
                    self.transition_to_idle().await?;
                }
            } else if state.strategy_state == StrategyState::WatchWindow {
                // Check if window expired
                let minutes_elapsed = round.minutes_elapsed();
                if minutes_elapsed >= self.config.strategy.window_min as i64 {
                    info!("Watch window expired after {} minutes", minutes_elapsed);
                    drop(state);
                    self.transition_to_idle().await?;
                }
            }
        }

        Ok(())
    }

    /// Set the current round
    pub async fn set_round(&self, round: Round) -> Result<()> {
        let round_id = self.store.upsert_round(&round).await?;
        let mut round_with_id = round.clone();
        round_with_id.id = Some(round_id);

        let mut state = self.state.write().await;
        state.current_round = Some(round_with_id);

        // Reset signal detector
        let mut detector = self.signal_detector.write().await;
        detector.reset(Some(&round.slug));

        // Transition to watch window if idle
        if state.strategy_state == StrategyState::Idle {
            state.strategy_state = StrategyState::WatchWindow;
            info!("Entering watch window for round: {}", round.slug);
        }

        Ok(())
    }

    /// Enter Leg1 position
    async fn enter_leg1(&self, side: Side, price: Decimal) -> Result<()> {
        let mut state = self.state.write().await;

        // Validate state
        if state.strategy_state != StrategyState::WatchWindow {
            return Err(PloyError::InvalidStateTransition {
                from: state.strategy_state.to_string(),
                to: "LEG1_PENDING".to_string(),
            });
        }

        // Get round
        let round = state
            .current_round
            .as_ref()
            .ok_or_else(|| PloyError::Internal("No active round".to_string()))?;

        // Risk check
        self.risk_manager
            .check_leg1_entry(self.config.strategy.shares, price, round)
            .await?;

        // Get token ID
        let token_id = round.token_id(side).to_string();
        let round_id = round.id.unwrap();

        // Create cycle
        let cycle_id = self.store.create_cycle(round_id, StrategyState::Leg1Pending).await?;

        info!(
            "Entering Leg1: {} {} shares of {} @ {}",
            side, self.config.strategy.shares, token_id, price
        );

        state.strategy_state = StrategyState::Leg1Pending;
        drop(state);

        // Execute order
        let result = self
            .executor
            .buy(&token_id, side, self.config.strategy.shares, price)
            .await?;

        // Update state based on result
        let mut state = self.state.write().await;

        if result.filled_shares > 0 {
            let fill_price = result.avg_fill_price.unwrap_or(price);

            // Update database
            self.store
                .update_cycle_leg1(cycle_id, side, fill_price, result.filled_shares)
                .await?;

            // Update state
            state.current_cycle = Some(CycleContext {
                cycle_id,
                leg1_side: side,
                leg1_price: fill_price,
                leg1_shares: result.filled_shares,
                leg1_order_id: result.order_id,
                leg2_order_id: None,
            });

            state.strategy_state = StrategyState::Leg1Filled;
            info!(
                "Leg1 filled: {} shares @ {}",
                result.filled_shares, fill_price
            );

            // Mark signal as triggered
            let mut detector = self.signal_detector.write().await;
            detector.mark_triggered(side);
        } else {
            // Order failed
            self.store.abort_cycle(cycle_id, "Leg1 not filled").await?;
            state.strategy_state = StrategyState::Abort;
            warn!("Leg1 order failed to fill");
        }

        Ok(())
    }

    /// Enter Leg2 position
    async fn enter_leg2(&self, side: Side, price: Decimal) -> Result<()> {
        let mut state = self.state.write().await;

        // Validate state
        if state.strategy_state != StrategyState::Leg1Filled {
            return Err(PloyError::InvalidStateTransition {
                from: state.strategy_state.to_string(),
                to: "LEG2_PENDING".to_string(),
            });
        }

        let ctx = state
            .current_cycle
            .as_ref()
            .ok_or_else(|| PloyError::Internal("No active cycle".to_string()))?
            .clone();

        let round = state
            .current_round
            .as_ref()
            .ok_or_else(|| PloyError::Internal("No active round".to_string()))?;

        let token_id = round.token_id(side).to_string();

        info!(
            "Entering Leg2: {} {} shares of {} @ {}",
            side, ctx.leg1_shares, token_id, price
        );

        state.strategy_state = StrategyState::Leg2Pending;
        drop(state);

        // Execute order
        let result = self
            .executor
            .buy(&token_id, side, ctx.leg1_shares, price)
            .await?;

        // Update state based on result
        let mut state = self.state.write().await;

        if result.filled_shares > 0 {
            let fill_price = result.avg_fill_price.unwrap_or(price);

            // Calculate PnL using centralized calculator
            let net_pnl = self.calculator.expected_pnl(
                result.filled_shares,
                ctx.leg1_price,
                fill_price,
            );

            // Update database
            self.store
                .update_cycle_leg2(ctx.cycle_id, fill_price, result.filled_shares, net_pnl)
                .await?;

            // Record success
            self.risk_manager.record_success(net_pnl).await;

            // Update daily metrics
            let today = Utc::now().date_naive();
            self.store.record_cycle_completion(today, net_pnl).await?;

            state.strategy_state = StrategyState::CycleComplete;
            info!(
                "Leg2 filled: {} shares @ {}. Cycle PnL: {}",
                result.filled_shares, fill_price, net_pnl
            );
        } else {
            // Leg2 failed - this is bad, we have open exposure
            error!("Leg2 order failed to fill - open exposure!");

            self.store.abort_cycle(ctx.cycle_id, "Leg2 not filled").await?;
            self.risk_manager.record_failure("Leg2 not filled").await;

            state.strategy_state = StrategyState::Abort;
        }

        Ok(())
    }

    /// Force Leg2 or abort when time is running out
    async fn force_leg2_or_abort(&self) -> Result<()> {
        let state = self.state.read().await;

        let ctx = match &state.current_cycle {
            Some(c) => c.clone(),
            None => return Ok(()),
        };

        let round = match &state.current_round {
            Some(r) => r.clone(),
            None => return Ok(()),
        };

        drop(state);

        warn!(
            "Forcing Leg2 with {} seconds remaining",
            round.seconds_remaining()
        );

        // Try to get opposite side quote
        let opposite_side = ctx.leg1_side.opposite();
        let token_id = round.token_id(opposite_side);

        if let Ok((_, Some(ask))) = self.executor.get_prices(token_id).await {
            // Use higher price tolerance for forced execution
            let forced_price = ask * (Decimal::ONE + self.config.strategy.slippage_buffer);
            if let Err(e) = self.enter_leg2(opposite_side, forced_price).await {
                error!("Forced Leg2 failed: {}", e);
                self.abort_cycle("Forced Leg2 failed").await?;
            }
        } else {
            // No quote available, must abort
            self.abort_cycle("No quote for forced Leg2").await?;
        }

        Ok(())
    }

    /// Abort the current cycle
    async fn abort_cycle(&self, reason: &str) -> Result<()> {
        let mut state = self.state.write().await;

        if let Some(ctx) = &state.current_cycle {
            warn!("Aborting cycle {}: {}", ctx.cycle_id, reason);
            self.store.abort_cycle(ctx.cycle_id, reason).await?;
            self.risk_manager.record_failure(reason).await;

            let today = Utc::now().date_naive();
            self.store.record_cycle_abort(today).await?;
        }

        state.strategy_state = StrategyState::Abort;
        state.current_cycle = None;

        Ok(())
    }

    /// Transition back to idle state
    async fn transition_to_idle(&self) -> Result<()> {
        let mut state = self.state.write().await;

        state.strategy_state = StrategyState::Idle;
        state.current_cycle = None;
        state.current_round = None;

        // Reset signal detector
        let mut detector = self.signal_detector.write().await;
        detector.reset(None);

        debug!("Transitioned to IDLE state");
        Ok(())
    }

    /// Get risk manager for external queries
    pub fn risk_manager(&self) -> Arc<RiskManager> {
        Arc::clone(&self.risk_manager)
    }

    /// Check if dry run mode is enabled
    pub fn is_dry_run(&self) -> bool {
        self.executor.is_dry_run()
    }
}
