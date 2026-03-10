use crate::adapters::QuoteUpdate;
use crate::domain::Side;
use crate::error::Result;
use crate::strategy::split_arb::{
    ArbSide, ArbStats, HedgedPosition, MonitoredMarket, PartialPosition, PositionStatus,
    SplitArbEngine,
};
use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

impl SplitArbEngine {
    /// Main run loop
    pub async fn run(&self, mut quote_rx: broadcast::Receiver<QuoteUpdate>) -> Result<()> {
        info!("Split Arbitrage Engine started");
        info!(
            "Config: max_entry={}¢, target_total={}¢, min_profit={}¢",
            self.config.max_entry_price * dec!(100),
            self.config.target_total_cost * dec!(100),
            self.config.min_profit_margin * dec!(100)
        );

        let mut check_interval = tokio::time::interval(std::time::Duration::from_secs(1));

        loop {
            tokio::select! {
                Ok(update) = quote_rx.recv() => {
                    self.on_quote_update(update).await;
                }
                _ = check_interval.tick() => {
                    self.check_positions().await;
                }
            }
        }
    }

    async fn on_quote_update(&self, update: QuoteUpdate) {
        {
            let mut cache = self.price_cache.write().await;
            cache.update(
                &update.token_id,
                update.quote.best_bid,
                update.quote.best_ask,
            );
        }

        self.check_opportunity(&update.token_id).await;
    }

    async fn check_opportunity(&self, token_id: &str) {
        let markets = self.monitored_markets.read().await;
        let cache = self.price_cache.read().await;

        let market = markets
            .values()
            .find(|m| m.up_token_id == token_id || m.down_token_id == token_id);

        let market = match market {
            Some(m) => m.clone(),
            None => return,
        };

        drop(markets);

        let up_price = cache.get_ask(&market.up_token_id);
        let down_price = cache.get_ask(&market.down_token_id);

        let (up_ask, down_ask) = match (up_price, down_price) {
            (Some(u), Some(d)) => (u, d),
            _ => return,
        };

        drop(cache);

        let partial_positions = self.partial_positions.read().await;
        let has_partial = partial_positions.contains_key(&market.condition_id);
        drop(partial_positions);

        if has_partial {
            self.check_hedge(&market.condition_id, up_ask, down_ask)
                .await;
        } else {
            self.check_new_entry(&market, up_ask, down_ask).await;
        }
    }

    async fn check_new_entry(&self, market: &MonitoredMarket, up_ask: Decimal, down_ask: Decimal) {
        let partial_count = self.partial_positions.read().await.len();
        if partial_count >= self.config.max_unhedged_positions {
            return;
        }

        let (side, entry_price, token_id, other_token_id) = if up_ask <= self.config.max_entry_price
        {
            (
                ArbSide::Up,
                up_ask,
                &market.up_token_id,
                &market.down_token_id,
            )
        } else if down_ask <= self.config.max_entry_price {
            (
                ArbSide::Down,
                down_ask,
                &market.down_token_id,
                &market.up_token_id,
            )
        } else {
            return;
        };

        let budget =
            (Decimal::ONE - self.config.min_profit_margin) / (Decimal::ONE + self.config.fee_rate);
        let max_hedge_price = budget - entry_price;

        let other_ask = if side == ArbSide::Up {
            down_ask
        } else {
            up_ask
        };
        if other_ask > max_hedge_price + dec!(0.10) {
            debug!(
                "Skipping {} entry at {}¢ - other side at {}¢ (max hedge: {}¢)",
                side,
                entry_price * dec!(100),
                other_ask * dec!(100),
                max_hedge_price * dec!(100)
            );
            return;
        }

        {
            let mut stats = self.stats.write().await;
            stats.signals_detected += 1;
        }

        info!(
            "🎯 ENTRY SIGNAL: {} @ {}¢ (market: {}, max hedge: {}¢)",
            side,
            entry_price * dec!(100),
            &market.condition_id[..8],
            max_hedge_price * dec!(100)
        );

        if self.dry_run {
            info!(
                "  [DRY RUN] Would buy {} shares of {}",
                self.config.shares_per_trade, side
            );
        } else {
            match self
                .execute_buy(token_id, entry_price, self.config.shares_per_trade)
                .await
            {
                Ok(_) => {
                    info!(
                        "  ✓ Order placed for {} @ {}¢",
                        side,
                        entry_price * dec!(100)
                    );
                }
                Err(e) => {
                    error!("  ✗ Order failed: {}", e);
                    return;
                }
            }
        }

        let position = PartialPosition {
            event_id: market.event_id.clone(),
            condition_id: market.condition_id.clone(),
            first_side: side,
            first_token_id: token_id.clone(),
            first_entry_price: entry_price,
            shares: self.config.shares_per_trade,
            entry_time: Utc::now(),
            event_end_time: market.event_end_time,
            other_token_id: other_token_id.clone(),
            status: PositionStatus::WaitingForHedge,
            max_hedge_price,
            confirmed: self.dry_run,
        };

        {
            let mut positions = self.partial_positions.write().await;
            positions.insert(market.condition_id.clone(), position);
        }

        {
            let mut stats = self.stats.write().await;
            stats.first_leg_entries += 1;
        }
    }

