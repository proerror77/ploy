use super::{CycleContext, StrategyEngine};
use crate::domain::{Order, OrderStatus, Round, Side, StrategyState, TimeInForce};
use crate::error::{PloyError, Result};
use crate::strategy::{MarketDepth, SlippageCheck};
use chrono::Utc;
use rust_decimal::Decimal;
use tracing::{debug, error, info, warn};

impl StrategyEngine {
    /// Enter Leg2 position
    pub(super) async fn enter_leg2(&self, side: Side, price: Decimal) -> Result<()> {
        self.enter_leg2_inner(side, price, false).await
    }

    /// Enter Leg2 position in forced mode.
    ///
    /// Forced mode is used near round end to reduce exposure. It must not depend on fresh WS
    /// order book data, since the timeout path is triggered specifically when WS quotes may
    /// not be arriving. Slippage/depth rejections are treated as warnings and executed
    /// best-effort (still with FOK to avoid partial hedges).
    pub(super) async fn enter_leg2_forced(&self, side: Side, price: Decimal) -> Result<()> {
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
                .is_some_and(|cycle| cycle.force_leg2_attempted)
            {
                warn!("Force Leg2 already attempted for this cycle; skipping duplicate");
                return Ok(());
            }
            drop(state);
            let mut state = self.state.write().await;
            if let Some(ref mut cycle) = state.current_cycle {
                cycle.force_leg2_attempted = true;
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

        let mut best_bid: Option<Decimal> = None;
        let mut best_ask: Option<Decimal> = None;
        let mut bid_size: Option<Decimal> = None;
        let mut ask_size: Option<Decimal> = None;

        if forced {
            if let Some(quote) = self.quote_cache.get(&token_id) {
                best_bid = quote.best_bid;
                best_ask = quote.best_ask;
                bid_size = quote.bid_size;
                ask_size = quote.ask_size;
            }
        } else {
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

        let max_leg2_price = if forced {
            (Decimal::ONE - ctx.leg1_price).min(Decimal::ONE)
        } else {
            Decimal::ONE
        };
        order_price = order_price.max(price).min(max_leg2_price);

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

        let (expected_version, cycle_state_expected_version, leg2_fill_expected_version) = {
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
            let active = state
                .current_cycle
                .as_mut()
                .ok_or_else(|| PloyError::Internal("No active cycle".to_string()))?;
            let cycle_state_expected_version = active.cycle_version;
            active.leg2_order_id = Some(request.client_order_id.clone());
            active.cycle_version += 1;
            let leg2_fill_expected_version = active.cycle_version;
            state.version += 1;
            (
                expected_version,
                cycle_state_expected_version,
                leg2_fill_expected_version,
            )
        };

        self.persist_strategy_state_best_effort(
            StrategyState::Leg2Pending,
            round.id,
            Some(ctx.cycle_id),
        )
        .await;

        let cycle_state_update_error = match self
            .store
            .update_cycle_state(
                ctx.cycle_id,
                StrategyState::Leg2Pending,
                cycle_state_expected_version,
            )
            .await
        {
            Ok(()) => None,
            Err(err) => Some(err),
        };

        if let Some(err) = cycle_state_update_error {
            let unwind_summary = match self
                .unwind_leg1_exposure(&ctx, &round, ctx.leg1_shares)
                .await
            {
                Ok(summary) => summary,
                Err(error) => format!("unwind failed: {}", error),
            };

            let reason = format!(
                "Failed to persist LEG2_PENDING (expected version {}): {}; {}",
                cycle_state_expected_version, err, unwind_summary
            );
            if let Err(error) = self.store.abort_cycle(ctx.cycle_id, &reason).await {
                error!("Failed to abort cycle {} in DB: {}", ctx.cycle_id, error);
            }
            self.risk_manager
                .record_failure("Failed to persist LEG2_PENDING state")
                .await;
            self.persist_halt_if_needed().await;

            let today = Utc::now().date_naive();
            if let Err(error) = self.store.record_cycle_abort(today).await {
                error!("Failed to record cycle abort: {}", error);
            }

            let halt_reason = "Failed to persist LEG2_PENDING state - open exposure";
            self.risk_manager.trigger_circuit_breaker(halt_reason).await;
            if let Err(error) = self.store.halt_trading(today, halt_reason).await {
                error!("Failed to persist halt_trading: {}", error);
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

        let client_order_id = request.client_order_id.clone();
        let order = Order::from_request(&request, Some(ctx.cycle_id), 2, None);
        if let Err(err) = self.store.insert_order(&order).await {
            let unwind_summary = match self
                .unwind_leg1_exposure(&ctx, &round, ctx.leg1_shares)
                .await
            {
                Ok(summary) => summary,
                Err(error) => format!("unwind failed: {}", error),
            };

            let reason = format!("Failed to persist Leg2 order; {}", unwind_summary);
            if let Err(error) = self.store.abort_cycle(ctx.cycle_id, &reason).await {
                error!("Failed to abort cycle {} in DB: {}", ctx.cycle_id, error);
            }
            self.risk_manager
                .record_failure("Failed to persist Leg2 order")
                .await;
            self.persist_halt_if_needed().await;

            let today = Utc::now().date_naive();
            if let Err(error) = self.store.record_cycle_abort(today).await {
                error!("Failed to record cycle abort: {}", error);
            }

            let halt_reason = "Failed to persist Leg2 order - open exposure";
            self.risk_manager.trigger_circuit_breaker(halt_reason).await;
            if let Err(error) = self.store.halt_trading(today, halt_reason).await {
                error!("Failed to persist halt_trading: {}", error);
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
            Ok(result) => result,
            Err(err) => {
                let unwind_shares = ctx.leg1_shares;
                let unwind_summary =
                    match self.unwind_leg1_exposure(&ctx, &round, unwind_shares).await {
                        Ok(summary) => summary,
                        Err(error) => format!("unwind failed: {}", error),
                    };

                let reason = format!("Leg2 execution failed; {}", unwind_summary);
                if let Err(error) = self.store.abort_cycle(ctx.cycle_id, &reason).await {
                    error!("Failed to abort cycle {} in DB: {}", ctx.cycle_id, error);
                }
                self.risk_manager
                    .record_failure("Leg2 execution failed")
                    .await;

                let today = Utc::now().date_naive();
                if let Err(error) = self.store.record_cycle_abort(today).await {
                    error!("Failed to record cycle abort: {}", error);
                }

                let halt_reason = "Leg2 execution failed - open exposure";
                self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                if let Err(error) = self.store.halt_trading(today, halt_reason).await {
                    error!("Failed to persist halt_trading: {}", error);
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
        };

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

        {
            let state = self.state.read().await;
            if state.version != expected_version + 1 {
                let observed_version = state.version;
                warn!(
                    "State version mismatch in Leg2: expected {}, got {}. Another thread modified state during order execution.",
                    expected_version + 1,
                    observed_version
                );
                if let Err(error) = self
                    .store
                    .abort_cycle(ctx.cycle_id, "State modified by concurrent operation")
                    .await
                {
                    error!("Failed to abort cycle {} in DB: {}", ctx.cycle_id, error);
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
            let net_pnl =
                self.calculator
                    .expected_pnl(result.filled_shares, ctx.leg1_price, fill_price);

            let cycle_update_error = match self
                .store
                .update_cycle_leg2(
                    ctx.cycle_id,
                    fill_price,
                    result.filled_shares,
                    net_pnl,
                    leg2_fill_expected_version,
                )
                .await
            {
                Ok(()) => None,
                Err(err) => Some(err),
            };

            if let Some(err) = cycle_update_error {
                error!(
                    "Failed to update cycle {} after Leg2 fill: {}",
                    ctx.cycle_id, err
                );

                let today = Utc::now().date_naive();
                if let Err(error) = self
                    .store
                    .abort_cycle(
                        ctx.cycle_id,
                        &format!("DB update failed after Leg2 fill: {}", err),
                    )
                    .await
                {
                    error!("Failed to abort cycle {} in DB: {}", ctx.cycle_id, error);
                }
                self.risk_manager
                    .record_failure("DB update failed after Leg2 fill")
                    .await;
                if let Err(error) = self.store.record_cycle_abort(today).await {
                    error!("Failed to record cycle abort: {}", error);
                }

                let halt_reason = "DB update failed after Leg2 fill";
                self.risk_manager.trigger_circuit_breaker(halt_reason).await;
                if let Err(error) = self.store.halt_trading(today, halt_reason).await {
                    error!("Failed to persist halt_trading: {}", error);
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

            self.risk_manager.record_success(net_pnl).await;
            self.persist_halt_if_needed().await;

            let today = Utc::now().date_naive();
            if let Err(error) = self.store.record_cycle_completion(today, net_pnl).await {
                error!("Failed to record cycle completion: {}", error);
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
                    if let Err(error) = self
                        .store
                        .abort_cycle(ctx.cycle_id, "State modified by concurrent operation")
                        .await
                    {
                        error!("Failed to abort cycle {} in DB: {}", ctx.cycle_id, error);
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
        } else {
            error!(
                "Leg2 not fully filled - open exposure (filled {}, expected {}, status {:?})",
                result.filled_shares, ctx.leg1_shares, result.status
            );

            let today = Utc::now().date_naive();
            let unhedged = ctx.leg1_shares.saturating_sub(result.filled_shares);
            let unwind_summary = if unhedged > 0 {
                match self.unwind_leg1_exposure(&ctx, &round, unhedged).await {
                    Ok(summary) => summary,
                    Err(error) => format!("unwind failed: {}", error),
                }
            } else {
                "unwind skipped (no unhedged shares)".to_string()
            };

            let reason = format!(
                "Leg2 not fully filled (filled {}, expected {}, {:?}); {}",
                result.filled_shares, ctx.leg1_shares, result.status, unwind_summary
            );

            if let Err(error) = self.store.abort_cycle(ctx.cycle_id, &reason).await {
                error!("Failed to abort cycle {} in DB: {}", ctx.cycle_id, error);
            }
            self.risk_manager
                .record_failure("Leg2 not fully filled")
                .await;

            if let Err(error) = self.store.record_cycle_abort(today).await {
                error!("Failed to record cycle abort: {}", error);
            }

            let halt_reason = "Leg2 not fully filled - open exposure";
            self.risk_manager.trigger_circuit_breaker(halt_reason).await;
            if let Err(error) = self.store.halt_trading(today, halt_reason).await {
                error!("Failed to persist halt_trading: {}", error);
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
    pub(super) async fn unwind_leg1_exposure(
        &self,
        ctx: &CycleContext,
        round: &Round,
        shares_to_unwind: u64,
    ) -> Result<String> {
        if shares_to_unwind == 0 {
            return Ok("unwind skipped (0 shares)".to_string());
        }

        let token_id = round.token_id(ctx.leg1_side).to_string();

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
            if let Some(quote) = self.quote_cache.get(&token_id) {
                best_bid = quote.best_bid;
                best_ask = quote.best_ask;
                bid_size = quote.bid_size;
                ask_size = quote.ask_size;
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

        let client_order_id = request.client_order_id.clone();
        let order = Order::from_request(&request, Some(ctx.cycle_id), 1, None);
        if let Err(error) = self.store.insert_order(&order).await {
            error!(
                "Failed to persist unwind order (cycle {}): {}",
                ctx.cycle_id, error
            );
        }

        let result = self.executor.execute(&request).await?;

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
}
