use super::*;

impl MomentumEngine {
    /// Run the momentum strategy
    pub async fn run(
        &self,
        market_data: &CryptoDataPlaneHandle,
        mut chainlink_rx: Option<broadcast::Receiver<ChainlinkUpdate>>,
        chainlink_cache: Option<&ChainlinkPriceCache>,
    ) -> Result<()> {
        info!("Starting momentum engine (dry_run={})", self.dry_run);
        let mut binance_rx = market_data.subscribe_prices();
        let mut pm_rx = market_data.subscribe_quotes();
        let binance_cache = market_data.price_cache();
        let pm_cache = market_data.quote_cache();

        if self.config.hold_to_resolution {
            info!("=== CRYINGLITTLEBABY CONFIRMATORY MODE ===");
            info!(
                "• Entry window: {}-{}s before resolution",
                self.config.min_time_remaining_secs, self.config.max_time_remaining_secs
            );
            info!("• Hold to resolution: YES (collect $1)");
            info!(
                "• Min CEX move: {:.2}%, Max entry: {:.0}¢",
                self.config.min_move_pct * dec!(100),
                self.config.max_entry_price * dec!(100)
            );
        } else {
            info!("=== PREDICTIVE MODE (early entry) ===");
            info!(
                "Config: min_move={:.2}%, max_entry={:.0}¢, min_edge={:.1}%",
                self.config.min_move_pct * dec!(100),
                self.config.max_entry_price * dec!(100),
                self.config.min_edge * dec!(100)
            );
        }

        if let Err(e) = self.event_matcher.refresh().await {
            error!("Failed to refresh events: {}", e);
        }

        let event_matcher = &self.event_matcher;
        let refresh_interval = tokio::time::interval(Duration::from_secs(60));
        tokio::pin!(refresh_interval);

        let resolution_interval = tokio::time::interval(Duration::from_secs(30));
        tokio::pin!(resolution_interval);

        let signal_process_interval = tokio::time::interval(Duration::from_millis(500));
        tokio::pin!(signal_process_interval);

        if self.config.best_edge_only {
            info!("=== CROSS-SYMBOL RISK CONTROL ===");
            info!("• Best edge only: YES (queue signals, select highest edge)");
            info!(
                "• Signal collection delay: {}ms",
                self.config.signal_collection_delay_ms
            );
            info!(
                "• Max window exposure: ${:.2}",
                self.config.max_window_exposure_usd
            );
        }

        let has_chainlink = chainlink_rx.is_some();
        if has_chainlink {
            info!("=== DIRECTIONAL PREDICTION MODE ===");
            info!("• Ground truth: Chainlink RTDS (not Binance)");
            info!("• Fee model: parabolic (crypto, fee_rate=0.25, exp=2)");
            info!(
                "• Entry threshold: EV_net >= {:.1}%",
                self.entry_threshold * 100.0
            );
        }

        if self.config.directional_mode {
            info!("=== DIRECTIONAL MODE (BINANCE AS ORACLE) ===");
            info!("• Ground truth: Binance spot price (Chainlink proxy)");
            info!("• Fee model: parabolic (crypto, fee_rate=0.25, exp=2)");
            info!(
                "• Entry threshold: EV_net >= {:.1}%",
                self.entry_threshold * 100.0
            );
            info!("• Vol floor: {:.4}", self.config.directional_vol_floor);
            info!("• Symbols: {:?}", self.config.symbols);
        }

        loop {
            tokio::select! {
                Ok(cl_update) = async {
                    match chainlink_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(cl_cache) = chainlink_cache {
                        if let Err(e) = self.on_chainlink_update(&cl_update, cl_cache, &binance_cache, &pm_cache).await {
                            error!("Error processing Chainlink update: {}", e);
                        }
                    }
                }

                Ok(price_update) = binance_rx.recv() => {
                    if !has_chainlink {
                        if let Err(e) = self.on_cex_update(&price_update, &binance_cache, &pm_cache).await {
                            error!("Error processing CEX update: {}", e);
                        }
                    }
                }

                Ok(quote_update) = pm_rx.recv() => {
                    if let Err(e) = self.on_pm_update(&quote_update).await {
                        error!("Error processing PM update: {}", e);
                    }
                }

                _ = refresh_interval.tick() => {
                    if let Err(e) = event_matcher.refresh().await {
                        warn!("Failed to refresh events: {}", e);
                    }
                }

                _ = resolution_interval.tick() => {
                    if self.config.hold_to_resolution {
                        let (_won, _lost, _payout) = self.check_resolved_positions().await;
                    }
                }

                _ = signal_process_interval.tick() => {
                    if self.config.best_edge_only {
                        if let Err(e) = self.process_pending_signals().await {
                            error!("Error processing pending signals: {}", e);
                        }
                    }
                }
            }
        }
    }

