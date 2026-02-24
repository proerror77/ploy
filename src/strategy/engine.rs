use crate::adapters::{QuoteCache, QuoteUpdate};
use crate::config::AppConfig;
use crate::domain::{Order, OrderStatus, Round, Side, StrategyState, TimeInForce};
use crate::error::{PloyError, Result};
use crate::strategy::engine_store::EngineStore;
use crate::strategy::{
    MarketDepth, OrderExecutor, RiskManager, SignalDetector, SlippageCheck, SlippageConfig,
    SlippageProtection, TradingCalculator,
};
use chrono::Utc;
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use tracing::{debug, error, info, warn};

/// Main strategy engine orchestrating all components
pub struct StrategyEngine {
    config: AppConfig,
    store: Box<dyn EngineStore>,
    executor: OrderExecutor,
    risk_manager: Arc<RiskManager>,
    signal_detector: Arc<RwLock<SignalDetector>>,
    quote_cache: QuoteCache,
    state: Arc<RwLock<EngineState>>,
    calculator: TradingCalculator,
    /// Slippage protection for order execution
    slippage: SlippageProtection,
    /// Mutex to prevent concurrent order submissions (separate from state lock)
    execution_mutex: Mutex<()>,
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
    /// Version number for optimistic locking (prevents race conditions)
    version: u64,
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
    /// Guard against duplicate forced Leg2 submissions from concurrent paths.
    force_leg2_attempted: bool,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            strategy_state: StrategyState::Idle,
            current_round: None,
            current_cycle: None,
            shutdown: false,
            version: 0,
        }
    }
}

