use super::*;

impl StaggeredArbAdapter {
    pub(super) fn handle_market_update(&mut self, update: &MarketUpdate) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        match update {
            MarketUpdate::BinancePrice {
                symbol,
                price,
                timestamp,
            } => {
                self.spot_prices
                    .entry(symbol.clone())
                    .and_modify(|sp| sp.update(*price, None, *timestamp))
                    .or_insert_with(|| SpotPrice::new(*price, None, *timestamp));

                if let Some(windows) = self.active_windows.get_mut(symbol) {
                    for window in windows.iter_mut() {
                        if window.open_price.is_none() {
                            window.open_price = Some(*price);
                        }
                    }
                }
            }

            MarketUpdate::BinanceL2 {
                symbol,
                obi_5,
                timestamp,
                ..
            } => {
                if let Some(prev) = self.binance_l2_obi_5.insert(symbol.clone(), *obi_5) {
                    self.binance_l2_obi_prev_5.insert(symbol.clone(), prev);
                }
                self.binance_l2_obi_ts.insert(symbol.clone(), *timestamp);
            }

            MarketUpdate::PolymarketQuote {
                token_id,
                quote,
                timestamp,
                ..
            } => {
                if let Some(route) = self.token_to_quote_route.get(token_id) {
                    let symbol = route.symbol.clone();
                    let event_id = route.event_id.clone();
                    let direction = route.direction.clone();
                    let ask = quote.best_ask;
                    let ts = *timestamp;

                    self.record_pm_quote(&event_id, direction, ask, quote.ask_size, ts);
                    actions.extend(self.check_leg2_opportunities(&symbol, ts));
                    actions.extend(self.try_entry(&symbol, ts));
                }
            }

            MarketUpdate::EventDiscovered {
                event_id,
                series_id,
                up_token,
                down_token,
                end_time,
                condition_id,
                ..
            } => {
                let Some((symbol, window_secs)) = Self::series_to_symbol(series_id) else {
                    return actions;
                };

                if !self
                    .config
                    .backtest_config
                    .symbols
                    .iter()
                    .any(|configured| configured == symbol)
                {
                    return actions;
                }

                let backtest = &self.config.backtest_config;
                if !backtest.allowed_window_durations.is_empty() {
                    let tol = backtest.window_duration_tolerance as i64;
                    let matches = backtest
                        .allowed_window_durations
                        .iter()
                        .any(|&duration| (window_secs as i64 - duration as i64).abs() <= tol);
                    if !matches {
                        return actions;
                    }
                }

                self.token_to_quote_route.insert(
                    up_token.clone(),
                    QuoteRoute {
                        event_id: event_id.clone(),
                        symbol: symbol.to_string(),
                        direction: Direction::Up,
                    },
                );
                self.token_to_quote_route.insert(
                    down_token.clone(),
                    QuoteRoute {
                        event_id: event_id.clone(),
                        symbol: symbol.to_string(),
                        direction: Direction::Down,
                    },
                );

                let windows = self.active_windows.entry(symbol.to_string()).or_default();
                if !windows.iter().any(|window| window.event_id == *event_id) {
                    let open_price = self.spot_prices.get(symbol).map(|spot| spot.price);
                    windows.push(LiveWindow {
                        event_id: event_id.clone(),
                        symbol: symbol.to_string(),
                        up_token: up_token.clone(),
                        down_token: down_token.clone(),
                        condition_id: condition_id.clone(),
                        end_time: *end_time,
                        open_price,
                        window_secs,
                    });
                    debug!(
                        "[STAG-ARB] Window added: {} {} {}s end={}",
                        symbol,
                        event_id,
                        window_secs,
                        end_time.format("%H:%M:%S"),
                    );
                }
            }

            MarketUpdate::EventExpired { event_id } => {
                let expired_windows: Vec<LiveWindow> = self
                    .active_windows
                    .values()
                    .flat_map(|windows| windows.iter())
                    .filter(|window| window.event_id == *event_id)
                    .cloned()
                    .collect();
                for window in &expired_windows {
                    self.settle_expired_event(window, Utc::now(), &mut actions);
                }
                for windows in self.active_windows.values_mut() {
                    windows.retain(|window| window.event_id != *event_id);
                }
                self.pm_asks_by_event.remove(event_id);
                self.pm_quote_state_by_event.remove(event_id);
                self.token_to_quote_route
                    .retain(|_, route| route.event_id != *event_id);
            }
            _ => {}
        }

        actions
    }

    pub(super) fn handle_order_update(&mut self, update: &OrderUpdate) -> Vec<StrategyAction> {
        if self.dry_run {
            Vec::new()
        } else {
            self.process_live_order_update(update)
        }
    }

    pub(super) fn handle_tick(&mut self, now: DateTime<Utc>) -> Vec<StrategyAction> {
        let mut actions = self.reconcile_stale_live_orders(now);

        for windows in self.active_windows.values_mut() {
            windows.retain(|window| window.end_time > now);
        }

        let mut symbols: HashSet<String> = self.active_windows.keys().cloned().collect();
        symbols.extend(
            self.positions
                .iter()
                .filter(|position| position.state == PaperPositionState::Leg1Filled)
                .map(|position| position.symbol.clone()),
        );
        for symbol in &symbols {
            actions.extend(self.check_leg2_opportunities(symbol, now));
        }

        for symbol in &symbols {
            if self.has_opening_window_candidate(symbol, now) {
                actions.extend(self.try_entry(symbol, now));
            }
        }

        let should_print = self
            .last_summary
            .map(|timestamp| (now - timestamp).num_seconds() >= 60)
            .unwrap_or(true);
        if should_print {
            let summary = self.build_summary();
            info!("{}", summary);
            actions.push(StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::Custom("summary".to_string()),
                    summary,
                ),
            });
            self.last_summary = Some(now);
        }

        actions
    }
}
