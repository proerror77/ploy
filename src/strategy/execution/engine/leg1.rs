use super::{CycleContext, StrategyEngine, INITIAL_CYCLE_DB_VERSION};
use crate::domain::{Order, OrderStatus, Side, StrategyState, TimeInForce};
use crate::error::{PloyError, Result};
use crate::strategy::{MarketDepth, SlippageCheck};
use chrono::Utc;
use rust_decimal::Decimal;
use tracing::{debug, error, info, warn};

/// Enter Leg1 position.
pub(super) async fn enter_leg1(engine: &StrategyEngine, side: Side, price: Decimal) -> Result<()> {
    let _exec_guard = engine.execution_mutex.lock().await;

    let (round, round_id) = {
        let state = engine.state.read().await;
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

    engine
        .quote_cache
        .validate_freshness(&token_id, engine.config.execution.max_quote_age_secs)
        .await?;

    let quote = engine
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

    let order_size = Decimal::from(engine.config.strategy.shares);
    let mut order_price = match engine.slippage.check_buy_order(&depth, order_size, price) {
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

    order_price = order_price.max(price).min(Decimal::ONE);

    engine
        .risk_manager
        .check_leg1_entry(engine.config.strategy.shares, order_price, &round)
        .await?;

    let mut request = crate::domain::OrderRequest::buy_limit(
        token_id.clone(),
        side,
        engine.config.strategy.shares,
        order_price,
    );
    request.time_in_force = TimeInForce::IOC;

    let (cycle_id, expected_version, cycle_version) = {
        let mut state = engine.state.write().await;

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
            || current_round.minutes_elapsed() >= engine.config.strategy.window_min as i64
        {
            return Err(PloyError::InvalidState(format!(
                "Round {} is no longer within the entry window",
                current_round.slug
            )));
        }

        let cycle_id = engine
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
            leg1_shares: engine.config.strategy.shares,
            leg1_order_id: request.client_order_id.clone(),
            leg2_order_id: None,
            force_leg2_attempted: false,
            cycle_version,
        });
        state.version += 1;

        (cycle_id, expected_version, cycle_version)
    };

    engine
        .persist_strategy_state_best_effort(
            StrategyState::Leg1Pending,
            Some(round_id),
            Some(cycle_id),
        )
        .await;

    let today = Utc::now().date_naive();
    if let Err(e) = engine.store.increment_cycle_count(today).await {
        error!("Failed to increment cycle count: {}", e);
    }

    info!(
        "Entering Leg1: {} {} shares of {} @ {}",
        side, engine.config.strategy.shares, token_id, order_price
    );

    let client_order_id = request.client_order_id.clone();
    let order = Order::from_request(&request, Some(cycle_id), 1, None);
    if let Err(e) = engine.store.insert_order(&order).await {
        let halt_reason = "Failed to persist Leg1 order";
        engine
            .risk_manager
            .trigger_circuit_breaker(halt_reason)
            .await;
        engine.persist_halt_if_needed().await;
        engine.abort_cycle(halt_reason).await?;
        return Err(e);
    }

    let result = match engine.executor.execute(&request).await {
        Ok(r) => r,
        Err(e) => {
            let _ = engine
                .store
                .update_order_status(&client_order_id, OrderStatus::Failed, None)
                .await;
            let halt_reason = "Leg1 execution failed";
            engine
                .risk_manager
                .trigger_circuit_breaker(halt_reason)
                .await;
            engine.persist_halt_if_needed().await;
            engine.abort_cycle(halt_reason).await?;
            return Err(e);
        }
    };

    let _ = engine
        .store
        .update_order_status(
            &client_order_id,
            OrderStatus::Submitted,
            Some(&result.order_id),
        )
        .await;

    if result.filled_shares > 0 {
        let fill_price = result.avg_fill_price.unwrap_or(request.limit_price);
        let _ = engine
            .store
            .update_order_fill(
                &client_order_id,
                result.filled_shares,
                fill_price,
                result.status,
            )
            .await;
    } else {
        let _ = engine
            .store
            .update_order_status(&client_order_id, result.status, None)
            .await;
    }

    {
        let state = engine.state.read().await;
        if state.version != expected_version + 1 {
            let observed_version = state.version;
            warn!(
                    "State version mismatch: expected {}, got {}. Another thread modified state during order execution.",
                    expected_version + 1,
                    observed_version
                );
            if let Err(e) = engine
                .store
                .abort_cycle(cycle_id, "State modified by concurrent operation")
                .await
            {
                error!("Failed to abort cycle {} in DB: {}", cycle_id, e);
            }

            let halt_reason = "Concurrent state modification detected";
            engine
                .risk_manager
                .trigger_circuit_breaker(halt_reason)
                .await;
            engine.persist_halt_if_needed().await;
            engine
                .persist_strategy_state_best_effort(StrategyState::Abort, Some(round_id), None)
                .await;
            return Err(PloyError::Internal(
                "Concurrent state modification detected during Leg1 execution".to_string(),
            ));
        }
    }

    if result.filled_shares > 0 {
        let fill_price = result.avg_fill_price.unwrap_or(order_price);

        let leg1_update_error = match engine
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

            let unwind_summary = match engine
                .unwind_leg1_exposure(&unwind_ctx, &round, result.filled_shares)
                .await
            {
                Ok(s) => s,
                Err(e) => format!("unwind failed: {}", e),
            };

            let today = Utc::now().date_naive();
            let halt_reason = "DB update failed after Leg1 fill - exposure may exist";
            engine
                .risk_manager
                .trigger_circuit_breaker(halt_reason)
                .await;
            if let Err(e) = engine.store.halt_trading(today, halt_reason).await {
                error!("Failed to persist halt_trading: {}", e);
            }
            engine.persist_halt_if_needed().await;
            let _ = engine
                .store
                .abort_cycle(cycle_id, &format!("{}; {}", halt_reason, unwind_summary))
                .await;

            {
                let mut state = engine.state.write().await;
                state.strategy_state = StrategyState::Abort;
                state.current_cycle = None;
                state.version += 1;
            }

            engine
                .persist_strategy_state_best_effort(StrategyState::Abort, Some(round_id), None)
                .await;

            return Err(err);
        }

        {
            let mut state = engine.state.write().await;
            if state.version != expected_version + 1 {
                let observed_version = state.version;
                drop(state);
                warn!(
                    "State version mismatch after Leg1 DB update: expected {}, got {}",
                    expected_version + 1,
                    observed_version
                );
                if let Err(e) = engine
                    .store
                    .abort_cycle(cycle_id, "State modified by concurrent operation")
                    .await
                {
                    error!("Failed to abort cycle {} in DB: {}", cycle_id, e);
                }

                let halt_reason = "Concurrent state modification detected";
                engine
                    .risk_manager
                    .trigger_circuit_breaker(halt_reason)
                    .await;
                engine.persist_halt_if_needed().await;
                engine
                    .persist_strategy_state_best_effort(StrategyState::Abort, Some(round_id), None)
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
                cycle_version: cycle_version + 1,
            });

            state.strategy_state = StrategyState::Leg1Filled;
            state.version += 1;
        }

        engine
            .persist_strategy_state_best_effort(
                StrategyState::Leg1Filled,
                Some(round_id),
                Some(cycle_id),
            )
            .await;

        info!(
            "Leg1 filled: {} shares @ {}",
            result.filled_shares, fill_price
        );

        {
            let mut detector = engine.signal_detector.write().await;
            detector.mark_triggered(side);
        }
    } else {
        engine
            .abort_cycle_neutral(&format!("Leg1 not filled ({:?})", result.status))
            .await?;
        warn!("Leg1 order got 0 fill");
    }

    Ok(())
}