impl StrategyEngine {
    /// Create a new strategy engine
    pub async fn new(
        config: AppConfig,
        store: impl EngineStore + 'static,
        executor: OrderExecutor,
        quote_cache: QuoteCache,
    ) -> Result<Self> {
        // Safety guard: if we can't confirm fills, the current engine would treat
        // submitted (but unconfirmed) orders as failures, risking stray live orders.
        if !config.dry_run.enabled && !config.execution.confirm_fills {
            return Err(PloyError::Validation(
                "execution.confirm_fills must be true when dry_run.enabled is false".to_string(),
            ));
        }

        let risk_manager = Arc::new(RiskManager::new(config.risk.clone()));
        let signal_detector = SignalDetector::new(config.strategy.clone());

        // Create calculator from config buffers
        let calculator = TradingCalculator::with_buffers(
            config.strategy.fee_buffer,
            config.strategy.slippage_buffer,
            config.strategy.profit_buffer,
        );

        // Create slippage protection from config
        let slippage = SlippageProtection::new(SlippageConfig {
            max_slippage_pct: config.strategy.slippage_buffer,
            ..SlippageConfig::default()
        });

        Ok(Self {
            config,
            store: Box::new(store),
            executor,
            risk_manager,
            signal_detector: Arc::new(RwLock::new(signal_detector)),
            quote_cache,
            state: Arc::new(RwLock::new(EngineState::default())),
            calculator,
            slippage,
            execution_mutex: Mutex::new(()),
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
            match tokio::time::timeout(std::time::Duration::from_secs(1), updates.recv()).await {
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
        // Snapshot state needed for decision-making without holding locks across async work.
        let (round, strategy_state, current_cycle) = {
            let state = self.state.read().await;
            let Some(round) = state.current_round.clone() else {
                // No active round set yet; ignore market data.
                return Ok(());
            };
            (round, state.strategy_state, state.current_cycle.clone())
        };

        // Always enforce round/window transitions even when quote updates are frequent.
        if round.has_ended() {
            if strategy_state.requires_abort_on_round_end() {
                self.abort_cycle_and_halt_safely("Round ended").await?;
            } else {
                self.transition_to_idle().await?;
            }
            return Ok(());
        }

        if strategy_state == StrategyState::WatchWindow {
            let minutes_elapsed = round.minutes_elapsed();
            if minutes_elapsed >= self.config.strategy.window_min as i64 {
                info!("Watch window expired after {} minutes", minutes_elapsed);
                self.transition_to_idle().await?;
                return Ok(());
            }
        }

        // Ignore updates for tokens that don't belong to the active round.
        // Note: this must happen *after* time-based transitions. The WebSocket client can have
        // multiple historical token subscriptions, and we still need to enforce round/window
        // expiry even if we're receiving quotes for unrelated tokens.
        if update.token_id != round.up_token_id && update.token_id != round.down_token_id {
            return Ok(());
        }

        // Process based on current strategy state
        match strategy_state {
            StrategyState::Idle => {
                // Nothing to do, waiting for round start
            }
            StrategyState::WatchWindow => {
                // Check for dump signal
                let round_slug = Some(round.slug.as_str());
                let signal = {
                    let mut detector = self.signal_detector.write().await;
                    detector.update(&update.quote, round_slug)
                };

                if let Some(signal) = signal {
                    // Validate signal
                    if signal.is_valid(self.config.execution.max_spread_bps) {
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
                let should_enter_leg2 = match current_cycle.as_ref() {
                    Some(ctx) => {
                        let opposite_side = ctx.leg1_side.opposite();
                        if update.side != opposite_side {
                            None
                        } else if let Some(ask) = update.quote.best_ask {
                            let detector = self.signal_detector.read().await;
                            detector
                                .check_leg2_condition(ctx.leg1_price, ask)
                                .then_some((opposite_side, ask))
                        } else {
                            None
                        }
                    }
                    None => None,
                };

                // Check for force Leg2
                let should_force = self.risk_manager.must_force_leg2(&round);

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
                if state.strategy_state.requires_abort_on_round_end() {
                    drop(state);
                    self.abort_cycle_and_halt_safely("Round ended").await?;
                } else {
                    drop(state);
                    self.transition_to_idle().await?;
                }
            } else if matches!(
                state.strategy_state,
                StrategyState::CycleComplete | StrategyState::Abort
            ) {
                // Terminal cycle state cleanup (timeout path). Without quote updates this state
                // would otherwise persist indefinitely.
                drop(state);
                self.transition_to_idle().await?;
            } else if state.strategy_state == StrategyState::Leg1Filled
                && self.risk_manager.must_force_leg2(round)
            {
                // No quote updates (timeout path), but we're near round end and still exposed.
                // Force Leg2 using REST best prices.
                drop(state);
                self.force_leg2_or_abort().await?;
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
        // Avoid resetting detector/state on the same round every poll interval.
        // Also: never switch rounds mid-cycle. The engine must not mix tokens/prices across rounds.
        {
            let state = self.state.read().await;
            if let Some(current) = state.current_round.as_ref() {
                if current.slug == round.slug {
                    return Ok(());
                }

                if state.strategy_state.requires_abort_on_round_end() {
                    warn!(
                        current_round = %current.slug,
                        new_round = %round.slug,
                        state = %state.strategy_state,
                        "Ignoring round change while a cycle is active"
                    );
                    return Ok(());
                }
            }
        }

        let round_id = self.store.upsert_round(&round).await?;
        let mut round_with_id = round.clone();
        round_with_id.id = Some(round_id);

        {
            let mut state = self.state.write().await;
            state.current_round = Some(round_with_id);

            // Transition to watch window if idle (and still within the configured entry window).
            if state.strategy_state == StrategyState::Idle {
                if !round.has_ended()
                    && round.minutes_elapsed() < self.config.strategy.window_min as i64
                {
                    state.strategy_state = StrategyState::WatchWindow;
                    info!("Entering watch window for round: {}", round.slug);
                } else {
                    debug!(
                        "Round {} already outside watch window (elapsed={}m, window={}m, ended={})",
                        round.slug,
                        round.minutes_elapsed(),
                        self.config.strategy.window_min,
                        round.has_ended(),
                    );
                }
            }

            state.version += 1;
        }

        // Reset signal detector for the new round. (SignalDetector also self-resets when it
        // sees a new round slug, but doing it here makes the state transition explicit.)
        {
            let mut detector = self.signal_detector.write().await;
            detector.reset(Some(&round.slug));
        }

        // Persist strategy state for observability/crash recovery (best effort).
        let (strategy_state, cycle_id) = {
            let state = self.state.read().await;
            (
                state.strategy_state,
                state.current_cycle.as_ref().map(|c| c.cycle_id),
            )
        };
        self.persist_strategy_state_best_effort(strategy_state, Some(round_id), cycle_id)
            .await;

        Ok(())
    }

    /// Enter Leg1 position
    async fn enter_leg1(&self, side: Side, price: Decimal) -> Result<()> {
        let _exec_guard = self.execution_mutex.lock().await;

        // Snapshot current round/state without holding the lock across async work.
        let (round, round_id) = {
            let state = self.state.read().await;
            if state.strategy_state != StrategyState::WatchWindow {
                return Err(PloyError::InvalidStateTransition {
                    from: state.strategy_state.to_string(),
                    to: "LEG1_PENDING".to_string(),
                });
            }

            let round = state
                .current_round
                .clone()
                .ok_or_else(|| PloyError::Internal("No active round".to_string()))?;
            let round_id = round.id.ok_or_else(|| {
                crate::error::PloyError::InvalidState(
                    "Round ID not set after database upsert".to_string(),
                )
            })?;
            (round, round_id)
        };

        let token_id = round.token_id(side).to_string();

        // Ensure we have fresh market data for slippage + execution decisions.
        self.quote_cache
            .validate_freshness(&token_id, self.config.execution.max_quote_age_secs)
            .await?;

        let quote = self
            .quote_cache
            .get(&token_id)
            .ok_or_else(|| PloyError::QuoteUnavailable {
                token_id: token_id.clone(),
            })?;

        let (best_bid, best_ask) = match (quote.best_bid, quote.best_ask) {
            (Some(bid), Some(ask)) => (bid, ask),
            _ => {
                return Err(PloyError::MarketDataUnavailable(format!(
                    "Missing bid/ask for token {}",
                    token_id
                )));
            }
        };

        let depth = MarketDepth {
            best_bid,
            best_ask,
            bid_size: quote.bid_size.unwrap_or(Decimal::ZERO),
            ask_size: quote.ask_size.unwrap_or(Decimal::ZERO),
        };

        let order_size = Decimal::from(self.config.strategy.shares);
        let mut order_price = match self.slippage.check_buy_order(&depth, order_size, price) {
            SlippageCheck::Rejected { reason, .. } => {
                warn!("Leg1 slippage check failed: {}", reason);
                return Err(PloyError::Validation(format!(
                    "Leg1 slippage rejected: {}",
                    reason
                )));
            }
            SlippageCheck::Approved {
                limit_price,
                estimated_slippage_pct,
            } => {
                debug!(
                    "Leg1 slippage approved: {:.2}%",
                    estimated_slippage_pct * Decimal::from(100)
                );
                limit_price
            }
        };

        // Keep at least the requested price (forced paths may pass a higher limit).
        order_price = order_price.max(price).min(Decimal::ONE);

        // Risk check using worst-case limit price.
        self.risk_manager
            .check_leg1_entry(self.config.strategy.shares, order_price, &round)
            .await?;

        // Build order request now so we can reference its client_order_id in in-memory state.
        // (This makes abort paths safer even before we have an exchange order id.)
        let mut request = crate::domain::OrderRequest::buy_limit(
            token_id.clone(),
            side,
            self.config.strategy.shares,
            order_price,
        );
        request.time_in_force = TimeInForce::IOC;

        // Create cycle + move to LEG1_PENDING under state lock to prevent cross-round contamination.
        let (cycle_id, expected_version) = {
            let mut state = self.state.write().await;

            // Re-validate state/round (they can change while we were doing async checks).
            if state.strategy_state != StrategyState::WatchWindow {
                return Err(PloyError::InvalidStateTransition {
                    from: state.strategy_state.to_string(),
                    to: "LEG1_PENDING".to_string(),
                });
            }
            let current_round = state
                .current_round
                .as_ref()
                .ok_or_else(|| PloyError::Internal("No active round".to_string()))?;
            if current_round.slug != round.slug {
                return Err(PloyError::InvalidState(format!(
                    "Round changed before Leg1 submission (expected {}, got {})",
                    round.slug, current_round.slug
                )));
            }
            if current_round.has_ended()
                || current_round.minutes_elapsed() >= self.config.strategy.window_min as i64
            {
                return Err(PloyError::InvalidState(format!(
                    "Round {} is no longer within the entry window",
                    current_round.slug
                )));
            }

            let cycle_id = self
                .store
                .create_cycle(round_id, StrategyState::Leg1Pending)
                .await?;

            let expected_version = state.version;
            state.strategy_state = StrategyState::Leg1Pending;
            state.current_cycle = Some(CycleContext {
                cycle_id,
                leg1_side: side,
                leg1_price: order_price,
                leg1_shares: self.config.strategy.shares,
                // Use client_order_id until we have an exchange order id.
                leg1_order_id: request.client_order_id.clone(),
                leg2_order_id: None,
                force_leg2_attempted: false,
            });
            state.version += 1;

            (cycle_id, expected_version)
        };

        // Persist state transition (best effort).
        self.persist_strategy_state_best_effort(
            StrategyState::Leg1Pending,
            Some(round_id),
            Some(cycle_id),
        )
        .await;

        // Best-effort daily metrics update (avoid failing trading logic on telemetry).
        let today = Utc::now().date_naive();
        if let Err(e) = self.store.increment_cycle_count(today).await {
            error!("Failed to increment cycle count: {}", e);
        }

        info!(
            "Entering Leg1: {} {} shares of {} @ {}",
            side, self.config.strategy.shares, token_id, order_price
        );

        // Persist the intent before submitting to the exchange.
        let client_order_id = request.client_order_id.clone();
        let order = Order::from_request(&request, Some(cycle_id), 1);
        if let Err(e) = self.store.insert_order(&order).await {
            let halt_reason = "Failed to persist Leg1 order";
            self.risk_manager.trigger_circuit_breaker(halt_reason).await;
            self.persist_halt_if_needed().await;
            self.abort_cycle(halt_reason).await?;
            return Err(e);
        }

        let result = match self.executor.execute(&request).await {
            Ok(r) => r,
            Err(e) => {
                let _ = self
                    .store
                    .update_order_status(&client_order_id, OrderStatus::Failed, None)
                    .await;
                let halt_reason = "Leg1 execution failed";
                self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                self.persist_halt_if_needed().await;
                self.abort_cycle(halt_reason).await?;
                return Err(e);
            }
        };

        // Persist order submission + outcome.
        // First mark as submitted (sets submitted_at), then overwrite with final status/fill.
        let _ = self
            .store
            .update_order_status(
                &client_order_id,
                OrderStatus::Submitted,
                Some(&result.order_id),
            )
            .await;

        if result.filled_shares > 0 {
            let fill_price = result.avg_fill_price.unwrap_or(request.limit_price);
            let _ = self
                .store
                .update_order_fill(
                    &client_order_id,
                    result.filled_shares,
                    fill_price,
                    result.status,
                )
                .await;
        } else {
            let _ = self
                .store
                .update_order_status(&client_order_id, result.status, None)
                .await;
        }

        // Ensure state hasn't been modified by another task while the order was executing.
        {
            let state = self.state.read().await;
            if state.version != expected_version + 1 {
                let observed_version = state.version;
                warn!(
                    "State version mismatch: expected {}, got {}. Another thread modified state during order execution.",
                    expected_version + 1,
                    observed_version
                );
                // Abort + halt: this indicates a serious race that can lead to untracked exposure.
                if let Err(e) = self
                    .store
                    .abort_cycle(cycle_id, "State modified by concurrent operation")
                    .await
                {
                    error!("Failed to abort cycle {} in DB: {}", cycle_id, e);
                }

                let halt_reason = "Concurrent state modification detected";
                self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                self.persist_halt_if_needed().await;
                self.persist_strategy_state_best_effort(StrategyState::Abort, Some(round_id), None)
                    .await;
                return Err(PloyError::Internal(
                    "Concurrent state modification detected during Leg1 execution".to_string(),
                ));
            }
        }

        if result.filled_shares > 0 {
            let fill_price = result.avg_fill_price.unwrap_or(order_price);

            // Update database
            if let Err(err) = self
                .store
                .update_cycle_leg1(cycle_id, side, fill_price, result.filled_shares)
                .await
            {
                error!(
                    "Failed to update cycle {} after Leg1 fill (exposure exists): {}",
                    cycle_id, err
                );

                let unwind_ctx = CycleContext {
                    cycle_id,
                    leg1_side: side,
                    leg1_price: fill_price,
                    leg1_shares: result.filled_shares,
                    leg1_order_id: result.order_id.clone(),
                    leg2_order_id: None,
                force_leg2_attempted: false,
                };

                let unwind_summary = match self
                    .unwind_leg1_exposure(&unwind_ctx, &round, result.filled_shares)
                    .await
                {
                    Ok(s) => s,
                    Err(e) => format!("unwind failed: {}", e),
                };

                let today = Utc::now().date_naive();
                let halt_reason = "DB update failed after Leg1 fill - exposure may exist";
                self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                if let Err(e) = self.store.halt_trading(today, halt_reason).await {
                    error!("Failed to persist halt_trading: {}", e);
                }
                self.persist_halt_if_needed().await;
                let _ = self
                    .store
                    .abort_cycle(cycle_id, &format!("{}; {}", halt_reason, unwind_summary))
                    .await;

                {
                    let mut state = self.state.write().await;
                    state.strategy_state = StrategyState::Abort;
                    state.current_cycle = None;
                    state.version += 1;
                }

                self.persist_strategy_state_best_effort(StrategyState::Abort, Some(round_id), None)
                    .await;

                return Err(err);
            }

            // Update state
            {
                let mut state = self.state.write().await;
                if state.version != expected_version + 1 {
                    let observed_version = state.version;
                    drop(state);
                    warn!(
                        "State version mismatch after Leg1 DB update: expected {}, got {}",
                        expected_version + 1,
                        observed_version
                    );
                    if let Err(e) = self
                        .store
                        .abort_cycle(cycle_id, "State modified by concurrent operation")
                        .await
                    {
                        error!("Failed to abort cycle {} in DB: {}", cycle_id, e);
                    }

                    let halt_reason = "Concurrent state modification detected";
                    self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                    self.persist_halt_if_needed().await;
                    self.persist_strategy_state_best_effort(
                        StrategyState::Abort,
                        Some(round_id),
                        None,
                    )
                    .await;
                    return Err(PloyError::Internal(
                        "Concurrent state modification detected during Leg1 execution".to_string(),
                    ));
                }

                state.current_cycle = Some(CycleContext {
                    cycle_id,
                    leg1_side: side,
                    leg1_price: fill_price,
                    leg1_shares: result.filled_shares,
                    leg1_order_id: result.order_id,
                    leg2_order_id: None,
                force_leg2_attempted: false,
                });

                state.strategy_state = StrategyState::Leg1Filled;
                state.version += 1;
            }

            self.persist_strategy_state_best_effort(
                StrategyState::Leg1Filled,
                Some(round_id),
                Some(cycle_id),
            )
            .await;

            info!(
                "Leg1 filled: {} shares @ {}",
                result.filled_shares, fill_price
            );

            // Mark signal as triggered
            {
                let mut detector = self.signal_detector.write().await;
                detector.mark_triggered(side);
            }
        } else {
            // Order got 0 fill (IOC) - abort cycle but do not count as a failure.
            self.abort_cycle_neutral(&format!("Leg1 not filled ({:?})", result.status))
                .await?;
            warn!("Leg1 order got 0 fill");
        }

        Ok(())
    }

    /// Enter Leg2 position
    async fn enter_leg2(&self, side: Side, price: Decimal) -> Result<()> {
        self.enter_leg2_inner(side, price, false).await
    }

    /// Enter Leg2 position in forced mode.
    ///
    /// Forced mode is used near round end to reduce exposure. It must not depend on fresh WS
    /// order book data, since the timeout path is triggered specifically when WS quotes may
    /// not be arriving. Slippage/depth rejections are treated as warnings and executed
    /// best-effort (still with FOK to avoid partial hedges).
    async fn enter_leg2_forced(&self, side: Side, price: Decimal) -> Result<()> {
        self.enter_leg2_inner(side, price, true).await
    }

    async fn enter_leg2_inner(&self, side: Side, price: Decimal, forced: bool) -> Result<()> {
        let _exec_guard = self.execution_mutex.lock().await;

        // Guard against duplicate forced Leg2 submissions.
        if forced {
            let state = self.state.read().await;
            if state
                .current_cycle
                .as_ref()
                .is_some_and(|c| c.force_leg2_attempted)
            {
                warn!("Force Leg2 already attempted for this cycle; skipping duplicate");
                return Ok(());
            }
            drop(state);
            // Mark the flag under write lock before proceeding.
            let mut state = self.state.write().await;
            if let Some(ref mut ctx) = state.current_cycle {
                ctx.force_leg2_attempted = true;
            }
        }

        let (ctx, round) = {
            let state = self.state.read().await;
            if state.strategy_state != StrategyState::Leg1Filled {
                return Err(PloyError::InvalidStateTransition {
                    from: state.strategy_state.to_string(),
                    to: "LEG2_PENDING".to_string(),
                });
            }

            let ctx = state
                .current_cycle
                .clone()
                .ok_or_else(|| PloyError::Internal("No active cycle".to_string()))?;
            let round = state
                .current_round
                .clone()
                .ok_or_else(|| PloyError::Internal("No active round".to_string()))?;
            (ctx, round)
        };

        let token_id = round.token_id(side).to_string();

        // Market depth for slippage + execution decisions.
        //
        // Non-forced: require fresh WS data (depth-based slippage protection).
        // Forced: allow stale/missing WS data and fall back to REST best bid/ask.
        let mut best_bid: Option<Decimal> = None;
        let mut best_ask: Option<Decimal> = None;
        let mut bid_size: Option<Decimal> = None;
        let mut ask_size: Option<Decimal> = None;

        if forced {
            // Best-effort: use whatever is in the cache (even if stale).
            if let Some(q) = self.quote_cache.get(&token_id) {
                best_bid = q.best_bid;
                best_ask = q.best_ask;
                bid_size = q.bid_size;
                ask_size = q.ask_size;
            }
        } else {
            // Strict: require fresh WS snapshot.
            self.quote_cache
                .validate_freshness(&token_id, self.config.execution.max_quote_age_secs)
                .await?;
            let quote =
                self.quote_cache
                    .get(&token_id)
                    .ok_or_else(|| PloyError::QuoteUnavailable {
                        token_id: token_id.clone(),
                    })?;
            best_bid = quote.best_bid;
            best_ask = quote.best_ask;
            bid_size = quote.bid_size;
            ask_size = quote.ask_size;
        }

        if best_ask.is_none() {
            let (bid, ask) = self.executor.get_prices(&token_id).await?;
            best_bid = best_bid.or(bid);
            best_ask = best_ask.or(ask);
        }

        let best_ask = best_ask.ok_or_else(|| {
            PloyError::MarketDataUnavailable(format!("Missing ask for token {}", token_id))
        })?;
        let best_bid = best_bid.unwrap_or(best_ask);

        let depth = MarketDepth {
            best_bid,
            best_ask,
            bid_size: bid_size.unwrap_or(Decimal::ZERO),
            // If forced and size is unknown, skip depth rejection (still enforce price slippage).
            ask_size: if forced {
                ask_size.unwrap_or(Decimal::MAX)
            } else {
                ask_size.unwrap_or(Decimal::ZERO)
            },
        };

        let order_size = Decimal::from(ctx.leg1_shares);
        let mut order_price = match self.slippage.check_buy_order(&depth, order_size, price) {
            SlippageCheck::Rejected { reason, .. } => {
                if forced {
                    warn!(
                        "Forced Leg2 slippage/depth check rejected: {}. Proceeding best-effort.",
                        reason
                    );
                    // Mirror the +0.1% buffer used in slippage module to improve fill probability.
                    best_ask * (Decimal::ONE + Decimal::new(1, 3))
                } else {
                    warn!("Leg2 slippage check failed: {}", reason);
                    return Err(PloyError::Validation(format!(
                        "Leg2 slippage rejected: {}",
                        reason
                    )));
                }
            }
            SlippageCheck::Approved {
                limit_price,
                estimated_slippage_pct,
            } => {
                debug!(
                    "Leg2 slippage approved: {:.2}%",
                    estimated_slippage_pct * Decimal::from(100)
                );
                limit_price
            }
        };

        // Keep at least the requested price (forced paths may pass a higher limit).
        // Prevent forced Leg2 from creating a guaranteed loss: in binary markets,
        // combined leg cost > 1.0 means guaranteed loss regardless of outcome.
        let max_leg2_price = if forced {
            (Decimal::ONE - ctx.leg1_price).min(Decimal::ONE)
        } else {
            Decimal::ONE
        };
        order_price = order_price.max(price).min(max_leg2_price);

        // Execute order (FOK to avoid partial hedges).
        let mut request = crate::domain::OrderRequest::buy_limit(
            token_id.clone(),
            side,
            ctx.leg1_shares,
            order_price,
        );
        request.time_in_force = TimeInForce::FOK;

        info!(
            "Entering Leg2: {} {} shares of {} @ {}",
            side, ctx.leg1_shares, token_id, order_price
        );

        // Move to LEG2_PENDING under the state lock (re-validate first).
        let expected_version = {
            let mut state = self.state.write().await;
            if state.strategy_state != StrategyState::Leg1Filled {
                return Err(PloyError::InvalidStateTransition {
                    from: state.strategy_state.to_string(),
                    to: "LEG2_PENDING".to_string(),
                });
            }
            let Some(active) = state.current_cycle.as_ref() else {
                return Err(PloyError::Internal("No active cycle".to_string()));
            };
            if active.cycle_id != ctx.cycle_id {
                return Err(PloyError::InvalidState(format!(
                    "Active cycle changed before Leg2 submission (expected {}, got {})",
                    ctx.cycle_id, active.cycle_id
                )));
            }

            let expected_version = state.version;
            state.strategy_state = StrategyState::Leg2Pending;
            if let Some(active) = state.current_cycle.as_mut() {
                active.leg2_order_id = Some(request.client_order_id.clone());
            }
            state.version += 1;
            expected_version
        };

        // Persist state transition (best effort).
        self.persist_strategy_state_best_effort(
            StrategyState::Leg2Pending,
            round.id,
            Some(ctx.cycle_id),
        )
        .await;

        // Persist cycle state for crash recovery.
        let _ = self
            .store
            .update_cycle_state(ctx.cycle_id, StrategyState::Leg2Pending)
            .await;

        // Persist the intent before submitting to the exchange (best effort).
        let client_order_id = request.client_order_id.clone();
        let order = Order::from_request(&request, Some(ctx.cycle_id), 2);
        if let Err(err) = self.store.insert_order(&order).await {
            // Exposure exists (Leg1) and we refuse to submit Leg2 if we can't persist it.
            let unwind_summary = match self
                .unwind_leg1_exposure(&ctx, &round, ctx.leg1_shares)
                .await
            {
                Ok(s) => s,
                Err(e) => format!("unwind failed: {}", e),
            };

            let reason = format!("Failed to persist Leg2 order; {}", unwind_summary);
            if let Err(e) = self.store.abort_cycle(ctx.cycle_id, &reason).await {
                error!("Failed to abort cycle {} in DB: {}", ctx.cycle_id, e);
            }
            self.risk_manager
                .record_failure("Failed to persist Leg2 order")
                .await;
            self.persist_halt_if_needed().await;

            let today = Utc::now().date_naive();
            if let Err(e) = self.store.record_cycle_abort(today).await {
                error!("Failed to record cycle abort: {}", e);
            }

            let halt_reason = "Failed to persist Leg2 order - open exposure";
            self.risk_manager.trigger_circuit_breaker(halt_reason).await;
            if let Err(e) = self.store.halt_trading(today, halt_reason).await {
                error!("Failed to persist halt_trading: {}", e);
            }
            self.persist_halt_if_needed().await;

            {
                let mut state = self.state.write().await;
                state.strategy_state = StrategyState::Abort;
                state.current_cycle = None;
                state.version += 1;
            }

            self.persist_strategy_state_best_effort(StrategyState::Abort, round.id, None)
                .await;

            return Err(err);
        }

        let result = match self.executor.execute(&request).await {
            Ok(r) => r,
            Err(e) => {
                // Open exposure exists (Leg1). Best-effort unwind before halting.
                let unwind_shares = ctx.leg1_shares;
                let unwind_summary =
                    match self.unwind_leg1_exposure(&ctx, &round, unwind_shares).await {
                        Ok(s) => s,
                        Err(err) => format!("unwind failed: {}", err),
                    };

                let reason = format!("Leg2 execution failed; {}", unwind_summary);
                if let Err(err) = self.store.abort_cycle(ctx.cycle_id, &reason).await {
                    error!("Failed to abort cycle {} in DB: {}", ctx.cycle_id, err);
                }
                self.risk_manager
                    .record_failure("Leg2 execution failed")
                    .await;

                let today = Utc::now().date_naive();
                if let Err(e) = self.store.record_cycle_abort(today).await {
                error!("Failed to record cycle abort: {}", e);
            }

                let halt_reason = "Leg2 execution failed - open exposure";
                self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                if let Err(e) = self.store.halt_trading(today, halt_reason).await {
                    error!("Failed to persist halt_trading: {}", e);
                }
                self.persist_halt_if_needed().await;

                {
                    let mut state = self.state.write().await;
                    state.strategy_state = StrategyState::Abort;
                    state.current_cycle = None;
                    state.version += 1;
                }

                self.persist_strategy_state_best_effort(StrategyState::Abort, round.id, None)
                    .await;

                return Err(e);
            }
        };

        // Persist order submission + outcome (best effort).
        let _ = self
            .store
            .update_order_status(
                &client_order_id,
                OrderStatus::Submitted,
                Some(&result.order_id),
            )
            .await;

        if result.filled_shares > 0 {
            let fill_price = result.avg_fill_price.unwrap_or(request.limit_price);
            let _ = self
                .store
                .update_order_fill(
                    &client_order_id,
                    result.filled_shares,
                    fill_price,
                    result.status,
                )
                .await;
        } else {
            let _ = self
                .store
                .update_order_status(&client_order_id, result.status, None)
                .await;
        }

        // Ensure state hasn't been modified by another task while the order was executing.
        {
            let state = self.state.read().await;
            if state.version != expected_version + 1 {
                let observed_version = state.version;
                warn!(
                    "State version mismatch in Leg2: expected {}, got {}. Another thread modified state during order execution.",
                    expected_version + 1,
                    observed_version
                );
                // Abort + halt: this indicates a serious race that can lead to untracked exposure.
                if let Err(e) = self
                    .store
                    .abort_cycle(ctx.cycle_id, "State modified by concurrent operation")
                    .await
                {
                    error!("Failed to abort cycle {} in DB: {}", ctx.cycle_id, e);
                }

                let halt_reason = "Concurrent state modification detected";
                self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                self.persist_halt_if_needed().await;
                self.persist_strategy_state_best_effort(StrategyState::Abort, round.id, None)
                    .await;
                return Err(PloyError::Internal(
                    "Concurrent state modification detected during Leg2 execution".to_string(),
                ));
            }
        }

        if result.filled_shares == ctx.leg1_shares {
            let fill_price = result.avg_fill_price.unwrap_or(order_price);

            // Calculate PnL using centralized calculator
            let net_pnl =
                self.calculator
                    .expected_pnl(result.filled_shares, ctx.leg1_price, fill_price);

            // Update database
            let mut cycle_update_error: Option<PloyError> = None;
            if let Err(err) = self
                .store
                .update_cycle_leg2(ctx.cycle_id, fill_price, result.filled_shares, net_pnl)
                .await
            {
                error!(
                    "Failed to update cycle {} after Leg2 fill: {}",
                    ctx.cycle_id, err
                );
                cycle_update_error = Some(err);
            }

            // Record success
            self.risk_manager.record_success(net_pnl).await;
            self.persist_halt_if_needed().await;

            // Update daily metrics
            let today = Utc::now().date_naive();
            if let Err(e) = self.store.record_cycle_completion(today, net_pnl).await {
                error!("Failed to record cycle completion: {}", e);
            }

            if cycle_update_error.is_some() {
                let halt_reason = "DB update failed after Leg2 fill";
                self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                if let Err(e) = self.store.halt_trading(today, halt_reason).await {
                    error!("Failed to persist halt_trading: {}", e);
                }
                self.persist_halt_if_needed().await;
            }

            {
                let mut state = self.state.write().await;
                if state.version != expected_version + 1 {
                    let observed_version = state.version;
                    drop(state);
                    warn!(
                        "State version mismatch after Leg2 DB update: expected {}, got {}",
                        expected_version + 1,
                        observed_version
                    );
                    if let Err(e) = self
                        .store
                        .abort_cycle(ctx.cycle_id, "State modified by concurrent operation")
                        .await
                    {
                        error!("Failed to abort cycle {} in DB: {}", ctx.cycle_id, e);
                    }

                    let halt_reason = "Concurrent state modification detected";
                    self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                    self.persist_halt_if_needed().await;
                    self.persist_strategy_state_best_effort(StrategyState::Abort, round.id, None)
                        .await;
                    return Err(PloyError::Internal(
                        "Concurrent state modification detected during Leg2 execution".to_string(),
                    ));
                }

                state.strategy_state = StrategyState::CycleComplete;
                if let Some(active) = state.current_cycle.as_mut() {
                    active.leg2_order_id = Some(result.order_id.clone());
                }
                state.version += 1;
            }

            self.persist_strategy_state_best_effort(
                StrategyState::CycleComplete,
                round.id,
                Some(ctx.cycle_id),
            )
            .await;

            info!(
                "Leg2 filled: {} shares @ {}. Cycle PnL: {}",
                result.filled_shares, fill_price, net_pnl
            );

            if let Some(err) = cycle_update_error {
                return Err(err);
            }
        } else {
            // Leg2 failed (or only partially filled) - this is bad, we have open exposure.
            error!(
                "Leg2 not fully filled - open exposure (filled {}, expected {}, status {:?})",
                result.filled_shares, ctx.leg1_shares, result.status
            );

            let today = Utc::now().date_naive();
            let unhedged = ctx.leg1_shares.saturating_sub(result.filled_shares);
            let unwind_summary = if unhedged > 0 {
                match self.unwind_leg1_exposure(&ctx, &round, unhedged).await {
                    Ok(s) => s,
                    Err(err) => format!("unwind failed: {}", err),
                }
            } else {
                "unwind skipped (no unhedged shares)".to_string()
            };

            let reason = format!(
                "Leg2 not fully filled (filled {}, expected {}, {:?}); {}",
                result.filled_shares, ctx.leg1_shares, result.status, unwind_summary
            );

            if let Err(err) = self.store.abort_cycle(ctx.cycle_id, &reason).await {
                error!("Failed to abort cycle {} in DB: {}", ctx.cycle_id, err);
            }
            self.risk_manager
                .record_failure("Leg2 not fully filled")
                .await;

            if let Err(e) = self.store.record_cycle_abort(today).await {
                error!("Failed to record cycle abort: {}", e);
            }

            // Halt trading for manual intervention (exposure may exist).
            let halt_reason = "Leg2 not fully filled - open exposure";
            self.risk_manager.trigger_circuit_breaker(halt_reason).await;
            if let Err(e) = self.store.halt_trading(today, halt_reason).await {
                error!("Failed to persist halt_trading: {}", e);
            }
            self.persist_halt_if_needed().await;

            {
                let mut state = self.state.write().await;
                state.strategy_state = StrategyState::Abort;
                state.current_cycle = None;
                state.version += 1;
            }

            self.persist_strategy_state_best_effort(StrategyState::Abort, round.id, None)
                .await;
        }

        Ok(())
    }

    /// Best-effort unwind for unhedged Leg1 exposure.
    ///
    /// Caller must already have decided that exposure exists. This method submits a SELL IOC
    /// on the Leg1 token to reduce directional risk. Failures are returned for the caller to
    /// include in abort reasons / alerts.
    async fn unwind_leg1_exposure(
        &self,
        ctx: &CycleContext,
        round: &Round,
        shares_to_unwind: u64,
    ) -> Result<String> {
        if shares_to_unwind == 0 {
            return Ok("unwind skipped (0 shares)".to_string());
        }

        let token_id = round.token_id(ctx.leg1_side).to_string();

        // Prefer WS quote cache (has depth), but fall back to REST prices if stale/unavailable.
        let mut best_bid: Option<Decimal> = None;
        let mut best_ask: Option<Decimal> = None;
        let mut bid_size: Option<Decimal> = None;
        let mut ask_size: Option<Decimal> = None;

        if self
            .quote_cache
            .validate_freshness(&token_id, self.config.execution.max_quote_age_secs)
            .await
            .is_ok()
        {
            if let Some(q) = self.quote_cache.get(&token_id) {
                best_bid = q.best_bid;
                best_ask = q.best_ask;
                bid_size = q.bid_size;
                ask_size = q.ask_size;
            }
        }

        if best_bid.is_none() {
            let (bid, ask) = self.executor.get_prices(&token_id).await?;
            best_bid = bid;
            best_ask = ask;
        }

        let best_bid = best_bid.ok_or_else(|| {
            PloyError::MarketDataUnavailable(format!("Missing bid for unwind token {}", token_id))
        })?;
        let best_ask = best_ask.unwrap_or(best_bid);

        let depth = MarketDepth {
            best_bid,
            best_ask,
            bid_size: bid_size.unwrap_or(Decimal::ZERO),
            ask_size: ask_size.unwrap_or(Decimal::ZERO),
        };

        let order_size = Decimal::from(shares_to_unwind);
        let limit_price = match self.slippage.check_sell_order(&depth, order_size, best_bid) {
            SlippageCheck::Approved { limit_price, .. } => limit_price,
            SlippageCheck::Rejected { reason, .. } => {
                warn!(
                    "Unwind slippage/depth check rejected: {}. Proceeding best-effort.",
                    reason
                );
                // Mirror the -0.1% buffer used in slippage module to improve fill probability.
                best_bid * (Decimal::ONE - Decimal::new(1, 3))
            }
        }
        .max(Decimal::ZERO)
        .min(Decimal::ONE);

        info!(
            "Attempting unwind: SELL {} shares of {} @ {} (best_bid={})",
            shares_to_unwind, token_id, limit_price, best_bid
        );

        let mut request = crate::domain::OrderRequest::sell_limit(
            token_id.clone(),
            ctx.leg1_side,
            shares_to_unwind,
            limit_price,
        );
        request.time_in_force = TimeInForce::IOC;

        // Persist the intent (best effort) before submitting to the exchange.
        let client_order_id = request.client_order_id.clone();
        let order = Order::from_request(&request, Some(ctx.cycle_id), 1);
        if let Err(e) = self.store.insert_order(&order).await {
            error!("Failed to persist unwind order (cycle {}): {}", ctx.cycle_id, e);
        }

        let result = self.executor.execute(&request).await?;

        // Persist order submission + outcome (best effort).
        let _ = self
            .store
            .update_order_status(
                &client_order_id,
                OrderStatus::Submitted,
                Some(&result.order_id),
            )
            .await;

        if result.filled_shares > 0 {
            let fill_price = result.avg_fill_price.unwrap_or(limit_price);
            let _ = self
                .store
                .update_order_fill(
                    &client_order_id,
                    result.filled_shares,
                    fill_price,
                    result.status,
                )
                .await;
        } else {
            let _ = self
                .store
                .update_order_status(&client_order_id, result.status, None)
                .await;
        }

        Ok(format!(
            "unwind: sold {} of {} shares (status={:?}, avg_fill_price={:?})",
            result.filled_shares, shares_to_unwind, result.status, result.avg_fill_price
        ))
    }

    async fn persist_halt_if_needed(&self) {
        if self.risk_manager.can_trade().await {
            return;
        }

        let today = Utc::now().date_naive();
        let reason = self
            .risk_manager
            .halt_reason()
            .await
            .unwrap_or_else(|| "Risk circuit breaker triggered".to_string());

        if let Err(e) = self.store.halt_trading(today, &reason).await {
            error!("Failed to persist trading halt to DB: {}", e);
        }
    }

    async fn persist_strategy_state_best_effort(
        &self,
        state: StrategyState,
        round_id: Option<i32>,
        cycle_id: Option<i32>,
    ) {
        if let Err(e) = self
            .store
            .update_strategy_state(state, round_id, cycle_id)
            .await
        {
            error!("Failed to persist strategy_state to DB: {}", e);
        }
    }

    /// Abort the current cycle and halt trading.
    ///
    /// If we're in `LEG1_FILLED` and no Leg2 has been started, this will attempt a best-effort
    /// unwind (SELL IOC) to reduce directional exposure before halting.
    async fn abort_cycle_and_halt_safely(&self, reason: &str) -> Result<()> {
        let _exec_guard = self.execution_mutex.lock().await;

        let (strategy_state, round, ctx) = {
            let state = self.state.read().await;
            (
                state.strategy_state,
                state.current_round.clone(),
                state.current_cycle.clone(),
            )
        };

        let (cycle_id, full_reason) = match (ctx.as_ref(), round.as_ref()) {
            (Some(ctx), Some(round)) => {
                let mut full_reason = reason.to_string();

                // Only unwind if we know Leg2 hasn't been started.
                if strategy_state == StrategyState::Leg1Filled && ctx.leg2_order_id.is_none() {
                    match self.unwind_leg1_exposure(ctx, round, ctx.leg1_shares).await {
                        Ok(summary) => full_reason = format!("{}; {}", reason, summary),
                        Err(e) => full_reason = format!("{}; unwind failed: {}", reason, e),
                    }
                }

                (Some(ctx.cycle_id), full_reason)
            }
            _ => (ctx.as_ref().map(|c| c.cycle_id), reason.to_string()),
        };

        // Persist abort + metrics best effort (exposure handling and halting is higher priority).
        if let Some(cycle_id) = cycle_id {
            if let Err(err) = self.store.abort_cycle(cycle_id, &full_reason).await {
                error!("Failed to abort cycle {} in DB: {}", cycle_id, err);
            }

            self.risk_manager.record_failure(reason).await;

            let today = Utc::now().date_naive();
            if let Err(e) = self.store.record_cycle_abort(today).await {
                error!("Failed to record cycle abort: {}", e);
            }
            if let Err(e) = self.store.halt_trading(today, reason).await {
                error!("Failed to persist halt_trading: {}", e);
            }
        } else {
            let today = Utc::now().date_naive();
            if let Err(e) = self.store.halt_trading(today, reason).await {
                error!("Failed to persist halt_trading: {}", e);
            }
        }

        // Always halt trading on aborts that indicate potential exposure or operational risk.
        self.risk_manager.trigger_circuit_breaker(reason).await;
        self.persist_halt_if_needed().await;

        {
            let mut state = self.state.write().await;
            state.strategy_state = StrategyState::Abort;
            state.current_cycle = None;
            state.version += 1;
        }

        self.persist_strategy_state_best_effort(
            StrategyState::Abort,
            round.as_ref().and_then(|r| r.id),
            None,
        )
        .await;

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
            if let Err(e) = self.enter_leg2_forced(opposite_side, forced_price).await {
                error!("Forced Leg2 failed: {}", e);
                self.abort_cycle_and_halt_safely("Forced Leg2 failed")
                    .await?;
            }
        } else {
            // No quote available, must abort
            self.abort_cycle_and_halt_safely("No quote for forced Leg2")
                .await?;
        }

        Ok(())
    }

    /// Abort the current cycle
    async fn abort_cycle(&self, reason: &str) -> Result<()> {
        let (cycle_id, round_id) = {
            let mut state = self.state.write().await;
            let cycle_id = state.current_cycle.as_ref().map(|c| c.cycle_id);
            let round_id = state.current_round.as_ref().and_then(|r| r.id);
            state.strategy_state = StrategyState::Abort;
            state.current_cycle = None;
            state.version += 1;
            (cycle_id, round_id)
        };

        if let Some(cycle_id) = cycle_id {
            warn!("Aborting cycle {}: {}", cycle_id, reason);
            if let Err(e) = self.store.abort_cycle(cycle_id, reason).await {
                error!("Failed to abort cycle {} in DB: {}", cycle_id, e);
                let halt_reason = "Database error during abort_cycle";
                self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                self.persist_halt_if_needed().await;
            }
            self.risk_manager.record_failure(reason).await;
            self.persist_halt_if_needed().await;

            let today = Utc::now().date_naive();
            if let Err(e) = self.store.record_cycle_abort(today).await {
                error!("Failed to record cycle abort metric: {}", e);
                let halt_reason = "Database error during record_cycle_abort";
                self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                self.persist_halt_if_needed().await;
            }
        }

        self.persist_strategy_state_best_effort(StrategyState::Abort, round_id, None)
            .await;

        Ok(())
    }

    /// Abort the current cycle without recording a risk failure.
    ///
    /// This is for expected/neutral aborts where no exposure exists (e.g. an IOC order got 0 fill).
    async fn abort_cycle_neutral(&self, reason: &str) -> Result<()> {
        let (cycle_id, round_id) = {
            let mut state = self.state.write().await;
            let cycle_id = state.current_cycle.as_ref().map(|c| c.cycle_id);
            let round_id = state.current_round.as_ref().and_then(|r| r.id);
            state.strategy_state = StrategyState::Abort;
            state.current_cycle = None;
            state.version += 1;
            (cycle_id, round_id)
        };

        if let Some(cycle_id) = cycle_id {
            warn!("Aborting cycle {} (neutral): {}", cycle_id, reason);

            if let Err(e) = self.store.abort_cycle(cycle_id, reason).await {
                error!("Failed to abort cycle {} in DB: {}", cycle_id, e);
                // Operational DB error: halt to avoid blind state.
                let halt_reason = "Database error during abort_cycle_neutral";
                self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                self.persist_halt_if_needed().await;
            }

            let today = Utc::now().date_naive();
            if let Err(e) = self.store.record_cycle_abort_neutral(today).await {
                error!("Failed to record neutral cycle abort metric: {}", e);
                let halt_reason = "Database error during record_cycle_abort_neutral";
                self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                self.persist_halt_if_needed().await;
            }
        }

        // Persist strategy state for observability/crash recovery (best effort).
        self.persist_strategy_state_best_effort(StrategyState::Abort, round_id, None)
            .await;

        // If something else already halted trading, ensure it's persisted.
        self.persist_halt_if_needed().await;

        Ok(())
    }

    /// Transition back to idle state
    async fn transition_to_idle(&self) -> Result<()> {
        {
            let mut state = self.state.write().await;
            state.strategy_state = StrategyState::Idle;
            state.current_cycle = None;
            state.current_round = None;
            state.version += 1;
        }

        self.persist_strategy_state_best_effort(StrategyState::Idle, None, None)
            .await;

        // Reset signal detector
        {
            let mut detector = self.signal_detector.write().await;
            detector.reset(None);
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{
        BalanceResponse, MarketResponse, MarketSummary, OrderResponse, PositionResponse,
        TradeResponse,
    };
    use crate::config::AppConfig;
    use crate::domain::Round;
    use crate::exchange::{ExchangeClient, ExchangeKind};
    use crate::strategy::engine_store::mock::MockStore;
    use async_trait::async_trait;
    use chrono::{Duration, Utc};
    use rust_decimal_macros::dec;

    // ───────────────────── Mock Exchange Client ─────────────────────

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

        async fn get_best_prices(
            &self,
            _token_id: &str,
        ) -> Result<(Option<Decimal>, Option<Decimal>)> {
            Ok((Some(dec!(0.48)), Some(dec!(0.52))))
        }

        fn infer_order_status(&self, _order: &OrderResponse) -> OrderStatus {
            OrderStatus::Filled
        }

        fn calculate_fill(&self, _order: &OrderResponse) -> (u64, Option<Decimal>) {
            (100, Some(dec!(0.50)))
        }
    }

    // ───────────────────── Test helpers ─────────────────────

    /// Minimal config that passes the safety guard (dry_run=true).
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
        let executor = OrderExecutor::new_with_exchange(
            Arc::new(MockExchangeClient),
            config.execution.clone(),
        );
        let quote_cache = QuoteCache::new();
        StrategyEngine::new(config, MockStore::new(), executor, quote_cache)
            .await
            .expect("engine should construct")
    }

    // ───────────────────── Tests ─────────────────────

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

        // Same slug → no-op, version unchanged
        engine.set_round(round).await.unwrap();
        let v2 = engine.state.read().await.version;
        assert_eq!(v1, v2, "version should not change on duplicate round");
    }

    #[tokio::test]
    async fn set_round_expired_stays_idle() {
        let engine = test_engine().await;
        let round = expired_round();
        engine.set_round(round).await.unwrap();
        // Round already past window → stays Idle (or becomes Idle via has_ended())
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
        // Move to WatchWindow first
        let round = test_round(15);
        engine.set_round(round).await.unwrap();
        assert_eq!(engine.state().await, StrategyState::WatchWindow);

        // Transition to idle
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
        // Abort with no active cycle should still move to Abort state
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

        // Simulate a mid-cycle state by writing directly
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
            });
        }

        // Try to set a different round — should be rejected (mid-cycle)
        let new_round = Round {
            slug: "test-btc-different".to_string(),
            ..test_round(15)
        };
        engine.set_round(new_round).await.unwrap();

        // State should still be Leg1Filled (round change ignored)
        assert_eq!(engine.state().await, StrategyState::Leg1Filled);
    }

    #[tokio::test]
    async fn abort_cycle_with_active_cycle_clears_context() {
        let engine = test_engine().await;
        let round = test_round(15);
        engine.set_round(round).await.unwrap();

        // Simulate active cycle
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

        let executor = OrderExecutor::new_with_exchange(
            Arc::new(MockExchangeClient),
            config.execution.clone(),
        );
        let result = StrategyEngine::new(config, MockStore::new(), executor, QuoteCache::new()).await;
        assert!(result.is_err(), "should reject live mode without confirm_fills");
    }
}
