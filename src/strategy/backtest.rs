//! Compatibility layer for legacy backtest helpers.
//!
//! Shared backtest infrastructure now lives in the `ploy-backtest` crate. This
//! module preserves the legacy volatility-arb helpers that still depend on
//! app-local `volatility_arb` logic, while re-exporting the shared data types.

use chrono::Utc;
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write as IoWrite;
use std::path::Path;
use tracing::{debug, info};

use crate::strategy::volatility_arb::{
    calculate_implied_volatility, VolArbSignal, VolatilityArbConfig, VolatilityArbEngine,
};

pub use ploy_backtest::engine::{
    calculate_kline_volatility, load_klines_from_csv, load_pm_prices_from_csv, BacktestResults,
    BacktestTrade, KlineRecord, MarketSnapshot, PMPriceRecord, PaperSignal, PaperTradingStats,
    SymbolStats,
};

/// Legacy volatility-arb CSV backtest runner.
///
/// This remains in the main application because it depends directly on the
/// volatility-arb engine, not the new generic `BacktestStrategy` interface.
pub struct BacktestEngine {
    config: VolatilityArbConfig,
    vol_engine: VolatilityArbEngine,
    results: BacktestResults,
    current_equity: Decimal,
    peak_equity: Decimal,
    initial_capital: Decimal,
}

impl BacktestEngine {
    pub fn new(config: VolatilityArbConfig, initial_capital: Decimal) -> Self {
        Self {
            vol_engine: VolatilityArbEngine::new(config.clone()),
            config,
            results: BacktestResults::default(),
            current_equity: initial_capital,
            peak_equity: initial_capital,
            initial_capital,
        }
    }

    pub fn run(&mut self, klines: &[KlineRecord], pm_prices: &[PMPriceRecord]) -> BacktestResults {
        info!(
            "Starting legacy vol-arb backtest with {} klines and {} PM prices",
            klines.len(),
            pm_prices.len()
        );

        let vol_lookup = self.build_volatility_lookup(klines);
        let markets = self.group_by_market(pm_prices);

        if let Some(first) = pm_prices.first() {
            self.results.start_time = first.timestamp;
        }
        if let Some(last) = pm_prices.last() {
            self.results.end_time = last.timestamp;
        }

        for (market_id, prices) in markets {
            self.process_market(&market_id, &prices, &vol_lookup);
        }

        self.calculate_statistics();
        self.results.clone()
    }

