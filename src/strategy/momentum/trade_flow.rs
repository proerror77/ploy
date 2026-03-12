use super::*;

impl MomentumEngine {
    pub(super) async fn maybe_enter(
        &self,
        signal: MomentumSignal,
        event: &EventInfo,
    ) -> Result<()> {
        // Check daily trade limit
        if self.daily_limit_reached().await {
            debug!(
                "Daily trade limit reached ({}), skipping",
                self.config.max_daily_trades
            );
            return Ok(());
        }

        // Check cooldown first (fast check)
        if self.in_cooldown(&signal.symbol).await {
            debug!("{} in cooldown, skipping", signal.symbol);
            return Ok(());
        }

        let _entry_guard = self.entry_mutex.lock().await;

        // CRITICAL: Check if we already have a position in this symbol or event
        // This prevents duplicate orders from momentum + volatility signals
        {
            let positions = self.positions.read().await;

            // Check by symbol
            if positions.values().any(|p| p.symbol == signal.symbol) {
                debug!(
                    "Already have position in {}, skipping duplicate entry",
                    signal.symbol
                );
                return Ok(());
            }

            // Check by condition_id (same event)
            if positions
                .values()
                .any(|p| p.condition_id == event.condition_id)
            {
                debug!(
                    "Already have position in event {}, skipping",
                    event.condition_id
                );
                return Ok(());
            }
        }

        // Calculate window ID for this event
        let window_id = WindowRiskTracker::window_id(&event.end_time);

        // Check window exposure limit (cross-symbol risk control)
        let estimated_cost = signal.pm_price * Decimal::from(self.config.shares_per_trade);
        {
            let tracker = self.window_tracker.read().await;

            // Check if window already has an executed trade (best_edge_only mode)
            if self.config.best_edge_only && tracker.has_executed(&window_id) {
                debug!(
                    "Window {} already has trade, skipping {}",
                    window_id, signal.symbol
                );
                return Ok(());
            }

            // Check exposure limit
            if self.config.max_window_exposure_usd > Decimal::ZERO {
                let current_exposure = tracker.get_exposure(&window_id);
                if current_exposure + estimated_cost > self.config.max_window_exposure_usd {
                    debug!(
                        "Window {} exposure ${:.2} + ${:.2} would exceed limit ${:.2}",
                        window_id,
                        current_exposure,
                        estimated_cost,
                        self.config.max_window_exposure_usd
                    );
                    return Ok(());
                }
            }
        }

        // If best_edge_only mode, queue signal for later selection
        if self.config.best_edge_only {
            let pending = PendingSignal {
                signal: signal.clone(),
                event: event.clone(),
                edge: signal.edge,
                cost_usd: estimated_cost,
                timestamp: Utc::now(),
            };

            {
                let mut tracker = self.window_tracker.write().await;
                tracker.add_pending_signal(&window_id, pending);
            }

            info!(
                "📋 Queued: {} {} edge={:.2}% (window {})",
                signal.symbol,
                signal.direction,
                signal.edge * dec!(100),
                window_id
            );

            return Ok(());
        }

        // Determine base shares to trade - use fund manager if available
        let base_shares = if let Some(ref fm) = self.fund_manager {
            // Use fund manager for balance check and position sizing
            match fm
                .can_open_position(&event.condition_id, &signal.symbol, signal.pm_price)
                .await
            {
                Ok(PositionSizeResult::Approved { shares, amount_usd }) => {
                    info!(
                        "💰 Fund manager approved: {} shares @ {:.2}¢ = ${:.2}",
                        shares,
                        signal.pm_price * dec!(100),
                        amount_usd
                    );
                    shares
                }
                Ok(PositionSizeResult::Rejected(reason)) => {
                    debug!("Fund manager rejected: {}", reason);
                    return Ok(());
                }
                Err(e) => {
                    // Don't fall back to CLI shares - this bypasses risk management!
                    warn!("Fund manager error: {}, skipping trade for safety", e);
                    return Ok(());
                }
            }
        } else {
            // No fund manager - check max positions limit
            let positions = self.positions.read().await;
            if positions.len() >= self.config.max_positions {
                debug!(
                    "Max positions reached ({}), skipping",
                    self.config.max_positions
                );
                return Ok(());
            }
            // Position duplicate check already done above
            drop(positions);
            self.config.shares_per_trade
        };
        let shares_to_trade = self.apply_signal_position_sizing(base_shares, &signal);
        if shares_to_trade < 5 {
            debug!(
                "Position size {} below Polymarket minimum 5 shares (base={})",
                shares_to_trade, base_shares
            );
            return Ok(());
        }

        // Execute entry
        let token_id = match signal.direction {
            Direction::Up => &event.up_token_id,
            Direction::Down => &event.down_token_id,
        };

        // Log entry signal with mode-specific info
        let time_remaining = event.time_remaining().num_seconds();
        if self.config.hold_to_resolution {
            info!(
                "🎯 CONFIRMATORY ENTRY: {} {} @ {:.2}¢ | {}s to resolution | CEX: {:.2}%",
                signal.symbol,
                signal.direction,
                signal.pm_price * dec!(100),
                time_remaining,
                signal.cex_move_pct * dec!(100),
            );
            info!(
                "   → Expected payout: $1.00 (profit: {:.0}¢ per share)",
                (dec!(1) - signal.pm_price) * dec!(100)
            );
        } else {
            info!(
                "ENTRY SIGNAL: {} {} @ {:.2}¢ (CEX move: {:.2}%, edge: {:.2}%, conf: {:.0}%)",
                signal.symbol,
                signal.direction,
                signal.pm_price * dec!(100),
                signal.cex_move_pct * dec!(100),
                signal.edge * dec!(100),
                signal.confidence * 100.0,
            );
        }

        if self.dry_run {
            let expected_profit = if self.config.hold_to_resolution {
                let profit_per_share = dec!(1) - signal.pm_price;
                format!(
                    " → Expected: ${:.2}",
                    profit_per_share * Decimal::from(shares_to_trade)
                )
            } else {
                String::new()
            };
            info!(
                "[DRY RUN] Would buy {} shares of {} {}{}",
                shares_to_trade, signal.symbol, signal.direction, expected_profit
            );
        } else {
            // Create and execute order with calculated shares
            let order = OrderRequest::buy_limit(
                token_id.clone(),
                signal.direction.into(),
                shares_to_trade,
                signal.pm_price,
            );

            match self.executor.execute(&order).await {
                Ok(result) => {
                    let fill_price = result.avg_fill_price.unwrap_or(signal.pm_price);
                    let tracked_shares = if result.filled_shares > 0 {
                        result.filled_shares
                    } else {
                        shares_to_trade
                    };
                    let entry_notional = fill_price * Decimal::from(tracked_shares);
                    let trade_count = self.record_trade().await;
                    info!(
                        "Order filled: {} shares @ {:.2}¢ (trade #{} today)",
                        tracked_shares,
                        fill_price * dec!(100),
                        trade_count
                    );

                    // Record position with fund manager
                    if let Some(ref fm) = self.fund_manager {
                        fm.record_position_opened_with_amount(
                            &event.condition_id,
                            &signal.symbol,
                            entry_notional,
                        )
                        .await;
                    }

                    // Track position in local state
                    let position = Position {
                        token_id: token_id.clone(),
                        symbol: signal.symbol.clone(),
                        direction: signal.direction,
                        entry_price: fill_price,
                        entry_notional,
                        shares: tracked_shares,
                        entry_time: Utc::now(),
                        highest_price: fill_price,
                        event_end_time: event.end_time,
                        event_slug: event.slug.clone(),
                        condition_id: event.condition_id.clone(),
                        entry_p_hat: None,
                        window_open_price: None,
                    };

                    let mut positions = self.positions.write().await;
                    positions.insert(signal.symbol.clone(), position);

                    // Log trade entry
                    if let Some(ref logger) = self.trade_logger {
                        logger
                            .record_entry(
                                &signal.symbol,
                                &event.slug,
                                &event.condition_id,
                                &format!("{}", signal.direction),
                                fill_price,
                                tracked_shares,
                                signal.cex_move_pct,
                                signal.edge,
                            )
                            .await;
                    }
                    // Update cooldown only on confirmed fill
                    let mut last_trade = self.last_trade_time.write().await;
                    last_trade.insert(signal.symbol.clone(), Utc::now());
                }
                Err(e) => {
                    error!("Order failed: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Execute position exit
    pub(super) async fn execute_exit(
        &self,
        symbol: &str,
        price: Decimal,
        reason: ExitReason,
    ) -> Result<()> {
        let position = {
            let mut positions = self.positions.write().await;
            match positions.remove(symbol) {
                Some(p) => p,
                None => return Ok(()),
            }
        };

        let pnl_pct = position.pnl_pct(price);
        let pnl_usd = pnl_pct * Decimal::from(position.shares) * position.entry_price;

        info!(
            "EXIT: {} {} @ {:.2}¢ - {} (P&L: {:.2}% / ${:.2})",
            symbol,
            position.direction,
            price * dec!(100),
            reason,
            pnl_pct * dec!(100),
            pnl_usd,
        );

        let mut closed = self.dry_run;

        if self.dry_run {
            info!("[DRY RUN] Would sell {} shares", position.shares);
        } else {
            // Create sell order
            let order = OrderRequest::sell_limit(
                position.token_id.clone(),
                position.direction.into(),
                position.shares,
                price,
            );

            match self.executor.execute(&order).await {
                Ok(result) => {
                    let exit_price = result.avg_fill_price.unwrap_or(price);
                    info!(
                        "Exit filled: {} shares @ {:.2}¢",
                        result.filled_shares,
                        exit_price * dec!(100)
                    );
                    closed = true;
                }
                Err(e) => {
                    error!("Exit order failed: {}", e);
                    // Re-add position on failure
                    let mut positions = self.positions.write().await;
                    positions.insert(symbol.to_string(), position.clone());
                    closed = false;
                }
            }
        }

        if closed {
            if let Some(ref fm) = self.fund_manager {
                let released_notional = if position.entry_notional > Decimal::ZERO {
                    position.entry_notional
                } else {
                    position.entry_price * Decimal::from(position.shares)
                };
                fm.record_position_closed_with_amount(
                    &position.condition_id,
                    &position.symbol,
                    released_notional,
                )
                .await;
            }
        }

        Ok(())
    }

    /// Check if symbol is in cooldown period
    async fn in_cooldown(&self, symbol: &str) -> bool {
        let last_trade = self.last_trade_time.read().await;

        if let Some(last_time) = last_trade.get(symbol) {
            let elapsed = Utc::now() - *last_time;
            return elapsed.num_seconds() < self.config.cooldown_secs as i64;
        }

        false
    }

    /// Process pending signals and execute best edge (if ready)
    pub(super) async fn process_pending_signals(&self) -> Result<()> {
        if !self.config.best_edge_only {
            return Ok(());
        }

        let ready_windows = {
            let tracker = self.window_tracker.read().await;
            tracker.get_ready_windows(self.config.signal_collection_delay_ms)
        };

        for window_id in ready_windows {
            // Get the best signal for this window
            let best_signal = {
                let tracker = self.window_tracker.read().await;

                // Skip if already executed
                if tracker.has_executed(&window_id) {
                    continue;
                }

                tracker.get_best_signal(&window_id)
            };

            if let Some(pending) = best_signal {
                // Check window exposure limit
                let can_execute = {
                    let tracker = self.window_tracker.read().await;
                    let current_exposure = tracker.get_exposure(&window_id);
                    let max_exposure = self.config.max_window_exposure_usd;

                    max_exposure == Decimal::ZERO
                        || current_exposure + pending.cost_usd <= max_exposure
                };

                if can_execute {
                    info!(
                        "🏆 Best edge selected: {} {} edge={:.2}% (window {})",
                        pending.signal.symbol,
                        pending.signal.direction,
                        pending.edge * dec!(100),
                        window_id
                    );

                    // Execute the trade directly
                    self.execute_pending_trade(pending.clone()).await?;

                    // Mark window as executed and add exposure
                    {
                        let mut tracker = self.window_tracker.write().await;
                        tracker.mark_executed(&window_id);
                        tracker.add_exposure(&window_id, pending.cost_usd);
                        tracker.clear_pending(&window_id);
                    }
                } else {
                    info!(
                        "⚠️ Window {} at exposure limit, skipping {}",
                        window_id, pending.signal.symbol
                    );

                    // Clear pending signals for this window
                    let mut tracker = self.window_tracker.write().await;
                    tracker.clear_pending(&window_id);
                }
            }
        }

        // Periodic cleanup
        {
            let mut tracker = self.window_tracker.write().await;
            tracker.cleanup_old();
        }

        Ok(())
    }

    /// Execute a pending trade
    async fn execute_pending_trade(&self, pending: PendingSignal) -> Result<()> {
        let signal = &pending.signal;
        let event = &pending.event;
        let _entry_guard = self.entry_mutex.lock().await;

        // Re-check if we already have position (might have changed since queueing)
        {
            let positions = self.positions.read().await;
            if positions.values().any(|p| p.symbol == signal.symbol) {
                debug!("Already have position in {}, skipping", signal.symbol);
                return Ok(());
            }
            if positions
                .values()
                .any(|p| p.condition_id == event.condition_id)
            {
                debug!(
                    "Already have position in event {}, skipping",
                    event.condition_id
                );
                return Ok(());
            }
        }

        // Get base position size
        let base_shares = if let Some(ref fm) = self.fund_manager {
            match fm
                .can_open_position(&event.condition_id, &signal.symbol, signal.pm_price)
                .await
            {
                Ok(PositionSizeResult::Approved { shares, amount_usd }) => {
                    info!(
                        "💰 Fund manager approved: {} shares @ {:.2}¢ = ${:.2}",
                        shares,
                        signal.pm_price * dec!(100),
                        amount_usd
                    );
                    shares
                }
                Ok(PositionSizeResult::Rejected(reason)) => {
                    debug!("Fund manager rejected: {}", reason);
                    return Ok(());
                }
                Err(e) => {
                    // Don't fall back to CLI shares - this bypasses risk management!
                    warn!("Fund manager error: {}, skipping trade for safety", e);
                    return Ok(());
                }
            }
        } else {
            self.config.shares_per_trade
        };
        let shares_to_trade = self.apply_signal_position_sizing(base_shares, signal);
        if shares_to_trade < 5 {
            debug!(
                "Position size {} below Polymarket minimum 5 shares (base={})",
                shares_to_trade, base_shares
            );
            return Ok(());
        }

        // Execute entry
        let token_id = match signal.direction {
            Direction::Up => &event.up_token_id,
            Direction::Down => &event.down_token_id,
        };

        if self.dry_run {
            info!(
                "[DRY RUN] Best edge trade: {} {} {} shares @ {:.2}¢",
                signal.symbol,
                signal.direction,
                shares_to_trade,
                signal.pm_price * dec!(100)
            );
        } else {
            let order = OrderRequest::buy_limit(
                token_id.clone(),
                signal.direction.into(),
                shares_to_trade,
                signal.pm_price,
            );

            match self.executor.execute(&order).await {
                Ok(result) => {
                    let fill_price = result.avg_fill_price.unwrap_or(signal.pm_price);
                    let tracked_shares = if result.filled_shares > 0 {
                        result.filled_shares
                    } else {
                        shares_to_trade
                    };
                    let entry_notional = fill_price * Decimal::from(tracked_shares);
                    let trade_count = self.record_trade().await;

                    info!(
                        "Order filled: {} shares @ {:.2}¢ (trade #{} today)",
                        tracked_shares,
                        fill_price * dec!(100),
                        trade_count
                    );

                    // Record with fund manager
                    if let Some(ref fm) = self.fund_manager {
                        fm.record_position_opened_with_amount(
                            &event.condition_id,
                            &signal.symbol,
                            entry_notional,
                        )
                        .await;
                    }

                    // Track position
                    let position = Position {
                        token_id: token_id.clone(),
                        symbol: signal.symbol.clone(),
                        direction: signal.direction,
                        entry_price: fill_price,
                        entry_notional,
                        shares: tracked_shares,
                        entry_time: Utc::now(),
                        highest_price: fill_price,
                        event_end_time: event.end_time,
                        event_slug: event.slug.clone(),
                        condition_id: event.condition_id.clone(),
                        entry_p_hat: None,
                        window_open_price: None,
                    };

                    let mut positions = self.positions.write().await;
                    positions.insert(signal.symbol.clone(), position);

                    // Log trade
                    if let Some(ref logger) = self.trade_logger {
                        logger
                            .record_entry(
                                &signal.symbol,
                                &event.slug,
                                &event.condition_id,
                                &format!("{}", signal.direction),
                                fill_price,
                                tracked_shares,
                                signal.cex_move_pct,
                                signal.edge,
                            )
                            .await;
                    }
                    // Update cooldown only on confirmed fill
                    let mut last_trade = self.last_trade_time.write().await;
                    last_trade.insert(signal.symbol.clone(), Utc::now());
                }
                Err(e) => {
                    error!("Order failed: {}", e);
                }
            }
        }

        Ok(())
    }
}
