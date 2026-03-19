use super::{
    stats_rebuild, SymbolStats, TradeContext, TradeLogger, TradeOutcome, TradeRecord, TradingStats,
};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, error, info, warn};

impl TradeLogger {
    /// Load existing trades from file.
    pub async fn load(&self) -> crate::error::Result<()> {
        if !self.log_path.exists() {
            debug!("No existing trades file, starting fresh");
            return Ok(());
        }

        let content = tokio::fs::read_to_string(&self.log_path).await?;
        let trades: Vec<TradeRecord> = serde_json::from_str(&content)?;

        info!("Loaded {} historical trades", trades.len());

        {
            let mut cache = self.trades.write().await;
            *cache = trades;
        }

        self.recalculate_stats().await;
        Ok(())
    }

    /// Save trades to file.
    pub async fn save(&self) -> crate::error::Result<()> {
        if let Some(parent) = self.log_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let trades = self.trades.read().await;
        let content = serde_json::to_string_pretty(&*trades)?;
        tokio::fs::write(&self.log_path, content).await?;

        debug!("Saved {} trades to {:?}", trades.len(), self.log_path);
        Ok(())
    }

    /// Record a new trade entry (simple version).
    pub async fn record_entry(
        &self,
        symbol: &str,
        event_slug: &str,
        condition_id: &str,
        direction: &str,
        entry_price: Decimal,
        shares: u64,
        momentum_pct: Decimal,
        edge_pct: Decimal,
    ) -> String {
        self.record_entry_with_context(
            symbol,
            event_slug,
            condition_id,
            direction,
            entry_price,
            shares,
            momentum_pct,
            edge_pct,
            TradeContext::default(),
        )
        .await
    }

    /// Record a new trade entry with full market context.
    pub async fn record_entry_with_context(
        &self,
        symbol: &str,
        event_slug: &str,
        condition_id: &str,
        direction: &str,
        entry_price: Decimal,
        shares: u64,
        momentum_pct: Decimal,
        edge_pct: Decimal,
        context: TradeContext,
    ) -> String {
        let id = format!("{}_{}", condition_id, Utc::now().timestamp_millis());
        let cost_usd = entry_price * Decimal::from(shares);

        let record = TradeRecord {
            id: id.clone(),
            timestamp: Utc::now(),
            symbol: symbol.to_string(),
            event_slug: event_slug.to_string(),
            condition_id: condition_id.to_string(),
            direction: direction.to_string(),
            entry_price,
            shares,
            cost_usd,
            momentum_pct,
            edge_pct,
            outcome: TradeOutcome::Open,
            payout_usd: None,
            pnl_usd: None,
            resolved_at: None,
            context,
        };

        info!(
            "📝 Trade logged: {} {} {} @ {:.2}¢ | {} shares = ${:.2}",
            symbol,
            direction,
            event_slug,
            entry_price * dec!(100),
            shares,
            cost_usd
        );

        {
            let mut trades = self.trades.write().await;
            trades.push(record);
        }

        {
            let mut stats = self.stats.write().await;
            stats.total_trades += 1;
            stats.open += 1;
            stats.total_cost += cost_usd;

            let symbol_stats = stats
                .by_symbol
                .entry(symbol.to_string())
                .or_insert_with(|| SymbolStats {
                    symbol: symbol.to_string(),
                    ..Default::default()
                });
            symbol_stats.total_trades += 1;
            symbol_stats.open += 1;
            symbol_stats.total_cost += cost_usd;
            symbol_stats.last_trade = Some(Utc::now());
        }

        if let Err(e) = self.save().await {
            error!("Failed to save trades: {}", e);
        }

        id
    }

    /// Record trade resolution (win/loss).
    pub async fn record_resolution(&self, condition_id: &str, won: bool) {
        let mut trades = self.trades.write().await;

        if let Some(trade) = trades
            .iter_mut()
            .find(|t| t.condition_id == condition_id && t.outcome == TradeOutcome::Open)
        {
            let payout = if won {
                Decimal::from(trade.shares)
            } else {
                Decimal::ZERO
            };
            let pnl = payout - trade.cost_usd;

            trade.outcome = if won {
                TradeOutcome::Won
            } else {
                TradeOutcome::Lost
            };
            trade.payout_usd = Some(payout);
            trade.pnl_usd = Some(pnl);
            trade.resolved_at = Some(Utc::now());

            let symbol = trade.symbol.clone();

            info!(
                "📊 Trade resolved: {} {} {} | {} | PnL: ${:.2}",
                symbol,
                trade.direction,
                if won { "WON" } else { "LOST" },
                trade.event_slug,
                pnl
            );

            drop(trades);

            {
                let mut stats = self.stats.write().await;
                stats.open = stats.open.saturating_sub(1);
                stats.total_payout += payout;
                stats.total_pnl += pnl;

                if won {
                    stats.wins += 1;
                } else {
                    stats.losses += 1;
                }

                if let Some(symbol_stats) = stats.by_symbol.get_mut(&symbol) {
                    symbol_stats.open = symbol_stats.open.saturating_sub(1);
                    symbol_stats.total_payout += payout;
                    symbol_stats.total_pnl += pnl;
                    if won {
                        symbol_stats.wins += 1;
                    } else {
                        symbol_stats.losses += 1;
                    }
                }
            }

            if let Err(e) = self.save().await {
                error!("Failed to save trades: {}", e);
            }
        } else {
            warn!("Trade not found for condition_id: {}", condition_id);
        }
    }

    /// Recalculate statistics from trades.
    async fn recalculate_stats(&self) {
        let trades = self.trades.read().await;
        let stats: TradingStats = stats_rebuild::rebuild_stats(&trades);
        let mut cached_stats = self.stats.write().await;
        *cached_stats = stats;
    }
}
