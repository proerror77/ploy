//! Live Leg1 entry gating and opening-window evaluation for staggered arb.

use super::*;
use crate::strategy::gamma_scalping::greeks::BinaryGreeks;

struct PreparedEntryContext {
    elapsed_since_start: i64,
    ua: Decimal,
    da: Decimal,
    current_sum: Decimal,
    sigma: f64,
    p_hat: f64,
    greeks: Option<BinaryGreeks>,
    predicted_up: bool,
    obi: f64,
    strong_obi_bonus_active: bool,
    quote_state: PmEventQuoteState,
}

struct EntryOrderPlan {
    leg1_dir: Direction,
    leg1_ask: Decimal,
    token_id: String,
    side: Side,
    shares: u64,
    leg1_fee: Decimal,
    total_cost: Decimal,
}

pub(super) fn has_opening_window_candidate(
    adapter: &StaggeredArbAdapter,
    symbol: &str,
    ts: DateTime<Utc>,
) -> bool {
    let bc = &adapter.config.backtest_config;
    adapter
        .active_windows
        .get(symbol)
        .map(|windows| {
            windows.iter().any(|window| {
                let time_remaining = (window.end_time - ts).num_seconds();
                if time_remaining <= 0 || time_remaining < bc.min_time_remaining_secs as i64 {
                    return false;
                }

                let window_start =
                    window.end_time - chrono::Duration::seconds(window.window_secs as i64);
                let elapsed_since_start = (ts - window_start).num_seconds();
                if elapsed_since_start < 0 {
                    return false;
                }

                if bc.entry_after_start_min_secs > 0
                    && elapsed_since_start < bc.entry_after_start_min_secs as i64
                {
                    return false;
                }

                let allowed_max = bc.entry_after_start_max_secs_now(window.window_secs, true);
                allowed_max == 0 || elapsed_since_start <= allowed_max as i64
            })
        })
        .unwrap_or(false)
}

pub(super) fn try_entry(
    adapter: &mut StaggeredArbAdapter,
    symbol: &str,
    ts: DateTime<Utc>,
) -> Vec<StrategyAction> {
    let mut actions = Vec::new();
    let bc = &adapter.config.backtest_config;

    let windows: Vec<LiveWindow> = match adapter.active_windows.get(symbol) {
        Some(w) if !w.is_empty() => w.clone(),
        _ => return actions,
    };

    let (st, vol_info) = match adapter.spot_prices.get(symbol) {
        Some(s) => {
            let vol = s.volatility(bc.vol_lookback_secs).and_then(|v| v.to_f64());
            let n_ticks = s.history_len().min(5000) as f64;
            (s.price, (vol, n_ticks))
        }
        None => return actions,
    };

    for window in &windows {
        let (up_ask, down_ask) = adapter
            .pm_asks_by_event
            .get(&window.event_id)
            .copied()
            .unwrap_or((None, None));
        if let Some(action) =
            try_entry_for_window(adapter, symbol, ts, window, st, vol_info, up_ask, down_ask)
        {
            actions.push(action);
        }
    }
    actions
}

pub(super) fn try_entry_for_window(
    adapter: &mut StaggeredArbAdapter,
    symbol: &str,
    ts: DateTime<Utc>,
    window: &LiveWindow,
    st: Decimal,
    vol_info: (Option<f64>, f64),
    up_ask: Option<Decimal>,
    down_ask: Option<Decimal>,
) -> Option<StrategyAction> {
    let bc = adapter.config.backtest_config.clone();
    let context = prepare_entry_context(
        adapter, symbol, ts, window, st, vol_info, up_ask, down_ask, &bc,
    )?;
    let plan = build_entry_order_plan(adapter, symbol, ts, window, &bc, &context)?;
    submit_entry_order(adapter, symbol, ts, window, &bc, context, plan)
}