    /// Handle CEX price update - check for entry signals
    async fn on_cex_update(
        &self,
        update: &PriceUpdate,
        binance_cache: &PriceCache,
        pm_cache: &QuoteCache,
    ) -> Result<()> {
        let symbol = &update.symbol;
        if !self.config.symbols.contains(symbol) {
            return Ok(());
        }

        let spot = match binance_cache.get(symbol).await {
            Some(s) => s,
            None => return Ok(()),
        };

        let event = if self.config.hold_to_resolution {
            match self
                .event_matcher
                .find_event_with_timing(
                    symbol,
                    self.config.min_time_remaining_secs,
                    self.config.max_time_remaining_secs as i64,
                    true,
                )
                .await
            {
                Some(e) => e,
                None => {
                    debug!(
                        "{} no event in confirmatory window ({}-{}s)",
                        symbol,
                        self.config.min_time_remaining_secs,
                        self.config.max_time_remaining_secs
                    );
                    return Ok(());
                }
            }
        } else {
            match self.event_matcher.find_event(symbol).await {
                Some(e) => e,
                None => {
                    debug!("No active event for {}", symbol);
                    return Ok(());
                }
            }
        };

        if self.config.hold_to_resolution {
            let remaining = event.time_remaining().num_seconds();
            debug!(
                "{} found event {} with {}s remaining (confirmatory mode)",
                symbol, event.title, remaining
            );
        }

        {
            let mut tracker = self.event_tracker.write().await;
            if !tracker.has_active_event(&event.condition_id) {
                tracker.start_event(
                    symbol.clone(),
                    event.condition_id.clone(),
                    event.end_time,
                    spot.price,
                );
                info!(
                    "📊 {} new event {} started at {:.2}, ends {}",
                    symbol,
                    &event.condition_id[..8],
                    spot.price,
                    event.end_time.format("%H:%M:%S")
                );
            } else {
                tracker.update_price_by_event_id(&event.condition_id, spot.price);
            }
        }

        let (up_ask, down_ask) = self.get_pm_prices(pm_cache, &event).await;

        if self.config.directional_mode {
            return self
                .directional_entry_from_binance(symbol, &spot, &event, up_ask, down_ask)
                .await;
        }

        if let Some(signal) = self.detector.check(symbol, &spot, up_ask, down_ask) {
            self.maybe_enter(signal, &event).await?;
        }

        {
            let obi = if let Some(ref lob) = self.lob_cache {
                lob.get_obi(symbol, 5).await
            } else {
                None
            };

            let tracker = self.event_tracker.read().await;
            if let Some(vol_signal) = self.volatility_detector.check_signal(
                symbol,
                &event.condition_id,
                &tracker,
                up_ask,
                down_ask,
                obi,
                event.price_to_beat,
            ) {
                let momentum_signal = MomentumSignal {
                    symbol: symbol.clone(),
                    direction: match vol_signal.side {
                        Side::Up => Direction::Up,
                        Side::Down => Direction::Down,
                    },
                    cex_move_pct: vol_signal.deviation_pct,
                    pm_price: vol_signal.entry_price,
                    edge: vol_signal.edge,
                    confidence: vol_signal.confidence,
                    timestamp: Utc::now(),
                };
                info!(
                    "📈 {} VOLATILITY signal: {} deviation={:.3}% fair={:.2}¢ edge={:.1}%",
                    symbol,
                    vol_signal.side,
                    vol_signal.deviation_pct * dec!(100),
                    vol_signal.fair_value * dec!(100),
                    vol_signal.edge * dec!(100)
                );
                self.maybe_enter(momentum_signal, &event).await?;
            }
        }

        Ok(())
    }