    async fn check_hedge(&self, condition_id: &str, up_ask: Decimal, down_ask: Decimal) {
        let mut positions = self.partial_positions.write().await;

        let position = match positions.get_mut(condition_id) {
            Some(p) if p.status == PositionStatus::WaitingForHedge && p.confirmed => p,
            _ => return,
        };

        let hedge_price = match position.first_side {
            ArbSide::Up => down_ask,
            ArbSide::Down => up_ask,
        };

        if hedge_price > position.max_hedge_price {
            return;
        }

        let total_cost = position.first_entry_price + hedge_price;
        let fee_cost = total_cost * self.config.fee_rate;
        let locked_profit = Decimal::ONE - total_cost - fee_cost;

        if locked_profit < self.config.min_profit_margin {
            return;
        }

        let hedge_side = match position.first_side {
            ArbSide::Up => ArbSide::Down,
            ArbSide::Down => ArbSide::Up,
        };

        info!(
            "🔒 HEDGE SIGNAL: {} @ {}¢ (total: {}¢, profit: {}¢)",
            hedge_side,
            hedge_price * dec!(100),
            total_cost * dec!(100),
            locked_profit * dec!(100)
        );

        if self.dry_run {
            info!(
                "  [DRY RUN] Would buy {} shares of {} to hedge",
                position.shares, hedge_side
            );
        } else {
            match self
                .execute_buy(&position.other_token_id, hedge_price, position.shares)
                .await
            {
                Ok(_) => {
                    info!("  ✓ Hedge order placed");
                }
                Err(e) => {
                    error!("  ✗ Hedge order failed: {}", e);
                    return;
                }
            }
        }

        let hedged = HedgedPosition {
            event_id: position.event_id.clone(),
            condition_id: condition_id.to_string(),
            up_token_id: if position.first_side == ArbSide::Up {
                position.first_token_id.clone()
            } else {
                position.other_token_id.clone()
            },
            down_token_id: if position.first_side == ArbSide::Down {
                position.first_token_id.clone()
            } else {
                position.other_token_id.clone()
            },
            up_entry_price: if position.first_side == ArbSide::Up {
                position.first_entry_price
            } else {
                hedge_price
            },
            down_entry_price: if position.first_side == ArbSide::Down {
                position.first_entry_price
            } else {
                hedge_price
            },
            total_cost,
            locked_profit,
            shares: position.shares,
            entry_time: position.entry_time,
            hedge_time: Utc::now(),
            event_end_time: position.event_end_time,
        };

        positions.remove(condition_id);
        drop(positions);

        {
            let mut hedged_positions = self.hedged_positions.write().await;
            hedged_positions.push(hedged.clone());
        }

        {
            let mut stats = self.stats.write().await;
            stats.hedges_completed += 1;
            stats.total_profit += locked_profit * Decimal::from(hedged.shares);
        }

        info!(
            "✅ POSITION HEDGED: Total cost {}¢, Locked profit {}¢/share (${:.2} total)",
            total_cost * dec!(100),
            locked_profit * dec!(100),
            locked_profit * Decimal::from(hedged.shares)
        );
    }

