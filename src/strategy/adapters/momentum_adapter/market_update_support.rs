use super::*;
use crate::strategy::volatility::normal_cdf;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;

impl MomentumStrategyAdapter {
    fn has_pending_exit_for_token(&self, token_id: &str) -> bool {
        self.pending_orders
            .values()
            .any(|o| o.kind == MomentumOrderKind::Exit && o.token_id == token_id)
    }

    fn daily_limit_reached(&mut self) -> bool {
        if self.config.max_daily_trades == 0 {
            return false;
        }

        let now = Utc::now();
        if now.date_naive() != self.last_reset.date_naive() {
            self.daily_trades = 0;
            self.last_reset = now;
        }

        self.daily_trades >= self.config.max_daily_trades
    }

    fn in_cooldown(&self, symbol: &str) -> bool {
        if let Some(last_trade) = self.cooldowns.get(symbol) {
            let elapsed = (Utc::now() - *last_trade).num_seconds();
            elapsed < self.config.cooldown_secs as i64
        } else {
            false
        }
    }

    fn pick_entry_event_in_window<'a>(
        &'a self,
        event_list: &'a [EventState],
        now: DateTime<Utc>,
    ) -> Option<&'a EventState> {
        event_list
            .iter()
            .filter(|e| {
                let secs_remaining = (e.end_time - now).num_seconds();
                secs_remaining >= self.config.min_time_remaining_secs as i64
                    && secs_remaining <= self.config.max_time_remaining_secs as i64
            })
            .min_by_key(|e| e.end_time)
    }

    fn estimate_non_directional_fair_value(&self, move_pct: Decimal) -> Decimal {
        let x = move_pct * dec!(100);
        let fair = dec!(0.50) + x * dec!(1.5);
        fair.clamp(dec!(0.01), dec!(0.99))
    }

    fn non_directional_ev_after_costs(
        &self,
        fair_value: Decimal,
        entry_price: Decimal,
    ) -> Option<Decimal> {
        if entry_price <= Decimal::ZERO || fair_value <= Decimal::ZERO {
            return None;
        }

        let fee = entry_price * dec!(0.02);
        let slippage = entry_price * dec!(0.01);
        Some(fair_value - entry_price - fee - slippage)
    }

    fn check_momentum(&self, symbol: &str) -> Option<(Direction, Decimal)> {
        let state = self.cex_prices.get(symbol)?;
        let old_price = state.get_price_at(self.config.lookback_secs)?;

        if old_price <= Decimal::ZERO {
            return None;
        }

        let move_pct = (state.price - old_price) / old_price;
        if move_pct.abs() < self.config.min_move_pct {
            return None;
        }

        let direction = if move_pct > Decimal::ZERO {
            Direction::Up
        } else {
            Direction::Down
        };

        Some((direction, move_pct.abs()))
    }

    async fn check_directional_entry(
        &mut self,
        symbol: &str,
        price: &Decimal,
        timestamp: DateTime<Utc>,
    ) -> Option<StrategyAction> {
        let _cex = self.cex_prices.get(symbol)?;
        let event = {
            let event_list = self.events.get(symbol)?;
            self.pick_entry_event_in_window(event_list, timestamp)?
                .clone()
        };

        let Some(open_price) = event.open_price else {
            return None;
        };
        if open_price <= Decimal::ZERO || *price <= Decimal::ZERO {
            return None;
        }

        let s0 = open_price;
        let st = *price;
        let sigma = self.config.directional_vol_floor.max(1e-9);
        let log_return = ((st / s0).to_f64()?).ln();
        let window_secs = event.window_secs.max(1);
        let t_years = window_secs as f64 / (365.0 * 24.0 * 60.0 * 60.0);
        let z = (log_return + 0.5 * sigma * sigma * t_years) / (sigma * t_years.sqrt());
        let p_hat = normal_cdf(z);
        let effective_p = match self.config.min_confidence.partial_cmp(&0.5) {
            Some(std::cmp::Ordering::Greater) if p_hat >= self.config.min_confidence => p_hat,
            Some(std::cmp::Ordering::Greater) if p_hat <= (1.0 - self.config.min_confidence) => {
                1.0 - p_hat
            }
            Some(_) if p_hat >= 0.5 => p_hat,
            Some(_) => 1.0 - p_hat,
            None => return None,
        };

        let direction = if p_hat >= 0.5 {
            Direction::Up
        } else {
            Direction::Down
        };

        let token_id = match direction {
            Direction::Up => event.up_token_id.clone(),
            Direction::Down => event.down_token_id.clone(),
        };
        let quote = self.pm_quotes.get(&token_id)?;
        let entry_price = quote.best_ask?;
        if entry_price > self.config.max_entry_price || entry_price <= Decimal::ZERO {
            return None;
        }

        let ev_net = effective_p - entry_price.to_f64()? - 0.03;
        if ev_net < self.directional_entry_threshold {
            return None;
        }

        let secs_remaining = (event.end_time - timestamp).num_seconds().max(0) as f64;
        self.record_directional_signal(
            symbol,
            direction,
            &event.event_id,
            &token_id,
            p_hat,
            effective_p,
            ev_net,
            entry_price,
            sigma,
            s0,
            st,
            secs_remaining,
            window_secs,
        )
        .await;

        self.generate_entry(symbol, direction, entry_price)
    }

    pub(super) fn get_entry_price(&self, symbol: &str, direction: Direction) -> Option<Decimal> {
        let now = Utc::now();
        let event_list = self.events.get(symbol)?;
        let event = self.pick_entry_event_in_window(event_list, now)?;
        let token_id = match direction {
            Direction::Up => &event.up_token_id,
            Direction::Down => &event.down_token_id,
        };
        let quote = self.pm_quotes.get(token_id)?;
        quote.best_ask
    }

    pub(super) fn generate_entry(
        &mut self,
        symbol: &str,
        direction: Direction,
        entry_price: Decimal,
    ) -> Option<StrategyAction> {
        let now = Utc::now();
        let event = {
            let event_list = self.events.get(symbol)?;
            self.pick_entry_event_in_window(event_list, now)?.clone()
        };

        let (market_slug, token_id, market_side) = match direction {
            Direction::Up => (symbol.to_string(), event.up_token_id.clone(), Side::Up),
            Direction::Down => (symbol.to_string(), event.down_token_id.clone(), Side::Down),
        };

        let shares = if let Some(fixed_amount_usd) = self.fixed_amount_usd {
            let price_f64 = entry_price.to_string().parse::<f64>().ok()?;
            if price_f64 <= 0.0 {
                return None;
            }
            (fixed_amount_usd / price_f64).floor().max(1.0) as u64
        } else {
            self.config.shares_per_trade
        };

        let client_order_id = format!("{}_entry_{}_{}", self.id, symbol, now.timestamp_millis());

        self.pending_orders.insert(
            client_order_id.clone(),
            MomentumOrderTrack {
                kind: MomentumOrderKind::Entry,
                symbol: symbol.to_string(),
                token_id: token_id.clone(),
                side: market_side,
                direction,
                shares,
                price: entry_price,
            },
        );

        info!(
            "[{}] Entry signal: {} {} @ {:.2}¢ ({} shares, ${:.2})",
            self.id,
            direction,
            symbol,
            entry_price * dec!(100),
            shares,
            entry_price.to_string().parse::<f64>().unwrap_or(0.0) * shares as f64,
        );

        Some(super::crypto_submit_intent(
            client_order_id,
            market_slug,
            token_id,
            market_side,
            true,
            shares,
            entry_price,
            5,
        ))
    }

    pub(super) async fn handle_market_update(
        &mut self,
        update: &MarketUpdate,
    ) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        match update {
            MarketUpdate::BinancePrice {
                symbol,
                price,
                timestamp,
            } => {
                if let Some(state) = self.cex_prices.get_mut(symbol) {
                    state.update(*price, *timestamp, self.config.lookback_secs);
                } else {
                    self.cex_prices.insert(
                        symbol.clone(),
                        CexPriceState::new(symbol.clone(), *price, *timestamp),
                    );
                }

                if !self.enabled {
                    return Ok(actions);
                }

                if self.daily_limit_reached() {
                    return Ok(actions);
                }

                if self.in_cooldown(symbol) {
                    return Ok(actions);
                }

                if self.positions.len() >= self.config.max_positions {
                    return Ok(actions);
                }

                if self.positions.values().any(|p| &p.symbol == symbol) {
                    return Ok(actions);
                }

                if self.config.directional_mode {
                    if let Some(action) = self
                        .check_directional_entry(symbol, price, *timestamp)
                        .await
                    {
                        self.cooldowns.insert(symbol.clone(), Utc::now());
                        actions.push(action);
                    }
                    return Ok(actions);
                }

                if let Some((direction, move_pct)) = self.check_momentum(symbol) {
                    match self.get_entry_price(symbol, direction) {
                        Some(entry_price) => {
                            if entry_price <= self.config.max_entry_price {
                                let fair_value = self.estimate_non_directional_fair_value(move_pct);
                                let edge = fair_value - entry_price;

                                if edge < self.config.min_edge {
                                    debug!(
                                        "[{}] {} {} edge {:.1}% < min {:.1}%, skip",
                                        self.id,
                                        symbol,
                                        direction,
                                        edge * dec!(100),
                                        self.config.min_edge * dec!(100)
                                    );
                                } else {
                                    let ev_net = self
                                        .non_directional_ev_after_costs(fair_value, entry_price)
                                        .unwrap_or(Decimal::ZERO);
                                    if ev_net <= Decimal::ZERO {
                                        debug!(
                                            "[{}] {} {} ev_net {:.2}% <= 0 after fees/slippage, skip",
                                            self.id,
                                            symbol,
                                            direction,
                                            ev_net * dec!(100)
                                        );
                                    } else if let Some(action) =
                                        self.generate_entry(symbol, direction, entry_price)
                                    {
                                        self.cooldowns.insert(symbol.clone(), Utc::now());

                                        actions.push(StrategyAction::LogEvent {
                                            event: StrategyEvent::new(
                                                StrategyEventType::SignalDetected,
                                                format!(
                                                    "{} {} signal: {:.2}% move, entry {:.0}¢ edge {:.1}% ev {:.1}%",
                                                    symbol,
                                                    direction,
                                                    move_pct * dec!(100),
                                                    entry_price * dec!(100),
                                                    edge * dec!(100),
                                                    ev_net * dec!(100),
                                                ),
                                            ),
                                        });

                                        actions.push(action);
                                    }
                                }
                            } else {
                                debug!(
                                    "[{}] Entry price {:.0}¢ > max {:.0}¢ for {}",
                                    self.id,
                                    entry_price * dec!(100),
                                    self.config.max_entry_price * dec!(100),
                                    symbol
                                );
                            }
                        }
                        None => {
                            let now = Utc::now();
                            if let Some(event_list) = self.events.get(symbol) {
                                if let Some(event) =
                                    self.pick_entry_event_in_window(event_list, now)
                                {
                                    let token_id = match direction {
                                        Direction::Up => &event.up_token_id,
                                        Direction::Down => &event.down_token_id,
                                    };
                                    if let Some(q) = self.pm_quotes.get(token_id) {
                                        debug!(
                                            "[{}] Quote has no best_ask for {} (bid={:?})",
                                            self.id, direction, q.best_bid
                                        );
                                    } else {
                                        debug!(
                                            "[{}] No quote for token {} ({})",
                                            self.id,
                                            &token_id[..8],
                                            direction
                                        );
                                    }
                                } else {
                                    let nearest = event_list
                                        .iter()
                                        .filter(|e| e.end_time > now)
                                        .min_by_key(|e| e.end_time)
                                        .map(|e| (e.end_time - now).num_seconds())
                                        .unwrap_or(-1);
                                    debug!(
                                        "[{}] No event in timing window for {} ({}..{}s, nearest={}s)",
                                        self.id,
                                        symbol,
                                        self.config.min_time_remaining_secs,
                                        self.config.max_time_remaining_secs,
                                        nearest
                                    );
                                }
                            } else {
                                debug!("[{}] No event mapped for symbol {}", self.id, symbol);
                            }
                        }
                    }
                }
            }

            MarketUpdate::BinanceL2 { .. } => {}

            MarketUpdate::PolymarketQuote {
                token_id,
                quote,
                timestamp,
                ..
            } => {
                let is_new = !self.pm_quotes.contains_key(token_id);
                self.pm_quotes.insert(
                    token_id.clone(),
                    PmQuoteState {
                        token_id: token_id.clone(),
                        best_bid: quote.best_bid,
                        best_ask: quote.best_ask,
                        timestamp: *timestamp,
                    },
                );

                if let Some(pos) = self.positions.get_mut(token_id) {
                    pos.current_price = quote.best_bid.or(quote.best_ask);
                }

                if is_new {
                    info!(
                        "[{}] LOB: token {} bid: {}¢ ask: {}¢",
                        self.id,
                        &token_id[..8],
                        quote
                            .best_bid
                            .map(|b| (b * dec!(100)).to_string())
                            .unwrap_or("-".into()),
                        quote
                            .best_ask
                            .map(|a| (a * dec!(100)).to_string())
                            .unwrap_or("-".into())
                    );
                }

                if !self.config.hold_to_resolution {
                    let trigger = self.positions.get(token_id).and_then(|pos| {
                        let current = pos.current_price?;
                        if pos.entry_price.is_zero() {
                            return None;
                        }
                        let pnl_pct = (current - pos.entry_price) / pos.entry_price;
                        if pnl_pct >= self.exit_config.take_profit_pct {
                            Some((pos.clone(), "take_profit", pnl_pct))
                        } else if pnl_pct <= -self.exit_config.stop_loss_pct {
                            Some((pos.clone(), "stop_loss", pnl_pct))
                        } else {
                            None
                        }
                    });

                    if let Some((pos, reason, pnl_pct)) = trigger {
                        if self.has_pending_exit_for_token(&pos.token_id) {
                            return Ok(actions);
                        }

                        let exit_price = match quote.best_bid {
                            Some(p) if p > Decimal::ZERO => p,
                            _ => return Ok(actions),
                        };
                        let client_order_id = format!(
                            "{}_exit_{}_{}",
                            self.id,
                            pos.symbol,
                            Utc::now().timestamp_millis()
                        );
                        self.pending_orders.insert(
                            client_order_id.clone(),
                            MomentumOrderTrack {
                                kind: MomentumOrderKind::Exit,
                                symbol: pos.symbol.clone(),
                                token_id: pos.token_id.clone(),
                                side: pos.side,
                                direction: pos.direction,
                                shares: pos.shares,
                                price: exit_price,
                            },
                        );

                        if reason == "take_profit" {
                            info!(
                                "[{}] Take profit triggered: {} {:.1}% @ {:.2}¢",
                                self.id,
                                pos.symbol,
                                pnl_pct * dec!(100),
                                exit_price * dec!(100)
                            );
                        } else {
                            warn!(
                                "[{}] Stop loss triggered: {} {:.1}% @ {:.2}¢",
                                self.id,
                                pos.symbol,
                                pnl_pct * dec!(100),
                                exit_price * dec!(100)
                            );
                        }

                        actions.push(super::crypto_submit_intent(
                            client_order_id,
                            pos.symbol.clone(),
                            pos.token_id.clone(),
                            pos.side,
                            false,
                            pos.shares,
                            exit_price,
                            8,
                        ));
                    }
                }
            }

            MarketUpdate::EventDiscovered {
                event_id,
                series_id,
                up_token,
                down_token,
                end_time,
                price_to_beat: _,
                title: _,
                condition_id: _,
            } => {
                let Some((symbol, window_secs)) = symbol_and_window_for_series(series_id) else {
                    return Ok(actions);
                };

                let event_vec = self.events.entry(symbol.to_string()).or_default();
                let now = chrono::Utc::now();
                event_vec.retain(|e| e.end_time > now);

                if event_vec.iter().any(|e| e.event_id == *event_id) {
                    return Ok(actions);
                }

                let open_price = if self.config.directional_mode {
                    self.cex_prices.get(symbol).map(|s| s.price)
                } else {
                    None
                };

                event_vec.push(EventState {
                    event_id: event_id.clone(),
                    symbol: symbol.to_string(),
                    up_token_id: up_token.clone(),
                    down_token_id: down_token.clone(),
                    end_time: *end_time,
                    open_price,
                    window_secs,
                });

                debug!(
                    "[{}] Event discovered: {} for {} ({}m window, ends {})",
                    self.id,
                    event_id,
                    symbol,
                    window_secs / 60,
                    end_time
                );
            }

            MarketUpdate::EventExpired { event_id } => {
                for list in self.events.values_mut() {
                    list.retain(|e| &e.event_id != event_id);
                }
            }

            MarketUpdate::BinanceKline { .. }
            | MarketUpdate::BinanceFunding { .. }
            | MarketUpdate::BinanceLiquidation { .. }
            | MarketUpdate::DeribitIV { .. } => {}
        }

        Ok(actions)
    }
}