fn prepare_entry_context(
    adapter: &mut StaggeredArbAdapter,
    symbol: &str,
    ts: DateTime<Utc>,
    window: &LiveWindow,
    st: Decimal,
    vol_info: (Option<f64>, f64),
    up_ask: Option<Decimal>,
    down_ask: Option<Decimal>,
    bc: &StaggeredArbBacktestConfig,
) -> Option<PreparedEntryContext> {
    if !adapter.dry_run {
        if let Some(pause_until) = adapter.balance_pause_until {
            if ts < pause_until {
                adapter.bump_entry_reject_for_symbol(symbol, "balance_pause_active");
                return None;
            }
            adapter.balance_pause_until = None;
            adapter.consecutive_balance_failures = 0;
            info!("[STAG-ARB] Balance pause expired, resuming entries");
        }
    }

    if adapter.has_active_cycle_for_event(&window.event_id) {
        adapter.bump_entry_reject_for_symbol(symbol, "event_cycle_active");
        return None;
    }

    if bc.max_concurrent_positions > 0
        && adapter.active_cycle_count() >= bc.max_concurrent_positions
    {
        adapter.bump_entry_reject_for_symbol(symbol, "max_concurrent_reached");
        return None;
    }

    let time_remaining = (window.end_time - ts).num_seconds() as f64;
    if time_remaining <= 0.0 || time_remaining < bc.min_time_remaining_secs as f64 {
        adapter.bump_entry_reject_for_symbol(symbol, "time_remaining_too_low");
        return None;
    }

    let window_start = window.end_time - chrono::Duration::seconds(window.window_secs as i64);
    let elapsed_since_start = (ts - window_start).num_seconds();
    if elapsed_since_start < 0 {
        adapter.bump_entry_reject_for_symbol(symbol, "before_event_start");
        return None;
    }
    if bc.entry_after_start_min_secs > 0
        && elapsed_since_start < bc.entry_after_start_min_secs as i64
    {
        adapter.bump_entry_reject_for_symbol(symbol, "entry_observation_delay_active");
        return None;
    }

    let (ua, da) = match (up_ask, down_ask) {
        (Some(u), Some(d)) => (u, d),
        _ => {
            adapter.bump_entry_reject_for_symbol(symbol, "missing_pm_quotes");
            return None;
        }
    };
    let quote_state = adapter.event_quote_state(&window.event_id, up_ask, down_ask, ts);
    if !bc.pm_quote_is_fresh(quote_state.up.last_seen_at, ts)
        || !bc.pm_quote_is_fresh(quote_state.down.last_seen_at, ts)
    {
        adapter.bump_entry_reject_for_symbol(symbol, "pm_quotes_stale");
        return None;
    }
    if ua < bc.min_ask_price || da < bc.min_ask_price {
        adapter.bump_entry_reject_for_symbol(symbol, "ask_below_min");
        return None;
    }

    let current_sum = ua + da;
    if current_sum < bc.min_entry_sum {
        adapter.bump_entry_reject_for_symbol(symbol, "sum_below_min_entry_sum");
        return None;
    }
    if bc.max_initial_sum > Decimal::ZERO && current_sum >= bc.max_initial_sum {
        adapter.bump_entry_reject_for_symbol(symbol, "sum_above_max_initial_sum");
        return None;
    }

    let sigma = {
        let floor = bc.vol_floor;
        match vol_info.0 {
            Some(tick_vol) if tick_vol > 0.0 => {
                let n_ticks = vol_info.1;
                (tick_vol * n_ticks.sqrt()).max(floor)
            }
            _ => floor,
        }
    };
    if sigma < bc.min_entry_sigma {
        adapter.bump_entry_reject_for_symbol(symbol, "sigma_below_min_entry_sigma");
        return None;
    }
    if bc.max_entry_sigma > 0.0 && sigma > bc.max_entry_sigma {
        adapter.bump_entry_reject_for_symbol(symbol, "sigma_above_max_entry_sigma");
        return None;
    }

    let s0 = match window.open_price {
        Some(v) if v > Decimal::ZERO => v,
        _ => {
            adapter.bump_entry_reject_for_symbol(symbol, "missing_window_open_anchor");
            return None;
        }
    };
    let p_hat = estimate_probability(s0, st, sigma, time_remaining, bc.mu);
    let greeks = if bc.use_greeks {
        super::super::gamma_scalping::greeks::binary_greeks(
            st.to_f64().unwrap_or(0.0),
            s0.to_f64().unwrap_or(0.0),
            sigma,
            time_remaining,
            window.window_secs as f64,
        )
    } else {
        None
    };

    if let Some(ref g) = greeks {
        if bc.min_gamma > 0.0 && g.gamma.abs() < bc.min_gamma {
            adapter.bump_entry_reject_for_symbol(symbol, "greeks_gamma_below_min");
            return None;
        }
        if bc.max_theta_cost > 0.0 && g.theta.abs() > bc.max_theta_cost {
            adapter.bump_entry_reject_for_symbol(symbol, "greeks_theta_above_max");
            return None;
        }
        if bc.max_fair_value_distance < 0.5
            && (g.fair_value - 0.5).abs() > bc.max_fair_value_distance
        {
            adapter
                .bump_entry_reject_for_symbol(symbol, "greeks_fair_value_outside_long_gamma_band");
            return None;
        }
    }

    const MIN_PRICE_DISPLACEMENT: f64 = 0.0003;
    let displacement = ((st - s0) / s0).to_f64().unwrap_or(0.0);
    if displacement.abs() < MIN_PRICE_DISPLACEMENT {
        adapter.bump_entry_reject_for_symbol(symbol, "price_displacement_too_small");
        return None;
    }

    let predicted_up = if bc.reverse_signal {
        p_hat < 0.5
    } else {
        p_hat > 0.5
    };
    if predicted_up && displacement <= 0.0 {
        adapter.bump_entry_reject_for_symbol(symbol, "direction_displacement_mismatch");
        return None;
    }
    if !predicted_up && displacement >= 0.0 {
        adapter.bump_entry_reject_for_symbol(symbol, "direction_displacement_mismatch");
        return None;
    }

    if let Some(ref g) = greeks {
        const MIN_DELTA_ABS: f64 = 0.02;
        const MIN_VEGA_ABS: f64 = 0.0001;
        const MIN_D2_STRENGTH: f64 = 0.05;
        if g.delta.abs() < MIN_DELTA_ABS || g.vega.abs() < MIN_VEGA_ABS {
            adapter.bump_entry_reject_for_symbol(symbol, "greeks_strength_too_low");
            return None;
        }
        if predicted_up {
            if g.d2 < MIN_D2_STRENGTH || g.fair_value <= 0.5 {
                adapter.bump_entry_reject_for_symbol(symbol, "greeks_direction_mismatch");
                return None;
            }
        } else if g.d2 > -MIN_D2_STRENGTH || g.fair_value >= 0.5 {
            adapter.bump_entry_reject_for_symbol(symbol, "greeks_direction_mismatch");
            return None;
        }
    }

    const OI_CONFIRM_THRESHOLD: f64 = 0.005;
    const OI_MAX_STALE_SECS: i64 = 60;
    let obi_ts = match adapter.binance_l2_obi_ts.get(symbol) {
        Some(v) => *v,
        None => {
            adapter.bump_entry_reject_for_symbol(symbol, "obi_missing");
            return None;
        }
    };
    if (ts - obi_ts).num_seconds().abs() > OI_MAX_STALE_SECS {
        adapter.bump_entry_reject_for_symbol(symbol, "obi_stale");
        return None;
    }

    let obi = match adapter.binance_l2_obi_5.get(symbol) {
        Some(v) => v.to_f64().unwrap_or(0.0),
        None => {
            adapter.bump_entry_reject_for_symbol(symbol, "obi_missing");
            return None;
        }
    };
    let prev_obi = adapter
        .binance_l2_obi_prev_5
        .get(symbol)
        .map(|value| value.to_f64().unwrap_or(0.0));
    let fair_value_distance = greeks.as_ref().map(|g| (g.fair_value - 0.5).abs());
    let premium_sum_excess = adapter.premium_sum_excess(current_sum);
    let required_obi_strength =
        bc.obi_confirm_threshold + premium_sum_excess * bc.premium_sum_obi_slope;
    if !bc.obi_confirms_direction(predicted_up, obi, required_obi_strength) {
        let reason = if required_obi_strength > OI_CONFIRM_THRESHOLD {
            "obi_not_confirmed_for_premium_entry"
        } else {
            "obi_not_confirmed"
        };
        adapter.bump_entry_reject_for_symbol(symbol, reason);
        return None;
    }

    let obi_persistent = bc.obi_is_persistent(predicted_up, obi, prev_obi, required_obi_strength);
    let strong_obi_bonus_active = bc.strong_obi_entry_bonus_active(
        predicted_up,
        obi,
        prev_obi,
        current_sum,
        fair_value_distance,
    );
    if !obi_persistent && !strong_obi_bonus_active {
        adapter.bump_entry_reject_for_symbol(symbol, "obi_not_persistent");
        return None;
    }

    let direction_strength = (p_hat - 0.5).abs();
    let required_direction_strength =
        bc.direction_threshold_now(current_sum, strong_obi_bonus_active);
    if direction_strength < required_direction_strength {
        let reason = if strong_obi_bonus_active {
            "direction_strength_below_strong_obi_adjusted_threshold"
        } else if required_direction_strength > bc.direction_threshold {
            "direction_strength_below_sum_adjusted_threshold"
        } else {
            "direction_strength_below_threshold"
        };
        adapter.bump_entry_reject_for_symbol(symbol, reason);
        return None;
    }

    Some(PreparedEntryContext {
        elapsed_since_start,
        ua,
        da,
        current_sum,
        sigma,
        p_hat,
        greeks,
        predicted_up,
        obi,
        strong_obi_bonus_active,
        quote_state,
    })
}

