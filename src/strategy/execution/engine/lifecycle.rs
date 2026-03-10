use super::*;

pub(super) async fn persist_halt_if_needed(engine: &StrategyEngine) {
    if engine.risk_manager.can_trade().await {
        return;
    }

    let today = Utc::now().date_naive();
    let reason = engine
        .risk_manager
        .halt_reason()
        .await
        .unwrap_or_else(|| "Risk circuit breaker triggered".to_string());

    if let Err(e) = engine.store.halt_trading(today, &reason).await {
        error!("Failed to persist trading halt to DB: {}", e);
    }
}

pub(super) async fn persist_strategy_state_best_effort(
    engine: &StrategyEngine,
    state: StrategyState,
    round_id: Option<i32>,
    cycle_id: Option<i32>,
) {
    if let Err(e) = engine
        .store
        .update_strategy_state(state, round_id, cycle_id)
        .await
    {
        error!("Failed to persist strategy_state to DB: {}", e);
    }
}

pub(super) async fn abort_cycle_and_halt_safely(
    engine: &StrategyEngine,
    reason: &str,
) -> Result<()> {
    let _exec_guard = engine.execution_mutex.lock().await;

    let (strategy_state, round, ctx) = {
        let state = engine.state.read().await;
        (
            state.strategy_state,
            state.current_round.clone(),
            state.current_cycle.clone(),
        )
    };

    let (cycle_id, full_reason) = match (ctx.as_ref(), round.as_ref()) {
        (Some(ctx), Some(round)) => {
            let mut full_reason = reason.to_string();

            if strategy_state == StrategyState::Leg1Filled && ctx.leg2_order_id.is_none() {
                match engine
                    .unwind_leg1_exposure(ctx, round, ctx.leg1_shares)
                    .await
                {
                    Ok(summary) => full_reason = format!("{}; {}", reason, summary),
                    Err(e) => full_reason = format!("{}; unwind failed: {}", reason, e),
                }
            }

            (Some(ctx.cycle_id), full_reason)
        }
        _ => (ctx.as_ref().map(|c| c.cycle_id), reason.to_string()),
    };

    if let Some(cycle_id) = cycle_id {
        if let Err(err) = engine.store.abort_cycle(cycle_id, &full_reason).await {
            error!("Failed to abort cycle {} in DB: {}", cycle_id, err);
        }

        engine.risk_manager.record_failure(reason).await;

        let today = Utc::now().date_naive();
        if let Err(e) = engine.store.record_cycle_abort(today).await {
            error!("Failed to record cycle abort: {}", e);
        }
        if let Err(e) = engine.store.halt_trading(today, reason).await {
            error!("Failed to persist halt_trading: {}", e);
        }
    } else {
        let today = Utc::now().date_naive();
        if let Err(e) = engine.store.halt_trading(today, reason).await {
            error!("Failed to persist halt_trading: {}", e);
        }
    }

    engine.risk_manager.trigger_circuit_breaker(reason).await;
    persist_halt_if_needed(engine).await;

    {
        let mut state = engine.state.write().await;
        state.strategy_state = StrategyState::Abort;
        state.current_cycle = None;
        state.version += 1;
    }

    persist_strategy_state_best_effort(
        engine,
        StrategyState::Abort,
        round.as_ref().and_then(|r| r.id),
        None,
    )
    .await;

    Ok(())
}

pub(super) async fn force_leg2_or_abort(engine: &StrategyEngine) -> Result<()> {
    let state = engine.state.read().await;

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

    let opposite_side = ctx.leg1_side.opposite();
    let token_id = round.token_id(opposite_side);

    if let Ok((_, Some(ask))) = engine.executor.get_prices(token_id).await {
        let forced_price = ask * (Decimal::ONE + engine.config.strategy.slippage_buffer);
        if let Err(e) = engine.enter_leg2_forced(opposite_side, forced_price).await {
            error!("Forced Leg2 failed: {}", e);
            abort_cycle_and_halt_safely(engine, "Forced Leg2 failed").await?;
        }
    } else {
        abort_cycle_and_halt_safely(engine, "No quote for forced Leg2").await?;
    }

    Ok(())
}

