use super::*;

impl MomentumEngine {
    /// Check for resolved positions and handle them.
    /// Returns (won_count, lost_count, total_payout)
    pub(super) async fn check_resolved_positions(&self) -> (u32, u32, Decimal) {
        let now = Utc::now();
        let mut won_count = 0u32;
        let mut lost_count = 0u32;
        let mut total_payout = Decimal::ZERO;

        let resolved_symbols: Vec<String> = {
            let positions = self.positions.read().await;
            positions
                .iter()
                .filter(|(_, pos)| pos.event_end_time < now)
                .map(|(symbol, _)| symbol.clone())
                .collect()
        };

        if resolved_symbols.is_empty() {
            return (0, 0, Decimal::ZERO);
        }

        info!(
            "🔍 Checking {} resolved positions...",
            resolved_symbols.len()
        );

        for symbol in resolved_symbols {
            let pos_opt = {
                let positions = self.positions.read().await;
                positions.get(&symbol).cloned()
            };

            let pos = match pos_opt {
                Some(p) => p,
                None => continue,
            };

            let market_result = self
                .event_matcher
                .client()
                .get_market(&pos.condition_id)
                .await;

            match market_result {
                Ok(market) => {
                    if !market.closed {
                        debug!("{} market not closed yet, waiting...", symbol);
                        continue;
                    }

                    if !self.market_is_settled(&market) {
                        debug!(
                            "{} market closed but not settled yet (outcome prices not 1/0), waiting...",
                            symbol
                        );
                        continue;
                    }

                    let won = self.check_if_won(&pos, &market);

                    if won {
                        let payout = Decimal::from(pos.shares);
                        let profit = payout - (pos.entry_price * Decimal::from(pos.shares));

                        info!(
                            "🎉 {} WON! {} {} | {} shares @ {:.2}¢ → ${:.2} payout (+${:.2} profit)",
                            symbol,
                            pos.direction,
                            pos.event_slug,
                            pos.shares,
                            pos.entry_price * dec!(100),
                            payout,
                            profit
                        );

                        won_count += 1;
                        total_payout += payout;

                        #[cfg(feature = "claimer_daemon")]
                        {
                            if let Some(ref claimer) = self.claimer {
                                info!(
                                    "📋 Triggering claimer for {}: condition_id={}, shares={}",
                                    symbol,
                                    &pos.condition_id[..16.min(pos.condition_id.len())],
                                    pos.shares
                                );
                                match claimer.check_and_claim().await {
                                    Ok(results) => {
                                        for result in results {
                                            if result.success {
                                                info!(
                                                    "✅ Claimed ${:.2} from {}: tx={}",
                                                    result.amount_claimed,
                                                    &result.condition_id
                                                        [..16.min(result.condition_id.len())],
                                                    result.tx_hash
                                                );
                                            } else if let Some(err) = result.error {
                                                warn!(
                                                    "❌ Failed to claim {}: {}",
                                                    result.condition_id, err
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Failed to trigger claimer: {}", e);
                                    }
                                }
                            } else {
                                info!(
                                    "📋 Position {} needs claiming (no claimer configured): condition_id={}, shares={}",
                                    symbol,
                                    &pos.condition_id[..16.min(pos.condition_id.len())],
                                    pos.shares
                                );
                            }
                        }

                        #[cfg(not(feature = "claimer_daemon"))]
                        {
                            info!(
                                "📋 Position {} needs claiming (claimer feature disabled): condition_id={}, shares={}",
                                symbol,
                                &pos.condition_id[..16.min(pos.condition_id.len())],
                                pos.shares
                            );
                        }
                    } else {
                        let loss = pos.entry_price * Decimal::from(pos.shares);
                        info!(
                            "❌ {} LOST: {} {} | {} shares @ {:.2}¢ → -${:.2}",
                            symbol,
                            pos.direction,
                            pos.event_slug,
                            pos.shares,
                            pos.entry_price * dec!(100),
                            loss
                        );
                        lost_count += 1;
                    }

                    if let Some(ref logger) = self.trade_logger {
                        logger.record_resolution(&pos.condition_id, won).await;
                    }

                    {
                        let mut positions = self.positions.write().await;
                        positions.remove(&symbol);
                    }

                    if let Some(ref fm) = self.fund_manager {
                        let released_notional = if pos.entry_notional > Decimal::ZERO {
                            pos.entry_notional
                        } else {
                            pos.entry_price * Decimal::from(pos.shares)
                        };
                        fm.record_position_closed_with_amount(
                            &pos.condition_id,
                            &pos.symbol,
                            released_notional,
                        )
                        .await;
                    }
                }
                Err(e) => {
                    warn!("Failed to get market status for {}: {}", symbol, e);
                }
            }
        }

        if won_count > 0 || lost_count > 0 {
            info!(
                "📊 Resolution summary: {} won, {} lost, ${:.2} payout pending claim",
                won_count, lost_count, total_payout
            );
        }

        (won_count, lost_count, total_payout)
    }

    pub(super) fn market_is_settled(&self, market: &crate::adapters::MarketResponse) -> bool {
        if !market.closed {
            return false;
        }

        let mut prices = Vec::new();
        for t in &market.tokens {
            let Some(ref price_str) = t.price else {
                continue;
            };
            if let Ok(p) = price_str.parse::<Decimal>() {
                prices.push(p);
            }
        }

        if prices.is_empty() {
            return false;
        }

        let winners = prices.iter().filter(|p| **p >= dec!(0.99)).count();
        let losers = prices.iter().filter(|p| **p <= dec!(0.01)).count();
        winners == 1 && losers == prices.len().saturating_sub(1)
    }

    /// Check if we won based on market outcome prices.
    pub(super) fn check_if_won(
        &self,
        pos: &Position,
        market: &crate::adapters::MarketResponse,
    ) -> bool {
        for token in &market.tokens {
            if token.token_id == pos.token_id {
                if let Some(ref price_str) = token.price {
                    if let Ok(price) = price_str.parse::<f64>() {
                        return price >= 0.99;
                    }
                }
            }
        }

        warn!(
            "Could not determine outcome from market data for {}, using heuristic",
            pos.symbol
        );
        false
    }

    /// Handle Polymarket quote update - check exit conditions and dump signals.
    pub(super) async fn on_pm_update(&self, update: &QuoteUpdate) -> Result<()> {
        if let Some(ref dump_hedge) = self.dump_hedge {
            if let Some(ask) = update.quote.best_ask {
                dump_hedge
                    .on_simple_price_update(&update.token_id, ask)
                    .await;
            }
        }

        if let Some(ref cl_cache) = self.chainlink_cache {
            let positions = self.positions.read().await;
            if let Some((key, pos)) = positions
                .iter()
                .find(|(_, p)| p.token_id == update.token_id)
            {
                if let (Some(entry_p), Some(s0)) = (pos.entry_p_hat, pos.window_open_price) {
                    let key = key.clone();
                    let direction = pos.direction;
                    let time_remaining = pos.time_to_resolution().num_seconds() as f64;

                    if let Some(cl_symbol) =
                        crate::adapters::chainlink_rtds::to_chainlink_symbol(&pos.symbol)
                    {
                        if let Some(cl_spot) = cl_cache.get(cl_symbol).await {
                            let sigma = cl_spot
                                .volatility(300)
                                .and_then(|v| v.to_f64())
                                .unwrap_or(0.001);
                            let current_p_hat = probability::estimate_probability(
                                s0,
                                cl_spot.price,
                                sigma,
                                time_remaining,
                                0.0,
                            );
                            let effective_p = if direction == Direction::Up {
                                current_p_hat
                            } else {
                                1.0 - current_p_hat
                            };
                            let entry_effective = if direction == Direction::Up {
                                entry_p
                            } else {
                                1.0 - entry_p
                            };

                            if effective_p < entry_effective * 0.6 {
                                if let Some(bid) = update.quote.best_bid {
                                    drop(positions);
                                    let reason = ExitReason::ProbabilityStop {
                                        entry_p_hat: entry_effective,
                                        current_p_hat: effective_p,
                                    };
                                    self.execute_exit(&key, bid, reason).await?;
                                    return Ok(());
                                }
                            }

                            if time_remaining < 30.0 {
                                let ask_f64 = update
                                    .quote
                                    .best_ask
                                    .and_then(|a| a.to_f64())
                                    .unwrap_or(0.5);
                                let cost = self
                                    .fee_model
                                    .effective_rate(update.quote.best_ask.unwrap_or(dec!(0.5)))
                                    .to_f64()
                                    .unwrap_or(0.015);
                                let ev_net = effective_p - ask_f64 - cost;
                                if ev_net < 0.0 {
                                    if let Some(bid) = update.quote.best_bid {
                                        drop(positions);
                                        self.execute_exit(&key, bid, ExitReason::TimeExit).await?;
                                        return Ok(());
                                    }
                                }
                            }

                            if let Some(bid) = update.quote.best_bid {
                                let unrealized_pnl =
                                    (bid - pos.entry_price) * Decimal::from(pos.shares);
                                if unrealized_pnl < dec!(-5) {
                                    drop(positions);
                                    let reason = ExitReason::HardStop {
                                        loss_usd: -unrealized_pnl,
                                    };
                                    self.execute_exit(&key, bid, reason).await?;
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
        }

        if self.config.hold_to_resolution && self.chainlink_cache.is_none() {
            return Ok(());
        }

        let mut positions = self.positions.write().await;
        let pos_key = positions
            .iter()
            .find(|(_, p)| p.token_id == update.token_id)
            .map(|(k, _)| k.clone());

        if let Some(key) = pos_key {
            if let Some(pos) = positions.get_mut(&key) {
                if let Some(bid) = update.quote.best_bid {
                    pos.update_high(bid);

                    if let Some(reason) = self.exit_manager.check_exit(pos, bid) {
                        drop(positions);
                        self.execute_exit(&key, bid, reason).await?;
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::polymarket_clob::{MarketResponse, TokenInfo};
    use crate::config::ExecutionConfig;

    fn test_engine() -> MomentumEngine {
        let client = PolymarketClient::new("https://clob.polymarket.com", true).unwrap();
        let executor = OrderExecutor::new(client.clone(), ExecutionConfig::default());
        MomentumEngine::new(
            MomentumConfig::default(),
            ExitConfig::default(),
            client,
            executor,
            true,
        )
    }

    fn sample_market(closed: bool, token_prices: &[(&str, &str)]) -> MarketResponse {
        MarketResponse {
            condition_id: "cond-1".to_string(),
            question_id: None,
            tokens: token_prices
                .iter()
                .map(|(token_id, price)| TokenInfo {
                    token_id: (*token_id).to_string(),
                    outcome: String::new(),
                    price: Some((*price).to_string()),
                    extra: HashMap::new(),
                })
                .collect(),
            minimum_order_size: None,
            minimum_tick_size: None,
            active: false,
            closed,
            end_date_iso: None,
            neg_risk: None,
            extra: HashMap::new(),
        }
    }

    fn sample_position(token_id: &str) -> Position {
        Position {
            token_id: token_id.to_string(),
            symbol: "BTCUSDT".into(),
            direction: Direction::Up,
            entry_price: dec!(0.42),
            entry_notional: dec!(42),
            shares: 100,
            entry_time: Utc::now(),
            highest_price: dec!(0.42),
            event_end_time: Utc::now() + ChronoDuration::minutes(5),
            event_slug: "btc-window".into(),
            condition_id: "cond-1".into(),
            entry_p_hat: None,
            window_open_price: None,
        }
    }

    #[test]
    fn test_market_is_settled_requires_closed_binary_outcomes() {
        let engine = test_engine();

        let open_market = sample_market(false, &[("up", "1.0"), ("down", "0.0")]);
        assert!(!engine.market_is_settled(&open_market));

        let ambiguous_market = sample_market(true, &[("up", "0.55"), ("down", "0.45")]);
        assert!(!engine.market_is_settled(&ambiguous_market));

        let settled_market = sample_market(true, &[("up", "1.0"), ("down", "0.0")]);
        assert!(engine.market_is_settled(&settled_market));
    }

    #[test]
    fn test_check_if_won_matches_position_token_price() {
        let engine = test_engine();
        let market = sample_market(true, &[("winning-token", "1.0"), ("losing-token", "0.0")]);

        assert!(engine.check_if_won(&sample_position("winning-token"), &market));
        assert!(!engine.check_if_won(&sample_position("losing-token"), &market));
    }
}