fn build_entry_order_plan(
    adapter: &mut StaggeredArbAdapter,
    symbol: &str,
    ts: DateTime<Utc>,
    window: &LiveWindow,
    bc: &StaggeredArbBacktestConfig,
    context: &PreparedEntryContext,
) -> Option<EntryOrderPlan> {
    let allowed_entry_window_secs =
        bc.entry_after_start_max_secs_now(window.window_secs, context.strong_obi_bonus_active);
    if allowed_entry_window_secs > 0
        && context.elapsed_since_start > allowed_entry_window_secs as i64
    {
        adapter.bump_entry_reject_for_symbol(symbol, "entry_window_expired");
        return None;
    }

    let (leg1_dir, leg1_ask, other_quote_first_seen_at) = if context.predicted_up {
        (
            Direction::Up,
            context.ua,
            context.quote_state.down.first_seen_at,
        )
    } else {
        (
            Direction::Down,
            context.da,
            context.quote_state.up.first_seen_at,
        )
    };
    if !bc.entry_quote_is_persistent(other_quote_first_seen_at, ts) {
        adapter.bump_entry_reject_for_symbol(symbol, "other_ask_not_persistent");
        return None;
    }

    if leg1_ask > bc.max_leg1_price_now(context.strong_obi_bonus_active) {
        adapter.bump_entry_reject_for_symbol(symbol, "leg1_price_above_cap");
        return None;
    }

    let target_leg2 = bc.merge_target_sum - leg1_ask;
    if target_leg2 <= Decimal::ZERO {
        adapter.bump_entry_reject_for_symbol(symbol, "target_leg2_non_positive");
        return None;
    }

    if let Some(last) = adapter.cooldowns.get(symbol) {
        if (ts - *last).num_seconds() < bc.cooldown_secs as i64 {
            adapter.bump_entry_reject_for_symbol(symbol, "cooldown_active");
            return None;
        }
    }

    if bc.max_trades_per_event > 0 {
        let count = adapter
            .event_trade_counts
            .get(&window.event_id)
            .copied()
            .unwrap_or(0);
        if count >= bc.max_trades_per_event {
            adapter.bump_entry_reject_for_symbol(symbol, "max_trades_per_event_reached");
            return None;
        }
    }

    let mut fixed_amount_target: Option<Decimal> = None;
    let mut min_share_bump = false;
    let shares = if let Some(amount_usd) = adapter.fixed_amount_usd {
        fixed_amount_target = Decimal::try_from(amount_usd)
            .ok()
            .map(|d| d.max(Decimal::ZERO));
        let price_f64 = leg1_ask.to_f64().unwrap_or(0.5);
        if price_f64 > 0.0 {
            let calc_from_target = (amount_usd / price_f64).ceil() as u64;
            let min_shares_for_notional = (1.0_f64 / price_f64).ceil() as u64;
            let adjusted = calc_from_target.max(min_shares_for_notional).max(5);
            min_share_bump = adjusted > calc_from_target;
            adjusted
        } else {
            bc.shares_per_trade.max(5)
        }
    } else {
        let base_shares = bc.shares_per_trade.max(5);
        if bc.delta_weighted_sizing {
            if let Some(ref g) = context.greeks {
                let scale = (g.delta.abs() * 2.0).clamp(0.5, 2.0);
                ((base_shares as f64 * scale).round() as u64).max(5)
            } else {
                base_shares
            }
        } else {
            base_shares
        }
    };
    if shares == 0 {
        adapter.bump_entry_reject_for_symbol(symbol, "zero_share_sizing");
        return None;
    }

    let leg1_notional = leg1_ask * Decimal::from(shares);
    if let Some(target) = fixed_amount_target.filter(|t| *t > Decimal::ZERO) {
        if min_share_bump {
            info!(
                "[STAG-ARB] FIXED AMOUNT ADJUST {} target=${:.4} actual_leg_notional=${:.4} shares={} price={:.4}",
                symbol, target, leg1_notional, shares, leg1_ask
            );
            if leg1_notional > target * dec!(1.20) && !adapter.fixed_amount_overage_warned {
                let over_pct = ((leg1_notional - target) / target) * dec!(100);
                warn!(
                    "[STAG-ARB] fixed_amount_usd=${:.4} inflated to actual_leg_notional=${:.4} (+{:.1}%) because venue minimums apply ($1 notional / 5 shares)",
                    target, leg1_notional, over_pct
                );
                adapter.fixed_amount_overage_warned = true;
            }
        }
    }

    let leg1_fee = leg1_notional * adapter.config.fee_rate;
    let total_cost = leg1_notional + leg1_fee;
    let available_before = adapter.available_balance_for_leg1();
    let remaining_after = available_before - total_cost;
    if total_cost > available_before || remaining_after < adapter.min_balance_usd {
        adapter.bump_entry_reject_for_symbol(symbol, "reserve_guard");
        info!(
            "[STAG-ARB] SKIP ENTRY {} reserve_guard available=${:.4} cost=${:.4} min_balance=${:.4}",
            symbol, available_before, total_cost, adapter.min_balance_usd
        );
        return None;
    }

    let token_id = match leg1_dir {
        Direction::Up => window.up_token.clone(),
        Direction::Down => window.down_token.clone(),
    };
    let side = match leg1_dir {
        Direction::Up => Side::Up,
        Direction::Down => Side::Down,
    };

    Some(EntryOrderPlan {
        leg1_dir,
        leg1_ask,
        token_id,
        side,
        shares,
        leg1_fee,
        total_cost,
    })
}