pub(super) async fn abort_cycle(engine: &StrategyEngine, reason: &str) -> Result<()> {
    let (cycle_id, round_id) = {
        let mut state = engine.state.write().await;
        let cycle_id = state.current_cycle.as_ref().map(|c| c.cycle_id);
        let round_id = state.current_round.as_ref().and_then(|r| r.id);
        state.strategy_state = StrategyState::Abort;
        state.current_cycle = None;
        state.version += 1;
        (cycle_id, round_id)
    };

    if let Some(cycle_id) = cycle_id {
        warn!("Aborting cycle {}: {}", cycle_id, reason);
        if let Err(e) = engine.store.abort_cycle(cycle_id, reason).await {
            error!("Failed to abort cycle {} in DB: {}", cycle_id, e);
            let halt_reason = "Database error during abort_cycle";
            engine.risk_manager.trigger_circuit_breaker(halt_reason).await;
            persist_halt_if_needed(engine).await;
        }
        engine.risk_manager.record_failure(reason).await;
        persist_halt_if_needed(engine).await;

        let today = Utc::now().date_naive();
        if let Err(e) = engine.store.record_cycle_abort(today).await {
            error!("Failed to record cycle abort metric: {}", e);
            let halt_reason = "Database error during record_cycle_abort";
            engine.risk_manager.trigger_circuit_breaker(halt_reason).await;
            persist_halt_if_needed(engine).await;
        }
    }

    persist_strategy_state_best_effort(engine, StrategyState::Abort, round_id, None).await;

    Ok(())
}

pub(super) async fn abort_cycle_neutral(engine: &StrategyEngine, reason: &str) -> Result<()> {
    let (cycle_id, round_id) = {
        let mut state = engine.state.write().await;
        let cycle_id = state.current_cycle.as_ref().map(|c| c.cycle_id);
        let round_id = state.current_round.as_ref().and_then(|r| r.id);
        state.strategy_state = StrategyState::Abort;
        state.current_cycle = None;
        state.version += 1;
        (cycle_id, round_id)
    };

    if let Some(cycle_id) = cycle_id {
        warn!("Aborting cycle {} (neutral): {}", cycle_id, reason);

        if let Err(e) = engine.store.abort_cycle(cycle_id, reason).await {
            error!("Failed to abort cycle {} in DB: {}", cycle_id, e);
            let halt_reason = "Database error during abort_cycle_neutral";
            engine.risk_manager.trigger_circuit_breaker(halt_reason).await;
            persist_halt_if_needed(engine).await;
        }

        let today = Utc::now().date_naive();
        if let Err(e) = engine.store.record_cycle_abort_neutral(today).await {
            error!("Failed to record neutral cycle abort metric: {}", e);
            let halt_reason = "Database error during record_cycle_abort_neutral";
            engine.risk_manager.trigger_circuit_breaker(halt_reason).await;
            persist_halt_if_needed(engine).await;
        }
    }

    persist_strategy_state_best_effort(engine, StrategyState::Abort, round_id, None).await;
    persist_halt_if_needed(engine).await;

    Ok(())
}

pub(super) async fn transition_to_idle(engine: &StrategyEngine) -> Result<()> {
    {
        let mut state = engine.state.write().await;
        state.strategy_state = StrategyState::Idle;
        state.current_cycle = None;
        state.current_round = None;
        state.version += 1;
    }

    persist_strategy_state_best_effort(engine, StrategyState::Idle, None, None).await;

    {
        let mut detector = engine.signal_detector.write().await;
        detector.reset(None);
    }

    debug!("Transitioned to IDLE state");
    Ok(())
}