    async fn directional_entry_from_binance(
        &self,
        symbol: &str,
        spot: &SpotPrice,
        event: &EventInfo,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) -> Result<()> {
        let time_remaining = event.time_remaining().num_seconds() as f64;
        if time_remaining <= 0.0 {
            return Ok(());
        }

        let s0 = {
            let tracker = self.event_tracker.read().await;
            if let Some(record) = tracker.get_event(&event.condition_id) {
                record.start_price
            } else if let Some(ptb) = event.price_to_beat {
                ptb
            } else {
                return Ok(());
            }
        };

        let st = spot.price;
        let vol_floor = self.config.directional_vol_floor;
        let sigma = spot
            .volatility(300)
            .and_then(|v| v.to_f64())
            .map(|tick_vol| {
                if tick_vol > 0.0 {
                    let n_ticks = spot.history_len().min(5000) as f64;
                    (tick_vol * n_ticks.sqrt()).max(vol_floor)
                } else {
                    vol_floor
                }
            })
            .unwrap_or(vol_floor);

        let p_hat = probability::estimate_probability(s0, st, sigma, time_remaining, 0.0);
        let (direction, market_ask) = if p_hat > 0.5 {
            match up_ask {
                Some(ask) => (Direction::Up, ask),
                None => return Ok(()),
            }
        } else {
            match down_ask {
                Some(ask) => (Direction::Down, ask),
                None => return Ok(()),
            }
        };
        let effective_p = if direction == Direction::Up {
            p_hat
        } else {
            1.0 - p_hat
        };

        if market_ask > self.config.max_entry_price {
            return Ok(());
        }
        if market_ask < dec!(0.10) {
            trace!(
                "Skipping {} {} — ask {:.2}¢ below 10¢ floor",
                symbol,
                direction,
                market_ask * dec!(100)
            );
            return Ok(());
        }

        let fee_model = FeeModel::crypto();
        let effective_rate = fee_model.effective_rate(market_ask);
        let fee_per_share = market_ask * effective_rate;
        let spread_cost = dec!(0.01);
        let market_ask_f64 = market_ask.to_f64().unwrap_or(0.5);
        let cost_total_f64 =
            fee_per_share.to_f64().unwrap_or(0.01) + spread_cost.to_f64().unwrap_or(0.01);
        let ev_net = effective_p - market_ask_f64 - cost_total_f64;

        trace!(
            "🎯 {} {} p_hat={:.3} eff_p={:.3} ask={:.3} cost={:.4} ev_net={:.4} σ={:.5}",
            symbol, direction, p_hat, effective_p, market_ask_f64, cost_total_f64, ev_net, sigma
        );

        if ev_net < self.entry_threshold {
            return Ok(());
        }

        info!(
            "🎯 DIRECTIONAL ENTRY: {} {} p_hat={:.1}% ev_net={:.1}% ask={:.1}¢ σ={:.4}",
            symbol,
            direction,
            effective_p * 100.0,
            ev_net * 100.0,
            market_ask_f64 * 100.0,
            sigma,
        );

        let cex_move_pct = Decimal::try_from((st - s0) / s0).unwrap_or(Decimal::ZERO);
        let edge = Decimal::try_from(ev_net).unwrap_or(Decimal::ZERO);
        let signal = MomentumSignal {
            symbol: symbol.to_string(),
            direction,
            cex_move_pct,
            pm_price: market_ask,
            edge,
            confidence: effective_p,
            timestamp: Utc::now(),
        };

        self.maybe_enter(signal, event).await?;

        {
            let mut positions = self.positions.write().await;
            if let Some(pos) = positions
                .values_mut()
                .find(|p| p.condition_id == event.condition_id)
            {
                pos.entry_p_hat = Some(p_hat);
                pos.window_open_price = Some(s0);
            }
        }

        Ok(())
    }

    async fn get_pm_prices(
        &self,
        pm_cache: &QuoteCache,
        event: &EventInfo,
    ) -> (Option<Decimal>, Option<Decimal>) {
        let up_quote = pm_cache.get(&event.up_token_id);
        let down_quote = pm_cache.get(&event.down_token_id);

        let up_ask = up_quote.and_then(|q| q.best_ask);
        let down_ask = down_quote.and_then(|q| q.best_ask);

        (up_ask, down_ask)
    }