    async fn check_positions(&self) {
        let now = Utc::now();
        let mut to_remove = Vec::new();

        {
            let positions = self.partial_positions.read().await;

            for (condition_id, position) in positions.iter() {
                let elapsed = now - position.entry_time;
                let max_wait = Duration::seconds(self.config.max_hedge_wait_secs as i64);

                if elapsed > max_wait {
                    warn!(
                        "⏱️ Position timed out: {} {} @ {}¢ (waited {}s)",
                        position.first_side,
                        &condition_id[..8],
                        position.first_entry_price * dec!(100),
                        elapsed.num_seconds()
                    );
                    to_remove.push((condition_id.clone(), "timeout".to_string()));
                    continue;
                }

                let time_to_end = position.event_end_time - now;
                if time_to_end < Duration::seconds(30) {
                    warn!(
                        "⏰ Event ending soon, exiting unhedged: {} {}",
                        position.first_side,
                        &condition_id[..8]
                    );
                    to_remove.push((condition_id.clone(), "event_ending".to_string()));
                }
            }
        }

        for (condition_id, reason) in to_remove {
            self.exit_unhedged(&condition_id, &reason).await;
        }
    }

    async fn exit_unhedged(&self, condition_id: &str, reason: &str) {
        let mut positions = self.partial_positions.write().await;

        let position = match positions.remove(condition_id) {
            Some(p) => p,
            None => return,
        };

        drop(positions);

        let cache = self.price_cache.read().await;
        let current_bid = cache.get_bid(&position.first_token_id);
        drop(cache);

        let exit_price = current_bid.unwrap_or(position.first_entry_price);
        let pnl = exit_price - position.first_entry_price;
        let pnl_total = pnl * Decimal::from(position.shares);

        info!(
            "🚪 EXITING UNHEDGED: {} @ {}¢ → {}¢ ({}: {:.2}¢/share, ${:.2} total)",
            position.first_side,
            position.first_entry_price * dec!(100),
            exit_price * dec!(100),
            reason,
            pnl * dec!(100),
            pnl_total
        );

        if !self.dry_run {
            if let Err(e) = self
                .execute_sell(&position.first_token_id, exit_price, position.shares)
                .await
            {
                error!("  ✗ Exit order failed: {}", e);
            }
        }

        {
            let mut stats = self.stats.write().await;
            stats.unhedged_exits += 1;
            if pnl_total > Decimal::ZERO {
                stats.total_profit += pnl_total;
            } else {
                stats.total_loss += pnl_total.abs();
            }
        }
    }

    async fn execute_buy(&self, token_id: &str, price: Decimal, shares: u64) -> Result<()> {
        let order =
            crate::domain::OrderRequest::buy_limit(token_id.to_string(), Side::Up, shares, price);

        self.executor.execute(&order).await?;
        Ok(())
    }

    async fn execute_sell(&self, token_id: &str, price: Decimal, shares: u64) -> Result<()> {
        let order =
            crate::domain::OrderRequest::sell_limit(token_id.to_string(), Side::Up, shares, price);

        self.executor.execute(&order).await?;
        Ok(())
    }

    pub async fn confirm_position(&self, condition_id: &str) -> bool {
        let mut positions = self.partial_positions.write().await;
        if let Some(pos) = positions.get_mut(condition_id) {
            pos.confirmed = true;
            info!(
                "Position confirmed: {} {} @ {}c",
                pos.first_side,
                &condition_id[..8.min(condition_id.len())],
                pos.first_entry_price * dec!(100)
            );
            true
        } else {
            false
        }
    }

    pub async fn get_stats(&self) -> ArbStats {
        self.stats.read().await.clone()
    }

    pub async fn print_status(&self) {
        let stats = self.stats.read().await;
        let partial = self.partial_positions.read().await;
        let hedged = self.hedged_positions.read().await;

        info!("═══════════════════════════════════════════");
        info!("Split Arbitrage Status");
        info!("───────────────────────────────────────────");
        info!("Signals detected:    {}", stats.signals_detected);
        info!("First leg entries:   {}", stats.first_leg_entries);
        info!("Hedges completed:    {}", stats.hedges_completed);
        info!("Unhedged exits:      {}", stats.unhedged_exits);
        info!("───────────────────────────────────────────");
        info!("Active unhedged:     {}", partial.len());
        info!("Active hedged:       {}", hedged.len());
        info!("───────────────────────────────────────────");
        info!("Total profit:        ${:.2}", stats.total_profit);
        info!("Total loss:          ${:.2}", stats.total_loss);
        info!(
            "Net P&L:             ${:.2}",
            stats.total_profit - stats.total_loss
        );
        info!("═══════════════════════════════════════════");
    }
}
