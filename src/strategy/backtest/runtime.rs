use std::collections::HashMap;

use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use tracing::debug;

use crate::strategy::volatility_arb::VolArbSignal;

use super::{
    BacktestEngine, BacktestTrade, KlineRecord, PMPriceRecord, SymbolStats,
    calculate_implied_volatility, calculate_kline_volatility,
};

impl BacktestEngine {
    pub(super) fn build_volatility_lookup(
        &self,
        klines: &[KlineRecord],
    ) -> HashMap<(String, i64), f64> {
        let mut lookup = HashMap::new();
        let mut by_symbol: HashMap<String, Vec<&KlineRecord>> = HashMap::new();

        for kline in klines {
            by_symbol
                .entry(kline.symbol.clone())
                .or_default()
                .push(kline);
        }

        for (symbol, symbol_klines) in &by_symbol {
            let mut sorted: Vec<_> = symbol_klines.iter().copied().collect();
            sorted.sort_by_key(|k| k.timestamp);

            for i in 1..sorted.len() {
                let window = &sorted[..=i];
                let vol = calculate_kline_volatility(
                    &window.iter().map(|k| (*k).clone()).collect::<Vec<_>>(),
                    self.config.vol_lookback_periods,
                );

                let bucket = (sorted[i].timestamp.timestamp() / 900) * 900;
                lookup.insert((symbol.clone(), bucket), vol);
            }
        }

        lookup
    }

    pub(super) fn group_by_market<'a>(
        &self,
        prices: &'a [PMPriceRecord],
    ) -> HashMap<String, Vec<&'a PMPriceRecord>> {
        let mut markets: HashMap<String, Vec<&PMPriceRecord>> = HashMap::new();

        for price in prices {
            markets
                .entry(price.market_id.clone())
                .or_default()
                .push(price);
        }

        for prices in markets.values_mut() {
            prices.sort_by_key(|p| p.timestamp);
        }

        markets
    }

    pub(super) fn process_market(
        &mut self,
        market_id: &str,
        prices: &[&PMPriceRecord],
        vol_lookup: &HashMap<(String, i64), f64>,
    ) {
        if prices.is_empty() {
            return;
        }

        let outcome = prices.last().and_then(|p| p.outcome);
        if outcome.is_none() {
            debug!(market_id, "Skipping market without outcome");
            return;
        }
        let outcome = outcome.unwrap();

        let mut best_signal: Option<(VolArbSignal, &PMPriceRecord)> = None;

        for price in prices {
            let time_remaining = (price.resolution_time - price.timestamp).num_seconds() as u64;

            if time_remaining < self.config.min_time_remaining_secs
                || time_remaining > self.config.max_time_remaining_secs
            {
                continue;
            }

            let bucket = (price.timestamp.timestamp() / 900) * 900;
            let kline_vol = vol_lookup
                .get(&(price.symbol.clone(), bucket))
                .copied()
                .unwrap_or(0.003);

            self.vol_engine
                .update_kline_volatility(&price.symbol, kline_vol);

            let spot_price = self.estimate_spot_from_yes_price(
                price.yes_price,
                price.threshold_price,
                kline_vol,
                time_remaining,
            );

            if let Some(signal) = self.vol_engine.analyze_market(
                &price.symbol,
                market_id,
                &price.condition_id,
                spot_price,
                price.threshold_price,
                price.yes_price,
                price.yes_ask,
                time_remaining,
                Some(kline_vol),
            ) {
                if best_signal
                    .as_ref()
                    .is_none_or(|(current, _)| signal.confidence > current.confidence)
                {
                    best_signal = Some((signal, price));
                }
            }
        }

        if let Some((signal, entry_price)) = best_signal {
            self.execute_backtest_trade(&signal, entry_price, outcome);
        }
    }

    fn estimate_spot_from_yes_price(
        &self,
        yes_price: Decimal,
        threshold: Decimal,
        volatility: f64,
        time_remaining: u64,
    ) -> Decimal {
        let yes_f64 = yes_price.to_f64().unwrap_or(0.5);
        let time_fraction = time_remaining as f64 / 900.0;

        let buffer = if yes_f64 > 0.999 {
            0.02
        } else if yes_f64 < 0.001 {
            -0.02
        } else {
            (yes_f64 - 0.5) * 2.0 * volatility * time_fraction.sqrt()
        };

        threshold * Decimal::from_f64(1.0 + buffer).unwrap_or(Decimal::ONE)
    }

    fn execute_backtest_trade(
        &mut self,
        signal: &VolArbSignal,
        entry_record: &PMPriceRecord,
        actual_outcome: bool,
    ) {
        let entry_price = signal.market_price;
        let shares = signal.position_size;

        let won = if signal.buy_yes {
            actual_outcome
        } else {
            !actual_outcome
        };

        let exit_price = if won { Decimal::ONE } else { Decimal::ZERO };
        let cost = entry_price * Decimal::from(shares);
        let revenue = exit_price * Decimal::from(shares);
        let fees = cost * self.config.pm_fee_rate;
        let pnl = revenue - cost - fees;
        let pnl_pct = if cost > Decimal::ZERO {
            pnl / cost * dec!(100)
        } else {
            Decimal::ZERO
        };

        self.current_equity += pnl;
        if self.current_equity > self.peak_equity {
            self.peak_equity = self.current_equity;
        }

        let trade = BacktestTrade {
            entry_time: signal.timestamp,
            exit_time: entry_record.resolution_time,
            symbol: signal.symbol.clone(),
            market_id: signal.market_id.clone(),
            direction: if signal.buy_yes {
                "YES".into()
            } else {
                "NO".into()
            },
            entry_price,
            exit_price,
            shares,
            pnl,
            pnl_pct,
            won,
            fair_value: signal.fair_value,
            price_edge: signal.price_edge,
            vol_edge_pct: signal.vol_edge_pct,
            confidence: signal.confidence,
            buffer_pct: signal.buffer_pct,
            our_volatility: self
                .vol_engine
                .estimate_volatility(&signal.symbol, None)
                .combined_vol,
            implied_volatility: calculate_implied_volatility(
                entry_price.to_f64().unwrap_or(0.5),
                signal.buffer_pct.to_f64().unwrap_or(0.0),
                signal.time_remaining_secs as f64 / 900.0,
            )
            .unwrap_or(0.003),
        };

        self.results.total_trades += 1;
        self.results.total_volume += cost;
        self.results.total_pnl += pnl;

        if won {
            self.results.winning_trades += 1;
        } else {
            self.results.losing_trades += 1;
        }

        let symbol_stats = self
            .results
            .trades_by_symbol
            .entry(signal.symbol.clone())
            .or_insert(SymbolStats {
                total_trades: 0,
                winning_trades: 0,
                win_rate: 0.0,
                total_pnl: Decimal::ZERO,
            });
        symbol_stats.total_trades += 1;
        if won {
            symbol_stats.winning_trades += 1;
        }
        symbol_stats.total_pnl += pnl;

        self.results
            .equity_curve
            .push((entry_record.resolution_time, self.current_equity));

        self.results.trades.push(trade);
    }
}