    async fn on_chainlink_update(
        &self,
        update: &ChainlinkUpdate,
        chainlink_cache: &ChainlinkPriceCache,
        binance_cache: &PriceCache,
        pm_cache: &QuoteCache,
    ) -> Result<()> {
        let binance_symbol =
            match crate::adapters::chainlink_rtds::to_binance_symbol(&update.symbol) {
                Some(s) => s.to_string(),
                None => return Ok(()),
            };

        if !self.config.symbols.contains(&binance_symbol) {
            return Ok(());
        }

        let event = match self
            .event_matcher
            .find_event_with_timing(
                &binance_symbol,
                self.config.min_time_remaining_secs,
                self.config.max_time_remaining_secs as i64,
                true,
            )
            .await
        {
            Some(e) => e,
            None => return Ok(()),
        };

        let time_remaining = event.time_remaining().num_seconds() as f64;
        if time_remaining <= 0.0 {
            return Ok(());
        }

        let cl_spot = match chainlink_cache.get(&update.symbol).await {
            Some(s) => s,
            None => return Ok(()),
        };

        let s0 = {
            let tracker = self.event_tracker.read().await;
            if let Some(record) = tracker.get_event(&event.condition_id) {
                record.start_price
            } else if let Some(ptb) = event.price_to_beat {
                ptb
            } else {
                return Ok(());
            }
        };

        let st = cl_spot.price;
        let sigma = cl_spot
            .volatility(300)
            .and_then(|v| v.to_f64())
            .unwrap_or(0.001);

        let p_hat = probability::estimate_probability(s0, st, sigma, time_remaining, 0.0);
        let (up_ask, down_ask) = self.get_pm_prices(pm_cache, &event).await;
        let (direction, market_ask) = if p_hat > 0.5 {
            match up_ask {
                Some(ask) => (Direction::Up, ask),
                None => return Ok(()),
            }
        } else {
            match down_ask {
                Some(ask) => (Direction::Down, ask),
                None => return Ok(()),
            }
        };
        let effective_p = if direction == Direction::Up {
            p_hat
        } else {
            1.0 - p_hat
        };

        let best_bid = if direction == Direction::Up {
            pm_cache
                .get(&event.up_token_id)
                .and_then(|q| q.best_bid)
                .unwrap_or(market_ask)
        } else {
            pm_cache
                .get(&event.down_token_id)
                .and_then(|q| q.best_bid)
                .unwrap_or(market_ask)
        };
        let depth_ratio = dec!(0.3);
        let cost = self
            .fee_model
            .all_in_cost(market_ask, best_bid, market_ask, depth_ratio);
        let cost_total_f64 = cost.total.to_f64().unwrap_or(0.02);
        let market_ask_f64 = market_ask.to_f64().unwrap_or(0.5);
        let ev_net = effective_p - market_ask_f64 - cost_total_f64;

        debug!(
            "🔮 {} {} p_hat={:.3} effective_p={:.3} ask={:.3} cost={:.4} ev_net={:.4} threshold={:.3}",
            binance_symbol,
            direction,
            p_hat,
            effective_p,
            market_ask_f64,
            cost_total_f64,
            ev_net,
            self.entry_threshold
        );

        if ev_net < self.entry_threshold {
            return Ok(());
        }

        let (_momentum_10s, _momentum_60s) =
            if let Some(spot) = binance_cache.get(&binance_symbol).await {
                (
                    spot.momentum(10).and_then(|m| m.to_f64()).unwrap_or(0.0),
                    spot.momentum(60).and_then(|m| m.to_f64()).unwrap_or(0.0),
                )
            } else {
                (0.0, 0.0)
            };

        info!(
            "🔮 DIRECTIONAL ENTRY: {} {} p_hat={:.1}% ev_net={:.1}% ask={:.1}¢ cost={:.2}% σ={:.4}",
            binance_symbol,
            direction,
            effective_p * 100.0,
            ev_net * 100.0,
            market_ask_f64 * 100.0,
            cost_total_f64 * 100.0,
            sigma,
        );

        let cex_move_pct = Decimal::try_from((st - s0) / s0).unwrap_or(Decimal::ZERO);
        let edge = Decimal::try_from(ev_net).unwrap_or(Decimal::ZERO);
        let signal = MomentumSignal {
            symbol: binance_symbol,
            direction,
            cex_move_pct,
            pm_price: market_ask,
            edge,
            confidence: effective_p,
            timestamp: Utc::now(),
        };

        self.maybe_enter(signal, &event).await?;

        {
            let mut positions = self.positions.write().await;
            if let Some(pos) = positions
                .values_mut()
                .find(|p| p.condition_id == event.condition_id)
            {
                pos.entry_p_hat = Some(p_hat);
                pos.window_open_price = Some(s0);
            }
        }

        Ok(())
    }
}
