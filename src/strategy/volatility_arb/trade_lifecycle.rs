use chrono::Utc;
use rust_decimal::Decimal;
use tracing::info;

use super::{
    VolArbPosition, VolArbSignal, VolArbSignalRecord, VolArbStats, VolArbTrade, VolatilityArbEngine,
};

impl VolatilityArbEngine {
    pub fn record_entry(&mut self, signal: &VolArbSignal, entry_price: Decimal, shares: u64) {
        let position = VolArbPosition {
            market_id: signal.market_id.clone(),
            condition_id: signal.condition_id.clone(),
            symbol: signal.symbol.clone(),
            is_yes: signal.buy_yes,
            shares,
            entry_price,
            entry_time: Utc::now(),
            signal: VolArbSignalRecord {
                symbol: signal.symbol.clone(),
                buy_yes: signal.buy_yes,
                fair_value: signal.fair_value,
                market_price: signal.market_price,
                price_edge: signal.price_edge,
                vol_edge_pct: signal.vol_edge_pct,
                confidence: signal.confidence,
                buffer_pct: signal.buffer_pct,
                time_remaining_secs: signal.time_remaining_secs,
            },
        };

        self.positions.insert(signal.market_id.clone(), position);
        self.last_trade_time
            .insert(signal.market_id.clone(), Utc::now());
        self.stats.total_trades += 1;
        *self
            .stats
            .trades_by_symbol
            .entry(signal.symbol.clone())
            .or_insert(0) += 1;
    }

    pub fn record_resolution(&mut self, market_id: &str, won: bool) {
        if let Some(position) = self.positions.remove(market_id) {
            let payout = if won {
                Decimal::from(position.shares)
            } else {
                Decimal::ZERO
            };
            let cost = position.entry_price * Decimal::from(position.shares);
            let fees = cost * self.config.pm_fee_rate;
            let pnl = payout - cost - fees;

            if won {
                self.stats.winning_trades += 1;
            }
            self.stats.total_pnl += pnl;
            self.stats.total_volume += cost;
            *self
                .stats
                .pnl_by_symbol
                .entry(position.symbol.clone())
                .or_insert(Decimal::ZERO) += pnl;

            if self.stats.total_trades > 0 {
                self.stats.win_rate =
                    self.stats.winning_trades as f64 / self.stats.total_trades as f64;
            }

            self.recent_trades.push(VolArbTrade {
                signal: position.signal,
                entry_price: position.entry_price,
                exit_price: Some(if won { Decimal::ONE } else { Decimal::ZERO }),
                shares: position.shares,
                pnl: Some(pnl),
                outcome: Some(won),
                entry_time: position.entry_time,
                exit_time: Some(Utc::now()),
            });

            if self.recent_trades.len() > 100 {
                self.recent_trades.remove(0);
            }

            info!(
                market_id,
                won,
                %pnl,
                total_pnl = %self.stats.total_pnl,
                win_rate = self.stats.win_rate,
                "Trade resolved"
            );
        }
    }

    pub fn stats(&self) -> &VolArbStats {
        &self.stats
    }

    pub fn recent_trades(&self) -> &[VolArbTrade] {
        &self.recent_trades
    }

    pub fn positions(&self) -> &std::collections::HashMap<String, VolArbPosition> {
        &self.positions
    }
}