fn submit_entry_order(
    adapter: &mut StaggeredArbAdapter,
    symbol: &str,
    ts: DateTime<Utc>,
    window: &LiveWindow,
    bc: &StaggeredArbBacktestConfig,
    context: PreparedEntryContext,
    plan: EntryOrderPlan,
) -> Option<StrategyAction> {
    if adapter.dry_run {
        adapter.equity -= plan.total_cost;

        let window_duration = (window.end_time - ts).num_seconds() as f64;
        let max_wait_by_pct = (window_duration * bc.max_wait_pct) as i64;
        let max_wait = (bc.max_wait_secs as i64).min(max_wait_by_pct);
        let wait_deadline = ts + chrono::Duration::seconds(max_wait);

        adapter.positions.push(PaperPosition {
            symbol: symbol.to_string(),
            event_id: window.event_id.clone(),
            condition_id: window.condition_id.clone(),
            up_token: window.up_token.clone(),
            down_token: window.down_token.clone(),
            leg1_direction: plan.leg1_dir.clone(),
            leg1_price: plan.leg1_ask,
            leg1_shares: plan.shares,
            leg1_fee: plan.leg1_fee,
            leg1_time: ts,
            entry_obi: Some(context.obi),
            protective_stop_armed_at: None,
            wait_deadline,
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        adapter.cooldowns.insert(symbol.to_string(), ts);
        *adapter
            .event_trade_counts
            .entry(window.event_id.clone())
            .or_default() += 1;

        let msg = format!(
            "[STAG-ARB] ENTRY {} {} leg1=${:.4} sum=${:.4} p_hat={:.3} σ={:.5} (paper)",
            symbol, plan.leg1_dir, plan.leg1_ask, context.current_sum, context.p_hat, context.sigma,
        );
        info!("{}", msg);
        adapter.bump_entry_reject_for_symbol(symbol, "entry_accepted");

        return Some(StrategyAction::LogEvent {
            event: StrategyEvent::new(StrategyEventType::EntryTriggered, msg),
        });
    }

    let client_order_id = format!(
        "stag_leg1_{}_{}",
        window.event_id,
        Utc::now().timestamp_millis()
    );
    adapter.live_orders.insert(
        client_order_id.clone(),
        LiveOrderTrack {
            event_id: window.event_id.clone(),
            condition_id: window.condition_id.clone(),
            symbol: symbol.to_string(),
            up_token: window.up_token.clone(),
            down_token: window.down_token.clone(),
            direction: plan.leg1_dir.clone(),
            token_id: plan.token_id.clone(),
            leg: 1,
            price: plan.leg1_ask,
            shares: plan.shares,
            position_idx: None,
            close_reason: None,
            submitted_at: ts,
            cancel_requested_at: None,
            exchange_order_id: None,
            acknowledged_filled_qty: 0,
            entry_obi: Some(context.obi),
        },
    );
    adapter.pending_leg1_events.insert(window.event_id.clone());
    adapter.cooldowns.insert(symbol.to_string(), ts);

    let msg = format!(
        "[STAG-ARB] LEG1 SUBMIT {} {} @ {:.2}¢ ({} shares, ${:.2}) p_hat={:.3} σ={:.5}",
        symbol,
        plan.leg1_dir,
        plan.leg1_ask * dec!(100),
        plan.shares,
        plan.leg1_ask.to_f64().unwrap_or(0.0) * plan.shares as f64,
        context.p_hat,
        context.sigma,
    );
    info!("{}", msg);
    adapter.bump_entry_reject_for_symbol(symbol, "entry_accepted");

    Some(crypto_submit_intent(
        client_order_id,
        window.event_id.clone(),
        plan.token_id,
        plan.side,
        plan.shares,
        plan.leg1_ask,
        10,
    ))
}
