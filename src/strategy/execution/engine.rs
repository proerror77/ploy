use super::engine_store::EngineStore;
use crate::adapters::{QuoteCache, QuoteUpdate};
use crate::config::AppConfig;
use crate::domain::{Order, OrderStatus, Round, Side, StrategyState, TimeInForce};
use crate::error::{PloyError, Result};
use crate::strategy::{
    MarketDepth, OrderExecutor, RiskManager, SignalDetector, SlippageCheck, SlippageConfig,
    SlippageProtection, TradingCalculator,
};
use chrono::Utc;
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use tracing::{debug, error, info, warn};

mod hedge_flow;
mod lifecycle;

use lifecycle::{
    abort_cycle as abort_cycle_impl,
    abort_cycle_and_halt_safely as abort_cycle_and_halt_safely_impl,
    abort_cycle_neutral as abort_cycle_neutral_impl,
    force_leg2_or_abort as force_leg2_or_abort_impl,
    persist_halt_if_needed as persist_halt_if_needed_impl,
    persist_strategy_state_best_effort as persist_strategy_state_best_effort_impl,
    transition_to_idle as transition_to_idle_impl,
};

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
    #[allow(dead_code)]
    leg1_order_id: String,
    leg2_order_id: Option<String>,
    /// Guard against duplicate forced Leg2 submissions from concurrent paths.
    force_leg2_attempted: bool,
    /// DB row version for optimistic locking (cycles.version column)
    cycle_version: i32,
}

/// `cycles.version` starts at 1 in the database migration.
const INITIAL_CYCLE_DB_VERSION: i32 = 1;

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
        let (cycle_id, expected_version, cycle_version) = {
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
            let cycle_version = INITIAL_CYCLE_DB_VERSION;
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
                cycle_version,
            });
            state.version += 1;

            (cycle_id, expected_version, cycle_version)
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
        let order = Order::from_request(&request, Some(cycle_id), 1, None);
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

            // Update database with optimistic locking.
            // `Ok(false)` is a version conflict and must be handled as a hard failure.
            let leg1_update_error = match self
                .store
                .update_cycle_leg1(
                    cycle_id,
                    side,
                    fill_price,
                    result.filled_shares,
                    cycle_version,
                )
                .await
            {
                Ok(true) => None,
                Ok(false) => Some(PloyError::InvalidState(format!(
                    "Cycle {} version conflict while persisting Leg1 fill",
                    cycle_id
                ))),
                Err(err) => Some(err),
            };

            if let Some(err) = leg1_update_error {
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
                    cycle_version,
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
                    // `INITIAL_CYCLE_DB_VERSION` -> +1 after Leg1 fill.
                    cycle_version: cycle_version + 1,
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

    async fn persist_halt_if_needed(&self) {
        persist_halt_if_needed_impl(self).await
    }

    async fn persist_strategy_state_best_effort(
        &self,
        state: StrategyState,
        round_id: Option<i32>,
        cycle_id: Option<i32>,
    ) {
        persist_strategy_state_best_effort_impl(self, state, round_id, cycle_id).await
    }

    /// Abort the current cycle and halt trading.
    ///
    /// If we're in `LEG1_FILLED` and no Leg2 has been started, this will attempt a best-effort
    /// unwind (SELL IOC) to reduce directional exposure before halting.
    async fn abort_cycle_and_halt_safely(&self, reason: &str) -> Result<()> {
        abort_cycle_and_halt_safely_impl(self, reason).await
    }

    /// Force Leg2 or abort when time is running out
    async fn force_leg2_or_abort(&self) -> Result<()> {
        force_leg2_or_abort_impl(self).await
    }

    /// Abort the current cycle
    async fn abort_cycle(&self, reason: &str) -> Result<()> {
        abort_cycle_impl(self, reason).await
    }

    /// Abort the current cycle without recording a risk failure.
    ///
    /// This is for expected/neutral aborts where no exposure exists (e.g. an IOC order got 0 fill).
    async fn abort_cycle_neutral(&self, reason: &str) -> Result<()> {
        abort_cycle_neutral_impl(self, reason).await
    }

    /// Transition back to idle state
    async fn transition_to_idle(&self) -> Result<()> {
        transition_to_idle_impl(self).await
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
    use crate::adapters::OrderResponse;
    use crate::config::AppConfig;
    use crate::domain::Round;
    use crate::exchange::{ExchangeClient, ExchangeKind};
    use crate::strategy::execution::engine_store::mock::MockStore;
    use async_trait::async_trait;
    use chrono::{Duration, NaiveDate, Utc};
    use rust_decimal_macros::dec;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::sync::Mutex;

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

    async fn test_engine_with_store(store: impl EngineStore + 'static) -> StrategyEngine {
        let config = test_config();
        let executor = OrderExecutor::new_with_exchange(
            Arc::new(MockExchangeClient),
            config.execution.clone(),
        );
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
                cycle_version: 0,
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

        let executor = OrderExecutor::new_with_exchange(
            Arc::new(MockExchangeClient),
            config.execution.clone(),
        );
        let result =
            StrategyEngine::new(config, MockStore::new(), executor, QuoteCache::new()).await;
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
}
