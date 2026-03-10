use super::StrategyEngine;
use crate::adapters::QuoteUpdate;
use crate::domain::{Round, StrategyState};
use crate::error::Result;
use tracing::{debug, info, warn};

pub(super) async fn on_quote_update(engine: &StrategyEngine, update: QuoteUpdate) -> Result<()> {
    let (round, strategy_state, current_cycle) = {
        let state = engine.state.read().await;
        let Some(round) = state.current_round.clone() else {
            return Ok(());
        };
        (round, state.strategy_state, state.current_cycle.clone())
    };

    if round.has_ended() {
        if strategy_state.requires_abort_on_round_end() {
            engine.abort_cycle_and_halt_safely("Round ended").await?;
        } else {
            engine.transition_to_idle().await?;
        }
        return Ok(());
    }

    if strategy_state == StrategyState::WatchWindow {
        let minutes_elapsed = round.minutes_elapsed();
        if minutes_elapsed >= engine.config.strategy.window_min as i64 {
            info!("Watch window expired after {} minutes", minutes_elapsed);
            engine.transition_to_idle().await?;
            return Ok(());
        }
    }

    if update.token_id != round.up_token_id && update.token_id != round.down_token_id {
        return Ok(());
    }

    match strategy_state {
        StrategyState::Idle => {}
        StrategyState::WatchWindow => {
            let round_slug = Some(round.slug.as_str());
            let signal = {
                let mut detector = engine.signal_detector.write().await;
                detector.update(&update.quote, round_slug)
            };

            if let Some(signal) = signal {
                if signal.is_valid(engine.config.execution.max_spread_bps) {
                    if let Err(e) = engine.enter_leg1(signal.side, signal.trigger_price).await {
                        warn!("Failed to enter Leg1: {}", e);
                    }
                } else {
                    debug!(
                        "Signal rejected: spread {} > max {}",
                        signal.spread_bps, engine.config.execution.max_spread_bps
                    );
                }
            }
        }
        StrategyState::Leg1Pending => {}
        StrategyState::Leg1Filled => {
            let should_enter_leg2 = match current_cycle.as_ref() {
                Some(ctx) => {
                    let opposite_side = ctx.leg1_side.opposite();
                    if update.side != opposite_side {
                        None
                    } else if let Some(ask) = update.quote.best_ask {
                        let detector = engine.signal_detector.read().await;
                        detector
                            .check_leg2_condition(ctx.leg1_price, ask)
                            .then_some((opposite_side, ask))
                    } else {
                        None
                    }
                }
                None => None,
            };

            let should_force = engine.risk_manager.must_force_leg2(&round);

            if let Some((opposite_side, ask)) = should_enter_leg2 {
                if let Err(e) = engine.enter_leg2(opposite_side, ask).await {
                    warn!("Failed to enter Leg2: {}", e);
                }
            } else if should_force {
                engine.force_leg2_or_abort().await?;
            }
        }
        StrategyState::Leg2Pending => {}
        StrategyState::CycleComplete | StrategyState::Abort => {
            engine.transition_to_idle().await?;
        }
    }

    Ok(())
}

pub(super) async fn check_round_transition(engine: &StrategyEngine) -> Result<()> {
    let state = engine.state.read().await;

    if let Some(round) = &state.current_round {
        if round.has_ended() {
            info!("Round {} has ended", round.slug);

            if state.strategy_state.requires_abort_on_round_end() {
                drop(state);
                engine.abort_cycle_and_halt_safely("Round ended").await?;
            } else {
                drop(state);
                engine.transition_to_idle().await?;
            }
        } else if matches!(
            state.strategy_state,
            StrategyState::CycleComplete | StrategyState::Abort
        ) {
            drop(state);
            engine.transition_to_idle().await?;
        } else if state.strategy_state == StrategyState::Leg1Filled
            && engine.risk_manager.must_force_leg2(round)
        {
            drop(state);
            engine.force_leg2_or_abort().await?;
        } else if state.strategy_state == StrategyState::WatchWindow {
            let minutes_elapsed = round.minutes_elapsed();
            if minutes_elapsed >= engine.config.strategy.window_min as i64 {
                info!("Watch window expired after {} minutes", minutes_elapsed);
                drop(state);
                engine.transition_to_idle().await?;
            }
        }
    }

    Ok(())
}

pub(super) async fn set_round(engine: &StrategyEngine, round: Round) -> Result<()> {
    {
        let state = engine.state.read().await;
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

    let round_id = engine.store.upsert_round(&round).await?;
    let mut round_with_id = round.clone();
    round_with_id.id = Some(round_id);

    {
        let mut state = engine.state.write().await;
        state.current_round = Some(round_with_id);

        if state.strategy_state == StrategyState::Idle {
            if !round.has_ended() && round.minutes_elapsed() < engine.config.strategy.window_min as i64
            {
                state.strategy_state = StrategyState::WatchWindow;
                info!("Entering watch window for round: {}", round.slug);
            } else {
                debug!(
                    "Round {} already outside watch window (elapsed={}m, window={}m, ended={})",
                    round.slug,
                    round.minutes_elapsed(),
                    engine.config.strategy.window_min,
                    round.has_ended(),
                );
            }
        }

        state.version += 1;
    }

    {
        let mut detector = engine.signal_detector.write().await;
        detector.reset(Some(&round.slug));
    }

    let (strategy_state, cycle_id) = {
        let state = engine.state.read().await;
        (
            state.strategy_state,
            state.current_cycle.as_ref().map(|c| c.cycle_id),
        )
    };
    engine
        .persist_strategy_state_best_effort(strategy_state, Some(round_id), cycle_id)
        .await;

    Ok(())
}