    fn build_volatility_lookup(&self, klines: &[KlineRecord]) -> HashMap<(String, i64), f64> {
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

    fn group_by_market<'a>(
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

    fn process_market(
        &mut self,
        market_id: &str,
        prices: &[&PMPriceRecord],
        vol_lookup: &HashMap<(String, i64), f64>,
    ) {
        if prices.is_empty() {
            return;
        }

        let Some(outcome) = prices.last().and_then(|p| p.outcome) else {
            debug!(market_id, "Skipping market without outcome");
            return;
        };

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
                    .map_or(true, |(current, _)| signal.confidence > current.confidence)
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
            pnl / cost * rust_decimal_macros::dec!(100)
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

    fn calculate_statistics(&mut self) {
        let trades = &self.results.trades;
        if trades.is_empty() {
            return;
        }

        self.results.win_rate =
            self.results.winning_trades as f64 / self.results.total_trades as f64;
        self.results.avg_pnl_per_trade =
            self.results.total_pnl / Decimal::from(self.results.total_trades);

        let wins: Vec<_> = trades.iter().filter(|t| t.won).collect();
        let losses: Vec<_> = trades.iter().filter(|t| !t.won).collect();

        if !wins.is_empty() {
            self.results.avg_win =
                wins.iter().map(|t| t.pnl).sum::<Decimal>() / Decimal::from(wins.len() as u64);
            self.results.largest_win = wins.iter().map(|t| t.pnl).max().unwrap_or(Decimal::ZERO);
        }

        if !losses.is_empty() {
            self.results.avg_loss =
                losses.iter().map(|t| t.pnl).sum::<Decimal>() / Decimal::from(losses.len() as u64);
            self.results.largest_loss = losses.iter().map(|t| t.pnl).min().unwrap_or(Decimal::ZERO);
        }

        let mut peak = self.initial_capital;
        let mut max_dd = Decimal::ZERO;
        for (_, equity) in &self.results.equity_curve {
            if *equity > peak {
                peak = *equity;
            }
            let dd = (peak - equity) / peak;
            if dd > max_dd {
                max_dd = dd;
            }
        }
        self.results.max_drawdown = max_dd;

        let total_wins: Decimal = wins.iter().map(|t| t.pnl).sum();
        let total_losses: Decimal = losses.iter().map(|t| t.pnl.abs()).sum();
        if total_losses > Decimal::ZERO {
            self.results.profit_factor = (total_wins / total_losses).to_f64().unwrap_or(0.0);
        }

        let returns: Vec<f64> = trades.iter().filter_map(|t| t.pnl_pct.to_f64()).collect();
        if returns.len() > 1 {
            let mean = returns.iter().sum::<f64>() / returns.len() as f64;
            let variance =
                returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
            let std_dev = variance.sqrt();
            if std_dev > 0.0 {
                self.results.sharpe_ratio = mean / std_dev * (100.0_f64).sqrt();
            }
        }

        let total_hold_time: i64 = trades
            .iter()
            .map(|t| (t.exit_time - t.entry_time).num_seconds())
            .sum();
        self.results.avg_holding_time_secs = total_hold_time as f64 / trades.len() as f64;

        for stats in self.results.trades_by_symbol.values_mut() {
            if stats.total_trades > 0 {
                stats.win_rate = stats.winning_trades as f64 / stats.total_trades as f64;
            }
        }
    }
}

/// Legacy paper-trading logger for the volatility-arb workflow.
pub struct PaperTrader {
    config: VolatilityArbConfig,
    vol_engine: VolatilityArbEngine,
    signals: Vec<PaperSignal>,
    pending_signals: HashMap<String, PaperSignal>,
    log_file: Option<String>,
}

impl PaperTrader {
    pub fn new(config: VolatilityArbConfig, log_file: Option<String>) -> Self {
        Self {
            vol_engine: VolatilityArbEngine::new(config.clone()),
            config,
            signals: Vec::new(),
            pending_signals: HashMap::new(),
            log_file,
        }
    }

    pub fn update_volatility(&mut self, symbol: &str, kline_vol: f64) {
        self.vol_engine.update_kline_volatility(symbol, kline_vol);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn check_and_record(
        &mut self,
        symbol: &str,
        market_id: &str,
        condition_id: &str,
        spot_price: Decimal,
        threshold_price: Decimal,
        yes_price: Decimal,
        yes_ask: Decimal,
        time_remaining_secs: u64,
        tick_volatility: Option<f64>,
    ) -> Option<PaperSignal> {
        if self.pending_signals.contains_key(market_id) {
            return None;
        }

        let signal = self.vol_engine.analyze_market(
            symbol,
            market_id,
            condition_id,
            spot_price,
            threshold_price,
            yes_price,
            yes_ask,
            time_remaining_secs,
            tick_volatility,
        )?;

        let vol_estimate = self.vol_engine.estimate_volatility(symbol, tick_volatility);
        let implied_vol = calculate_implied_volatility(
            yes_price.to_f64().unwrap_or(0.5),
            signal.buffer_pct.to_f64().unwrap_or(0.0),
            time_remaining_secs as f64 / 900.0,
        )
        .unwrap_or(0.003);

        let paper_signal = PaperSignal {
            timestamp: Utc::now(),
            symbol: symbol.to_string(),
            market_id: market_id.to_string(),
            condition_id: condition_id.to_string(),
            direction: if signal.buy_yes {
                "YES".into()
            } else {
                "NO".into()
            },
            entry_price: signal.market_price,
            fair_value: signal.fair_value,
            price_edge: signal.price_edge,
            vol_edge_pct: signal.vol_edge_pct,
            confidence: signal.confidence,
            recommended_shares: signal.position_size,
            buffer_pct: signal.buffer_pct,
            our_volatility: vol_estimate.combined_vol,
            implied_volatility: implied_vol,
            time_remaining_secs,
            resolution_time: None,
            actual_outcome: None,
            would_have_won: None,
            theoretical_pnl: None,
        };

        self.log_signal(&paper_signal);
        self.pending_signals
            .insert(market_id.to_string(), paper_signal.clone());

        info!(
            symbol,
            direction = paper_signal.direction,
            entry_price = %paper_signal.entry_price,
            fair_value = %paper_signal.fair_value,
            price_edge = %paper_signal.price_edge,
            vol_edge_pct = paper_signal.vol_edge_pct,
            confidence = paper_signal.confidence,
            "Paper signal recorded"
        );

        Some(paper_signal)
    }

    pub fn record_resolution(&mut self, market_id: &str, outcome: bool) {
        if let Some(mut signal) = self.pending_signals.remove(market_id) {
            signal.resolution_time = Some(Utc::now());
            signal.actual_outcome = Some(outcome);

            let would_have_won = if signal.direction == "YES" {
                outcome
            } else {
                !outcome
            };
            signal.would_have_won = Some(would_have_won);

            let entry_price = signal.entry_price;
            let shares = signal.recommended_shares;
            let exit_price = if would_have_won {
                Decimal::ONE
            } else {
                Decimal::ZERO
            };
            let cost = entry_price * Decimal::from(shares);
            let revenue = exit_price * Decimal::from(shares);
            let fees = cost * self.config.pm_fee_rate;
            signal.theoretical_pnl = Some(revenue - cost - fees);

            self.log_resolution(&signal);
            self.signals.push(signal);
        }
    }

    pub fn statistics(&self) -> PaperTradingStats {
        let resolved: Vec<_> = self
            .signals
            .iter()
            .filter(|signal| signal.would_have_won.is_some())
            .collect();

        if resolved.is_empty() {
            return PaperTradingStats::default();
        }

        let total = resolved.len() as u64;
        let wins = resolved
            .iter()
            .filter(|signal| signal.would_have_won == Some(true))
            .count() as u64;
        let total_pnl: Decimal = resolved
            .iter()
            .filter_map(|signal| signal.theoretical_pnl)
            .sum();

        PaperTradingStats {
            total_signals: total,
            winning_signals: wins,
            win_rate: wins as f64 / total as f64,
            theoretical_pnl: total_pnl,
            avg_vol_edge: resolved
                .iter()
                .map(|signal| signal.vol_edge_pct)
                .sum::<f64>()
                / resolved.len() as f64,
            avg_confidence: resolved.iter().map(|signal| signal.confidence).sum::<f64>()
                / resolved.len() as f64,
            pending_signals: self.pending_signals.len() as u64,
        }
    }

    pub fn signals(&self) -> &[PaperSignal] {
        &self.signals
    }

    pub fn pending(&self) -> &HashMap<String, PaperSignal> {
        &self.pending_signals
    }

    fn log_signal(&self, signal: &PaperSignal) {
        if let Some(ref path) = self.log_file {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let json = serde_json::to_string(signal).unwrap_or_default();
                let _ = writeln!(file, "{}", json);
            }
        }
    }

    fn log_resolution(&self, signal: &PaperSignal) {
        if let Some(ref path) = self.log_file {
            let resolution_path = path.replace(".json", "_resolved.json");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(resolution_path)
            {
                let json = serde_json::to_string(signal).unwrap_or_default();
                let _ = writeln!(file, "{}", json);
            }
        }
    }

    pub fn export_csv<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let mut file = File::create(path).map_err(|e| e.to_string())?;
        writeln!(file, "timestamp,symbol,market_id,direction,entry_price,fair_value,price_edge,vol_edge_pct,confidence,our_vol,implied_vol,buffer_pct,time_remaining,outcome,won,pnl")
            .map_err(|e| e.to_string())?;

        for signal in &self.signals {
            writeln!(
                file,
                "{},{},{},{},{},{},{},{:.4},{:.4},{:.6},{:.6},{},{},{},{},{}",
                signal.timestamp.format("%Y-%m-%d %H:%M:%S"),
                signal.symbol,
                signal.market_id,
                signal.direction,
                signal.entry_price,
                signal.fair_value,
                signal.price_edge,
                signal.vol_edge_pct,
                signal.confidence,
                signal.our_volatility,
                signal.implied_volatility,
                signal.buffer_pct,
                signal.time_remaining_secs,
                signal
                    .actual_outcome
                    .map_or("pending".to_string(), |outcome| {
                        if outcome {
                            "YES".to_string()
                        } else {
                            "NO".to_string()
                        }
                    }),
                signal.would_have_won.map_or("pending".to_string(), |won| {
                    if won {
                        "WIN".to_string()
                    } else {
                        "LOSS".to_string()
                    }
                }),
                signal
                    .theoretical_pnl
                    .map_or("0".to_string(), |pnl| pnl.to_string()),
            )
            .map_err(|e| e.to_string())?;
        }

        Ok(())
    }
}
