use super::*;

#[derive(Debug)]
enum Leg2Action {
    Fill(Decimal, String),
    #[allow(dead_code)]
    Abort(String),
}

impl StaggeredArbBacktestEngine {
    pub(super) fn check_leg2_opportunities(&mut self, symbol: &str, ts: DateTime<Utc>) {
        let mut actions: Vec<(usize, Leg2Action)> = Vec::new();
        let mut protective_arm_updates: Vec<(usize, Option<DateTime<Utc>>)> = Vec::new();
        for i in 0..self.positions.len() {
            let (
                pos_symbol,
                pos_state,
                event_slug,
                leg1_direction,
                leg1_price,
                best_sum_seen,
                event_end_time,
                window_duration_secs,
                entry_sigma,
                s0,
                leg1_time,
                wait_deadline,
                entry_obi,
                protective_stop_armed_at,
            ) = {
                let pos = &self.positions[i];
                (
                    pos.symbol.clone(),
                    pos.state.clone(),
                    pos.event_slug.clone(),
                    pos.leg1_direction,
                    pos.leg1_price,
                    pos.best_sum_seen,
                    pos.event_end_time,
                    pos.window_duration_secs,
                    pos.entry_sigma,
                    pos.s0,
                    pos.leg1_time,
                    pos.wait_deadline,
                    pos.entry_obi,
                    pos.protective_stop_armed_at,
                )
            };

            if pos_symbol != symbol || pos_state != ArbPositionState::Leg1Filled {
                continue;
            }

            let pm_asks = match self.pm_asks_by_event.get(&event_slug).copied() {
                Some(a) => a,
                None => continue,
            };
            let quote_state = self.event_quote_state(&event_slug, pm_asks.0, pm_asks.1, ts);

            let (other_ask, other_state) = match leg1_direction {
                Direction::Up => (pm_asks.1, quote_state.down),
                Direction::Down => (pm_asks.0, quote_state.up),
            };
            if !self.config.pm_quote_is_fresh(other_state.last_seen_at, ts) {
                continue;
            }
            let other_ask = match other_ask {
                Some(a) if a >= self.config.min_ask_price => a,
                Some(_) => continue,
                None => continue,
            };

            if leg1_price + other_ask < self.config.min_entry_sum {
                continue;
            }

            let current_sum = leg1_price + other_ask;

            if current_sum < best_sum_seen {
                if let Some(pos) = self.positions.get_mut(i) {
                    pos.best_sum_seen = current_sum;
                }
            }

            let time_remaining = (event_end_time - ts).num_seconds() as f64;
            let in_final_window = self.config.no_trade_last_secs > 0
                && time_remaining <= self.config.no_trade_last_secs as f64
                && time_remaining > 0.0;
            let min_time = self.config.min_time_remaining_secs as f64;
            let force_threshold = self.config.force_close_threshold_now(
                time_remaining,
                window_duration_secs.max(0) as u64,
                in_final_window,
            );
            let protective_threshold = self.config.protective_close_threshold_now(
                time_remaining,
                window_duration_secs.max(0) as u64,
                in_final_window,
            );

            let current_greeks = if self.config.use_greeks {
                let spot = self
                    .spot_prices
                    .get(&pos_symbol)
                    .map(|sp| sp.price.to_f64().unwrap_or(0.0))
                    .unwrap_or(0.0);
                let strike = s0.to_f64().unwrap_or(0.0);
                if spot > 0.0 && strike > 0.0 && time_remaining > 0.0 {
                    binary_greeks(
                        spot,
                        strike,
                        entry_sigma,
                        time_remaining,
                        window_duration_secs as f64,
                    )
                } else {
                    None
                }
            } else {
                None
            };
            let current_obi = self
                .binance_l2_obi_5
                .get(&pos_symbol)
                .map(|value| value.to_f64().unwrap_or(0.0));
            let displacement_supportive = self
                .spot_prices
                .get(&pos_symbol)
                .and_then(|sp| {
                    if s0 <= Decimal::ZERO {
                        return None;
                    }
                    Some(((sp.price - s0) / s0).to_f64().unwrap_or(0.0))
                })
                .map(|displacement| match leg1_direction {
                    Direction::Up => displacement > 0.0,
                    Direction::Down => displacement < 0.0,
                })
                .unwrap_or(false);
            let greeks_supportive = current_greeks
                .as_ref()
                .map(|g| match leg1_direction {
                    Direction::Up => g.d2 > 0.05 && g.fair_value > 0.5,
                    Direction::Down => g.d2 < -0.05 && g.fair_value < 0.5,
                })
                .unwrap_or(!self.config.use_greeks);

            let secs_since_leg1 = (ts - leg1_time).num_seconds();
            let leg2_ready = secs_since_leg1 >= self.config.min_leg2_delay_secs as i64;

            if !in_final_window && current_sum < self.config.merge_target_sum && leg2_ready {
                actions.push((i, Leg2Action::Fill(other_ask, "merge".to_string())));
                continue;
            }

            if let Some(ref g) = current_greeks {
                if !in_final_window && leg2_ready && current_sum < Decimal::ONE {
                    let gamma_urgency = g.gamma.abs().min(1.0);
                    let adjusted_target = self.config.min_profit_target
                        * Decimal::from_f64(1.0 - gamma_urgency * 0.8).unwrap_or(Decimal::ONE);
                    if current_sum < self.config.merge_target_sum + adjusted_target {
                        trace!(
                            "Greeks merge: gamma={:.4} adjusted_target={:.4} sum={:.4}",
                            g.gamma,
                            adjusted_target,
                            current_sum
                        );
                        actions.push((i, Leg2Action::Fill(other_ask, "merge".to_string())));
                        continue;
                    }
                }

                if leg2_ready && self.config.max_theta_cost > 0.0 {
                    let theta_cost_remaining = g.theta.abs() * time_remaining;
                    if theta_cost_remaining > self.config.max_theta_cost {
                        trace!(
                            "Theta urgency: theta={:.6} cost_remaining={:.4} sum={:.4}",
                            g.theta,
                            theta_cost_remaining,
                            current_sum
                        );
                        if current_sum <= Decimal::ONE {
                            actions.push((i, Leg2Action::Fill(other_ask, "merge".to_string())));
                            continue;
                        }
                        if protective_threshold <= Decimal::ZERO
                            || current_sum <= protective_threshold
                        {
                            actions.push((
                                i,
                                Leg2Action::Fill(other_ask, "protective_theta".to_string()),
                            ));
                            continue;
                        }
                    }
                }
            }

            if !in_final_window && current_sum < Decimal::ONE && leg2_ready {
                actions.push((i, Leg2Action::Fill(other_ask, "merge".to_string())));
                continue;
            }

            if ts >= wait_deadline && leg2_ready {
                if force_threshold <= Decimal::ZERO || current_sum <= force_threshold {
                    actions.push((i, Leg2Action::Fill(other_ask, "forced_timeout".to_string())));
                }
                continue;
            }

            if self.config.max_leg1_loss > Decimal::ZERO {
                let (leg1_mark, leg1_mark_state) = match leg1_direction {
                    Direction::Up => (pm_asks.0, quote_state.up),
                    Direction::Down => (pm_asks.1, quote_state.down),
                };
                let leg1_current_value = if self
                    .config
                    .pm_quote_is_fresh(leg1_mark_state.last_seen_at, ts)
                {
                    leg1_mark.unwrap_or(leg1_price)
                } else {
                    leg1_price
                };
                let leg1_loss = leg1_price - leg1_current_value;
                if leg1_loss >= self.config.max_leg1_loss && leg2_ready {
                    let obi_supportive = self.config.obi_signal_still_supportive(
                        leg1_direction,
                        entry_obi,
                        current_obi,
                    );
                    if obi_supportive && displacement_supportive && greeks_supportive {
                        protective_arm_updates.push((i, None));
                        trace!(
                            "Skipping protective stop: signal still supportive obi={:?} displacement_supportive={} greeks_supportive={}",
                            current_obi,
                            displacement_supportive,
                            greeks_supportive
                        );
                        continue;
                    }
                    let hard_signal_broken = self
                        .config
                        .obi_signal_hard_flipped(leg1_direction, current_obi)
                        || (!displacement_supportive && !greeks_supportive);
                    let armed_at = protective_stop_armed_at.unwrap_or(ts);
                    let recovery_elapsed = (ts - armed_at).num_seconds();
                    let recovery_expired = self.config.protective_recovery_window_secs == 0
                        || recovery_elapsed >= self.config.protective_recovery_window_secs as i64;
                    if !hard_signal_broken && !recovery_expired {
                        protective_arm_updates.push((i, Some(armed_at)));
                        trace!(
                            "Arming protective stop: loss={:.4} recovery_elapsed={}s window={}s",
                            leg1_loss,
                            recovery_elapsed,
                            self.config.protective_recovery_window_secs
                        );
                        continue;
                    }
                    protective_arm_updates.push((i, None));
                    if protective_threshold <= Decimal::ZERO || current_sum <= protective_threshold
                    {
                        actions.push((
                            i,
                            Leg2Action::Fill(other_ask, "protective_stop_loss".to_string()),
                        ));
                    }
                    continue;
                } else if protective_stop_armed_at.is_some() {
                    protective_arm_updates.push((i, None));
                }
            }

            if time_remaining < min_time && leg2_ready {
                if force_threshold <= Decimal::ZERO || current_sum <= force_threshold {
                    actions.push((
                        i,
                        Leg2Action::Fill(other_ask, "forced_time_safety".to_string()),
                    ));
                }
            }
        }

        for (idx, armed_at) in protective_arm_updates {
            if let Some(pos) = self.positions.get_mut(idx) {
                pos.protective_stop_armed_at = armed_at;
            }
        }

        actions.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, action) in actions {
            match action {
                Leg2Action::Fill(other_ask, reason) => {
                    self.fill_leg2(idx, other_ask, &reason, ts);
                }
                Leg2Action::Abort(reason) => {
                    self.abort_position(idx, &reason, ts);
                }
            }
        }
    }

    pub(super) fn fill_leg2(
        &mut self,
        idx: usize,
        other_ask: Decimal,
        reason: &str,
        ts: DateTime<Utc>,
    ) {
        if idx >= self.positions.len() || self.positions[idx].state != ArbPositionState::Leg1Filled
        {
            return;
        }

        let pos = &self.positions[idx];
        let leg2_dir = match pos.leg1_direction {
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
        };
        let remaining_shares = pos.leg1_shares.saturating_sub(pos.leg2_shares.unwrap_or(0));
        if remaining_shares == 0 {
            return;
        }
        if !polymarket_order_meets_minimum(other_ask, remaining_shares) {
            return;
        }

        let depth = self.market_depth(&pos.symbol);
        let sim_result = self
            .execution_sim
            .simulate_buy(other_ask, ts, remaining_shares, depth);
        if sim_result.filled_shares == 0 {
            return;
        }

        let leg2_cost = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        let leg2_fee = self.fee_model.fee_shares(
            Decimal::from(sim_result.filled_shares),
            sim_result.fill_price,
        ) * sim_result.fill_price;
        let total_leg2_cost = leg2_cost + leg2_fee;

        if total_leg2_cost > self.equity {
            trace!("Cannot fill Leg2: insufficient equity");
            return;
        }

        self.equity -= total_leg2_cost;
        let fill_time = sim_result.fill_time;

        let (
            symbol,
            event_slug,
            leg1_direction,
            leg1_price,
            leg1_shares,
            leg1_time,
            entry_p_hat,
            entry_sigma,
            initial_sum,
            best_sum_seen,
            s0,
            window_duration_secs,
            entry_greeks,
            total_leg2_shares,
            total_leg2_price,
            total_leg2_fee,
        ) = {
            let pos = &mut self.positions[idx];
            let prev_leg2_shares = pos.leg2_shares.unwrap_or(0);
            let prev_leg2_price = pos.leg2_price.unwrap_or(Decimal::ZERO);
            let prev_leg2_fee = pos.leg2_fee.unwrap_or(Decimal::ZERO);

            let prev_notional = prev_leg2_price * Decimal::from(prev_leg2_shares);
            let add_notional = sim_result.fill_price * Decimal::from(sim_result.filled_shares);
            let total_leg2_shares = prev_leg2_shares + sim_result.filled_shares;
            let total_notional = prev_notional + add_notional;
            let total_leg2_price = total_notional / Decimal::from(total_leg2_shares);
            let total_leg2_fee = prev_leg2_fee + leg2_fee;

            pos.leg2_direction = Some(leg2_dir);
            pos.leg2_price = Some(total_leg2_price);
            pos.leg2_shares = Some(total_leg2_shares);
            pos.leg2_time = Some(fill_time);
            pos.leg2_fee = Some(total_leg2_fee);

            (
                pos.symbol.clone(),
                pos.event_slug.clone(),
                pos.leg1_direction,
                pos.leg1_price,
                pos.leg1_shares,
                pos.leg1_time,
                pos.entry_p_hat,
                pos.entry_sigma,
                pos.initial_sum,
                pos.best_sum_seen,
                pos.s0,
                pos.window_duration_secs,
                pos.entry_greeks,
                total_leg2_shares,
                total_leg2_price,
                total_leg2_fee,
            )
        };

        if total_leg2_shares < leg1_shares {
            debug!(
                "LEG2 PARTIAL {} | {}/{} filled avg={:.4}",
                event_slug, total_leg2_shares, leg1_shares, total_leg2_price
            );
            return;
        }

        let payout = Decimal::from(leg1_shares);
        let total_cost = Decimal::from(leg1_shares) * leg1_price
            + self.positions[idx].leg1_fee
            + Decimal::from(total_leg2_shares) * total_leg2_price
            + total_leg2_fee;
        let pnl = payout - total_cost;

        self.equity += payout;

        let holding_secs = (fill_time - leg1_time).num_seconds();
        let final_sum = leg1_price + total_leg2_price;

        let pos = &mut self.positions[idx];
        pos.state = ArbPositionState::Settled;
        pos.exit_reason = Some(reason.to_string());
        pos.pnl = Some(pnl);

        self.closed_trades.push(StaggeredArbClosedTrade {
            symbol: symbol.clone(),
            leg1_direction: format!("{}", leg1_direction),
            leg1_price,
            leg1_time,
            leg2_price: Some(total_leg2_price),
            leg2_time: Some(fill_time),
            shares: leg1_shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            initial_sum,
            final_sum: Some(final_sum),
            entry_p_hat,
            entry_sigma,
            best_sum_seen,
            s0,
            window_duration_secs,
            entry_delta: entry_greeks.map(|g| g.delta),
            entry_gamma: entry_greeks.map(|g| g.gamma),
            entry_theta: entry_greeks.map(|g| g.theta),
            entry_fair_value: entry_greeks.map(|g| g.fair_value),
        });

        self.recorder.record_exit(&BacktestSignal {
            signal_type: SignalType::Exit,
            symbol: symbol.clone(),
            direction: format!("{}", leg1_direction),
            timestamp: fill_time,
            p_hat: Some(entry_p_hat),
            ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            sigma: Some(entry_sigma),
            market_price: Some(total_leg2_price),
            spot_price: None,
            s0: Some(s0),
            time_remaining_secs: None,
            filter_reason: None,
            exit_reason: Some(reason.to_string()),
            exit_price: Some(total_leg2_price),
        });

        self.recorder.record_trade(&PendingTrade {
            symbol,
            direction: format!("{}", leg1_direction),
            entry_time: leg1_time,
            exit_time: fill_time,
            entry_price: leg1_price,
            exit_price: total_leg2_price,
            shares: leg1_shares as i32,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: Some(entry_p_hat),
            entry_ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            entry_sigma: Some(entry_sigma),
            s0: Some(s0),
        });

        debug!(
            "MERGE {} | leg1={:.4} leg2={:.4} sum={:.4} pnl={:.4}",
            event_slug, leg1_price, total_leg2_price, final_sum, pnl
        );
    }

    pub(super) fn abort_position(&mut self, idx: usize, reason: &str, ts: DateTime<Utc>) {
        let pos = &self.positions[idx];
        let current_price = match pos.leg1_direction {
            Direction::Up => self.pm_asks_by_event.get(&pos.event_slug).and_then(|a| a.0),
            Direction::Down => self.pm_asks_by_event.get(&pos.event_slug).and_then(|a| a.1),
        }
        .unwrap_or(pos.leg1_price);

        let depth = self.market_depth(&pos.symbol);
        let sim_result =
            self.execution_sim
                .simulate_sell(current_price, ts, pos.leg1_shares, depth);

        let proceeds = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        let sell_fee = self.fee_model.fee_shares(
            Decimal::from(sim_result.filled_shares),
            sim_result.fill_price,
        ) * sim_result.fill_price;
        let net_proceeds = proceeds - sell_fee;

        self.equity += net_proceeds;

        let entry_cost = Decimal::from(pos.leg1_shares) * pos.leg1_price + pos.leg1_fee;
        let pnl = net_proceeds - entry_cost;
        let holding_secs = (ts - pos.leg1_time).num_seconds();

        let symbol = pos.symbol.clone();

        let pos = &mut self.positions[idx];
        pos.state = ArbPositionState::Aborted;
        pos.exit_reason = Some(reason.to_string());
        pos.pnl = Some(pnl);

        self.closed_trades.push(StaggeredArbClosedTrade {
            symbol: symbol.clone(),
            leg1_direction: format!("{}", pos.leg1_direction),
            leg1_price: pos.leg1_price,
            leg1_time: pos.leg1_time,
            leg2_price: None,
            leg2_time: None,
            shares: pos.leg1_shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            initial_sum: pos.initial_sum,
            final_sum: None,
            entry_p_hat: pos.entry_p_hat,
            entry_sigma: pos.entry_sigma,
            best_sum_seen: pos.best_sum_seen,
            s0: pos.s0,
            window_duration_secs: pos.window_duration_secs,
            entry_delta: pos.entry_greeks.map(|g| g.delta),
            entry_gamma: pos.entry_greeks.map(|g| g.gamma),
            entry_theta: pos.entry_greeks.map(|g| g.theta),
            entry_fair_value: pos.entry_greeks.map(|g| g.fair_value),
        });

        self.recorder.record_exit(&BacktestSignal {
            signal_type: SignalType::Exit,
            symbol: symbol.clone(),
            direction: format!("{}", pos.leg1_direction),
            timestamp: ts,
            p_hat: Some(pos.entry_p_hat),
            ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            sigma: Some(pos.entry_sigma),
            market_price: Some(sim_result.fill_price),
            spot_price: None,
            s0: Some(pos.s0),
            time_remaining_secs: None,
            filter_reason: None,
            exit_reason: Some(reason.to_string()),
            exit_price: Some(sim_result.fill_price),
        });

        self.recorder.record_trade(&PendingTrade {
            symbol,
            direction: format!("{}", pos.leg1_direction),
            entry_time: pos.leg1_time,
            exit_time: ts,
            entry_price: pos.leg1_price,
            exit_price: sim_result.fill_price,
            shares: pos.leg1_shares as i32,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: Some(pos.entry_p_hat),
            entry_ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            entry_sigma: Some(pos.entry_sigma),
            s0: Some(pos.s0),
        });

        debug!("ABORT {} reason={} pnl={:.4}", pos.event_slug, reason, pnl);
    }

    pub(super) fn resolve_positions(
        &mut self,
        symbol: &str,
        event_slug: &str,
        up_won: bool,
        ts: DateTime<Utc>,
    ) {
        let mut to_fill: Vec<usize> = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol != symbol || pos.event_slug != event_slug {
                continue;
            }
            if pos.state != ArbPositionState::Leg1Filled {
                continue;
            }
            to_fill.push(i);
        }

        to_fill.sort_by(|a, b| b.cmp(a));
        for idx in to_fill {
            self.settle_position_with_outcome(idx, up_won, ts, "settlement");
        }
    }

    pub(super) fn settle_position_with_outcome(
        &mut self,
        idx: usize,
        up_won: bool,
        ts: DateTime<Utc>,
        reason: &str,
    ) {
        if idx >= self.positions.len() || self.positions[idx].state != ArbPositionState::Leg1Filled
        {
            return;
        }

        let pos = &self.positions[idx];
        let leg2_shares = pos.leg2_shares.unwrap_or(0);
        let leg2_price = pos.leg2_price.unwrap_or(Decimal::ZERO);
        let leg2_fee = pos.leg2_fee.unwrap_or(Decimal::ZERO);
        let winner_matches_leg1 = matches!(pos.leg1_direction, Direction::Up) == up_won;
        let payout = if winner_matches_leg1 {
            Decimal::from(pos.leg1_shares)
        } else {
            Decimal::from(leg2_shares)
        };
        let total_cost = Decimal::from(pos.leg1_shares) * pos.leg1_price
            + pos.leg1_fee
            + Decimal::from(leg2_shares) * leg2_price
            + leg2_fee;
        self.equity += payout;
        let pnl = payout - total_cost;
        let holding_secs = (ts - pos.leg1_time).num_seconds();
        let symbol = pos.symbol.clone();

        let pos = &mut self.positions[idx];
        pos.state = ArbPositionState::Settled;
        pos.exit_reason = Some(reason.to_string());
        pos.pnl = Some(pnl);

        self.closed_trades.push(StaggeredArbClosedTrade {
            symbol: symbol.clone(),
            leg1_direction: format!("{}", pos.leg1_direction),
            leg1_price: pos.leg1_price,
            leg1_time: pos.leg1_time,
            leg2_price: if leg2_shares > 0 {
                Some(leg2_price)
            } else {
                None
            },
            leg2_time: pos.leg2_time,
            shares: pos.leg1_shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            initial_sum: pos.initial_sum,
            final_sum: if leg2_shares > 0 {
                Some(pos.leg1_price + leg2_price)
            } else {
                None
            },
            entry_p_hat: pos.entry_p_hat,
            entry_sigma: pos.entry_sigma,
            best_sum_seen: pos.best_sum_seen,
            s0: pos.s0,
            window_duration_secs: pos.window_duration_secs,
            entry_delta: pos.entry_greeks.map(|g| g.delta),
            entry_gamma: pos.entry_greeks.map(|g| g.gamma),
            entry_theta: pos.entry_greeks.map(|g| g.theta),
            entry_fair_value: pos.entry_greeks.map(|g| g.fair_value),
        });

        let exit_price = if winner_matches_leg1 {
            Decimal::ONE
        } else {
            Decimal::ZERO
        };

        self.recorder.record_exit(&BacktestSignal {
            signal_type: SignalType::Exit,
            symbol: symbol.clone(),
            direction: format!("{}", pos.leg1_direction),
            timestamp: ts,
            p_hat: Some(pos.entry_p_hat),
            ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            sigma: Some(pos.entry_sigma),
            market_price: Some(exit_price),
            spot_price: None,
            s0: Some(pos.s0),
            time_remaining_secs: None,
            filter_reason: None,
            exit_reason: Some(reason.to_string()),
            exit_price: Some(exit_price),
        });

        self.recorder.record_trade(&PendingTrade {
            symbol,
            direction: format!("{}", pos.leg1_direction),
            entry_time: pos.leg1_time,
            exit_time: ts,
            entry_price: pos.leg1_price,
            exit_price,
            shares: pos.leg1_shares as i32,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: Some(pos.entry_p_hat),
            entry_ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            entry_sigma: Some(pos.entry_sigma),
            s0: Some(pos.s0),
        });
    }

    #[allow(dead_code)]
    pub(super) fn settle_single_leg(&mut self, idx: usize, exit_price: Decimal, ts: DateTime<Utc>) {
        let pos = &self.positions[idx];
        let proceeds = exit_price * Decimal::from(pos.leg1_shares);
        self.equity += proceeds;

        let entry_cost = Decimal::from(pos.leg1_shares) * pos.leg1_price + pos.leg1_fee;
        let pnl = proceeds - entry_cost;
        let holding_secs = (ts - pos.leg1_time).num_seconds();

        let symbol = pos.symbol.clone();

        let pos = &mut self.positions[idx];
        pos.state = ArbPositionState::Settled;
        pos.exit_reason = Some("settlement".to_string());
        pos.pnl = Some(pnl);

        self.closed_trades.push(StaggeredArbClosedTrade {
            symbol: symbol.clone(),
            leg1_direction: format!("{}", pos.leg1_direction),
            leg1_price: pos.leg1_price,
            leg1_time: pos.leg1_time,
            leg2_price: None,
            leg2_time: None,
            shares: pos.leg1_shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: "settlement".to_string(),
            initial_sum: pos.initial_sum,
            final_sum: None,
            entry_p_hat: pos.entry_p_hat,
            entry_sigma: pos.entry_sigma,
            best_sum_seen: pos.best_sum_seen,
            s0: pos.s0,
            window_duration_secs: pos.window_duration_secs,
            entry_delta: pos.entry_greeks.map(|g| g.delta),
            entry_gamma: pos.entry_greeks.map(|g| g.gamma),
            entry_theta: pos.entry_greeks.map(|g| g.theta),
            entry_fair_value: pos.entry_greeks.map(|g| g.fair_value),
        });

        self.recorder.record_trade(&PendingTrade {
            symbol,
            direction: format!("{}", pos.leg1_direction),
            entry_time: pos.leg1_time,
            exit_time: ts,
            entry_price: pos.leg1_price,
            exit_price,
            shares: pos.leg1_shares as i32,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: "settlement".to_string(),
            entry_p_hat: Some(pos.entry_p_hat),
            entry_ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            entry_sigma: Some(pos.entry_sigma),
            s0: Some(pos.s0),
        });
    }

    pub(super) fn close_remaining_positions(&mut self) {
        let ts = self.data_range_end.unwrap_or(Utc::now());
        let indices: Vec<usize> = self
            .positions
            .iter()
            .enumerate()
            .filter(|(_, p)| p.state == ArbPositionState::Leg1Filled)
            .map(|(i, _)| i)
            .rev()
            .collect();
        for idx in indices {
            let pos = &self.positions[idx];
            let pm_asks = self.pm_asks_by_event.get(&pos.event_slug).copied();
            let other_ask = pm_asks.and_then(|a| match pos.leg1_direction {
                Direction::Up => a.1,
                Direction::Down => a.0,
            });
            match other_ask {
                Some(ask) => self.fill_leg2(idx, ask, "data_exhausted", ts),
                None => self.abort_position(idx, "data_exhausted", ts),
            }
        }
    }
}
