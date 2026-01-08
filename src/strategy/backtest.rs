//! Backtest and Paper Trading Framework
//!
//! This module provides:
//! 1. Historical data loading from CSV/JSON files
//! 2. Backtest engine for strategy validation
//! 3. Paper trading mode for live signal recording without execution
//!
//! ## Usage
//!
//! ```bash
//! # Backtest volatility arbitrage strategy
//! ploy backtest vol-arb --data ./data/btc_2024.csv --start 2024-01-01 --end 2024-12-31
//!
//! # Paper trading mode
//! ploy paper-trade vol-arb --symbols BTC,ETH,SOL
//! ```

use chrono::{DateTime, Duration, NaiveDateTime, Utc, TimeZone};
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write as IoWrite};
use std::path::Path;
use tracing::{debug, info, warn, error};

use crate::strategy::volatility_arb::{
    VolatilityArbConfig, VolatilityArbEngine, VolArbSignal,
    calculate_fair_yes_price, calculate_implied_volatility,
};

// ============================================================================
// Historical Data Structures
// ============================================================================

/// Historical K-line (candlestick) data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KlineRecord {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
}

/// Historical Polymarket price snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PMPriceRecord {
    pub timestamp: DateTime<Utc>,
    pub market_id: String,
    pub condition_id: String,
    pub symbol: String,
    pub threshold_price: Decimal,
    pub yes_price: Decimal,
    pub no_price: Decimal,
    pub yes_bid: Decimal,
    pub yes_ask: Decimal,
    pub resolution_time: DateTime<Utc>,
    pub outcome: Option<bool>, // true = YES won, false = NO won
}

/// Combined snapshot for backtesting
#[derive(Debug, Clone)]
pub struct MarketSnapshot {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub spot_price: Decimal,
    pub threshold_price: Decimal,
    pub yes_price: Decimal,
    pub yes_ask: Decimal,
    pub time_remaining_secs: u64,
    pub resolution_time: DateTime<Utc>,
    pub market_id: String,
    pub condition_id: String,
    pub kline_volatility: f64,
    pub tick_volatility: Option<f64>,
    pub outcome: Option<bool>,
}

// ============================================================================
// Backtest Results
// ============================================================================

/// Individual backtest trade result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestTrade {
    pub entry_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub symbol: String,
    pub market_id: String,
    pub direction: String, // "YES" or "NO"
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub shares: u64,
    pub pnl: Decimal,
    pub pnl_pct: Decimal,
    pub won: bool,
    // Signal details
    pub fair_value: Decimal,
    pub price_edge: Decimal,
    pub vol_edge_pct: f64,
    pub confidence: f64,
    pub buffer_pct: Decimal,
    pub our_volatility: f64,
    pub implied_volatility: f64,
}

/// Backtest summary statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResults {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub total_trades: u64,
    pub winning_trades: u64,
    pub losing_trades: u64,
    pub win_rate: f64,
    pub total_pnl: Decimal,
    pub total_volume: Decimal,
    pub avg_pnl_per_trade: Decimal,
    pub max_drawdown: Decimal,
    pub sharpe_ratio: f64,
    pub profit_factor: f64,
    pub avg_win: Decimal,
    pub avg_loss: Decimal,
    pub largest_win: Decimal,
    pub largest_loss: Decimal,
    pub avg_holding_time_secs: f64,
    pub trades_by_symbol: HashMap<String, SymbolStats>,
    pub trades: Vec<BacktestTrade>,
    pub equity_curve: Vec<(DateTime<Utc>, Decimal)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolStats {
    pub total_trades: u64,
    pub winning_trades: u64,
    pub win_rate: f64,
    pub total_pnl: Decimal,
}

impl Default for BacktestResults {
    fn default() -> Self {
        Self {
            start_time: Utc::now(),
            end_time: Utc::now(),
            total_trades: 0,
            winning_trades: 0,
            losing_trades: 0,
            win_rate: 0.0,
            total_pnl: Decimal::ZERO,
            total_volume: Decimal::ZERO,
            avg_pnl_per_trade: Decimal::ZERO,
            max_drawdown: Decimal::ZERO,
            sharpe_ratio: 0.0,
            profit_factor: 0.0,
            avg_win: Decimal::ZERO,
            avg_loss: Decimal::ZERO,
            largest_win: Decimal::ZERO,
            largest_loss: Decimal::ZERO,
            avg_holding_time_secs: 0.0,
            trades_by_symbol: HashMap::new(),
            trades: Vec::new(),
            equity_curve: Vec::new(),
        }
    }
}

// ============================================================================
// Data Loading
// ============================================================================

/// Load K-line data from CSV file
/// Expected format: timestamp,symbol,open,high,low,close,volume
pub fn load_klines_from_csv<P: AsRef<Path>>(path: P) -> Result<Vec<KlineRecord>, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for (i, line) in reader.lines().enumerate() {
        if i == 0 {
            continue; // Skip header
        }

        let line = line.map_err(|e| format!("Failed to read line {}: {}", i, e))?;
        let parts: Vec<&str> = line.split(',').collect();

        if parts.len() < 7 {
            warn!("Skipping malformed line {}: insufficient columns", i);
            continue;
        }

        let timestamp = parse_timestamp(parts[0])
            .ok_or_else(|| format!("Invalid timestamp at line {}", i))?;

        let record = KlineRecord {
            timestamp,
            symbol: parts[1].to_string(),
            open: Decimal::from_str(parts[2]).unwrap_or(Decimal::ZERO),
            high: Decimal::from_str(parts[3]).unwrap_or(Decimal::ZERO),
            low: Decimal::from_str(parts[4]).unwrap_or(Decimal::ZERO),
            close: Decimal::from_str(parts[5]).unwrap_or(Decimal::ZERO),
            volume: Decimal::from_str(parts[6]).unwrap_or(Decimal::ZERO),
        };

        records.push(record);
    }

    info!("Loaded {} K-line records", records.len());
    Ok(records)
}

/// Load PM price data from CSV file
/// Expected format: timestamp,market_id,condition_id,symbol,threshold,yes_price,no_price,yes_bid,yes_ask,resolution_time,outcome
pub fn load_pm_prices_from_csv<P: AsRef<Path>>(path: P) -> Result<Vec<PMPriceRecord>, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for (i, line) in reader.lines().enumerate() {
        if i == 0 {
            continue; // Skip header
        }

        let line = line.map_err(|e| format!("Failed to read line {}: {}", i, e))?;
        let parts: Vec<&str> = line.split(',').collect();

        if parts.len() < 11 {
            warn!("Skipping malformed line {}: insufficient columns", i);
            continue;
        }

        let timestamp = parse_timestamp(parts[0])
            .ok_or_else(|| format!("Invalid timestamp at line {}", i))?;
        let resolution_time = parse_timestamp(parts[9])
            .ok_or_else(|| format!("Invalid resolution_time at line {}", i))?;

        let outcome = match parts[10].trim().to_lowercase().as_str() {
            "yes" | "true" | "1" => Some(true),
            "no" | "false" | "0" => Some(false),
            _ => None,
        };

        let record = PMPriceRecord {
            timestamp,
            market_id: parts[1].to_string(),
            condition_id: parts[2].to_string(),
            symbol: parts[3].to_string(),
            threshold_price: Decimal::from_str(parts[4]).unwrap_or(Decimal::ZERO),
            yes_price: Decimal::from_str(parts[5]).unwrap_or(dec!(0.5)),
            no_price: Decimal::from_str(parts[6]).unwrap_or(dec!(0.5)),
            yes_bid: Decimal::from_str(parts[7]).unwrap_or(dec!(0.5)),
            yes_ask: Decimal::from_str(parts[8]).unwrap_or(dec!(0.5)),
            resolution_time,
            outcome,
        };

        records.push(record);
    }

    info!("Loaded {} PM price records", records.len());
    Ok(records)
}

fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    // Try various formats
    if let Ok(ts) = s.parse::<i64>() {
        // Unix timestamp (seconds or milliseconds)
        if ts > 1_000_000_000_000 {
            return Utc.timestamp_millis_opt(ts).single();
        } else {
            return Utc.timestamp_opt(ts, 0).single();
        }
    }

    // Try ISO 8601 format
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }

    // Try common datetime formats
    let formats = [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
    ];

    for fmt in &formats {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(Utc.from_utc_datetime(&dt));
        }
    }

    None
}

// ============================================================================
// Volatility Calculation
// ============================================================================

/// Calculate historical volatility from K-lines
/// Returns 15-minute volatility as percentage (e.g., 0.003 = 0.3%)
pub fn calculate_kline_volatility(klines: &[KlineRecord], lookback: usize) -> f64 {
    if klines.len() < 2 {
        return 0.003; // Default 0.3%
    }

    let n = klines.len().min(lookback);
    let recent = &klines[klines.len() - n..];

    // Calculate log returns
    let returns: Vec<f64> = recent.windows(2)
        .filter_map(|w| {
            let prev = w[0].close.to_f64()?;
            let curr = w[1].close.to_f64()?;
            if prev > 0.0 {
                Some((curr / prev).ln())
            } else {
                None
            }
        })
        .collect();

    if returns.is_empty() {
        return 0.003;
    }

    // Calculate standard deviation
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter()
        .map(|r| (r - mean).powi(2))
        .sum::<f64>() / returns.len() as f64;

    variance.sqrt().max(0.0001)
}

// ============================================================================
// Backtest Engine
// ============================================================================

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

    /// Run backtest on historical data
    pub fn run(
        &mut self,
        klines: &[KlineRecord],
        pm_prices: &[PMPriceRecord],
    ) -> BacktestResults {
        info!("Starting backtest with {} klines and {} PM prices",
              klines.len(), pm_prices.len());

        // Build volatility lookup by symbol and time
        let vol_lookup = self.build_volatility_lookup(klines);

        // Group PM prices by market
        let markets = self.group_by_market(pm_prices);

        // Track start/end times
        if let Some(first) = pm_prices.first() {
            self.results.start_time = first.timestamp;
        }
        if let Some(last) = pm_prices.last() {
            self.results.end_time = last.timestamp;
        }

        // Process each market
        for (market_id, prices) in markets {
            self.process_market(&market_id, &prices, &vol_lookup);
        }

        // Calculate final statistics
        self.calculate_statistics();

        self.results.clone()
    }

    fn build_volatility_lookup(&self, klines: &[KlineRecord]) -> HashMap<(String, i64), f64> {
        let mut lookup = HashMap::new();
        let mut by_symbol: HashMap<String, Vec<&KlineRecord>> = HashMap::new();

        // Group klines by symbol
        for kline in klines {
            by_symbol.entry(kline.symbol.clone())
                .or_default()
                .push(kline);
        }

        // Calculate rolling volatility for each symbol
        for (symbol, symbol_klines) in &by_symbol {
            let mut sorted: Vec<_> = symbol_klines.iter().copied().collect();
            sorted.sort_by_key(|k| k.timestamp);

            for i in 1..sorted.len() {
                let window = &sorted[..=i];
                let vol = calculate_kline_volatility(
                    &window.iter().map(|k| (*k).clone()).collect::<Vec<_>>(),
                    self.config.vol_lookback_periods,
                );

                // Round timestamp to 15-minute bucket
                let bucket = (sorted[i].timestamp.timestamp() / 900) * 900;
                lookup.insert((symbol.clone(), bucket), vol);
            }
        }

        lookup
    }

    fn group_by_market<'a>(&self, prices: &'a [PMPriceRecord]) -> HashMap<String, Vec<&'a PMPriceRecord>> {
        let mut markets: HashMap<String, Vec<&PMPriceRecord>> = HashMap::new();

        for price in prices {
            markets.entry(price.market_id.clone())
                .or_default()
                .push(price);
        }

        // Sort each market's prices by timestamp
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

        let outcome = prices.last().and_then(|p| p.outcome);
        if outcome.is_none() {
            debug!(market_id, "Skipping market without outcome");
            return;
        }
        let outcome = outcome.unwrap();

        // Find best entry point
        let mut best_signal: Option<(VolArbSignal, &PMPriceRecord)> = None;

        for price in prices {
            let time_remaining = (price.resolution_time - price.timestamp).num_seconds() as u64;

            // Skip if outside time window
            if time_remaining < self.config.min_time_remaining_secs ||
               time_remaining > self.config.max_time_remaining_secs {
                continue;
            }

            // Get volatility estimate
            let bucket = (price.timestamp.timestamp() / 900) * 900;
            let kline_vol = vol_lookup.get(&(price.symbol.clone(), bucket))
                .copied()
                .unwrap_or(0.003);

            // Update engine volatility
            self.vol_engine.update_kline_volatility(&price.symbol, kline_vol);

            // Get spot price (use threshold + buffer implied by YES price as proxy)
            // In real backtest, we'd have actual spot prices
            let spot_price = self.estimate_spot_from_yes_price(
                price.yes_price,
                price.threshold_price,
                kline_vol,
                time_remaining,
            );

            // Analyze market
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
                // Keep best signal (highest confidence)
                if best_signal.as_ref().map_or(true, |(s, _)| signal.confidence > s.confidence) {
                    best_signal = Some((signal, price));
                }
            }
        }

        // Execute trade if signal found
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
        // Reverse the pricing formula to estimate spot price
        // P(YES) = N(buffer / (σ√T))
        // buffer = spot/threshold - 1

        let yes_f64 = yes_price.to_f64().unwrap_or(0.5);
        let time_fraction = time_remaining as f64 / 900.0;

        // Approximate inverse of N()
        // For YES close to 0.5, buffer ≈ 0
        // For YES > 0.5, buffer > 0
        let buffer = if yes_f64 > 0.999 {
            0.02 // Cap at 2%
        } else if yes_f64 < 0.001 {
            -0.02
        } else {
            // Simple approximation: buffer ≈ (YES - 0.5) * 2 * σ√T
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

        // Determine if we won
        let won = if signal.buy_yes {
            actual_outcome // Bought YES, win if outcome is YES
        } else {
            !actual_outcome // Bought NO, win if outcome is NO
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

        // Update equity
        self.current_equity += pnl;
        if self.current_equity > self.peak_equity {
            self.peak_equity = self.current_equity;
        }

        // Record trade
        let trade = BacktestTrade {
            entry_time: signal.timestamp,
            exit_time: entry_record.resolution_time,
            symbol: signal.symbol.clone(),
            market_id: signal.market_id.clone(),
            direction: if signal.buy_yes { "YES".into() } else { "NO".into() },
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
            our_volatility: self.vol_engine.estimate_volatility(&signal.symbol, None).combined_vol,
            implied_volatility: calculate_implied_volatility(
                entry_price.to_f64().unwrap_or(0.5),
                signal.buffer_pct.to_f64().unwrap_or(0.0),
                signal.time_remaining_secs as f64 / 900.0,
            ).unwrap_or(0.003),
        };

        // Update statistics
        self.results.total_trades += 1;
        self.results.total_volume += cost;
        self.results.total_pnl += pnl;

        if won {
            self.results.winning_trades += 1;
        } else {
            self.results.losing_trades += 1;
        }

        // Update symbol stats
        let symbol_stats = self.results.trades_by_symbol
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

        // Record equity curve point
        self.results.equity_curve.push((entry_record.resolution_time, self.current_equity));

        self.results.trades.push(trade);
    }

    fn calculate_statistics(&mut self) {
        let trades = &self.results.trades;

        if trades.is_empty() {
            return;
        }

        // Win rate
        self.results.win_rate = self.results.winning_trades as f64 / self.results.total_trades as f64;

        // Average PnL
        self.results.avg_pnl_per_trade = self.results.total_pnl / Decimal::from(self.results.total_trades);

        // Wins and losses
        let wins: Vec<_> = trades.iter().filter(|t| t.won).collect();
        let losses: Vec<_> = trades.iter().filter(|t| !t.won).collect();

        if !wins.is_empty() {
            self.results.avg_win = wins.iter().map(|t| t.pnl).sum::<Decimal>() / Decimal::from(wins.len() as u64);
            self.results.largest_win = wins.iter().map(|t| t.pnl).max().unwrap_or(Decimal::ZERO);
        }

        if !losses.is_empty() {
            self.results.avg_loss = losses.iter().map(|t| t.pnl).sum::<Decimal>() / Decimal::from(losses.len() as u64);
            self.results.largest_loss = losses.iter().map(|t| t.pnl).min().unwrap_or(Decimal::ZERO);
        }

        // Max drawdown
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

        // Profit factor
        let total_wins: Decimal = wins.iter().map(|t| t.pnl).sum();
        let total_losses: Decimal = losses.iter().map(|t| t.pnl.abs()).sum();

        if total_losses > Decimal::ZERO {
            self.results.profit_factor = (total_wins / total_losses).to_f64().unwrap_or(0.0);
        }

        // Sharpe ratio (simplified)
        let returns: Vec<f64> = trades.iter()
            .filter_map(|t| t.pnl_pct.to_f64())
            .collect();

        if returns.len() > 1 {
            let mean = returns.iter().sum::<f64>() / returns.len() as f64;
            let variance = returns.iter()
                .map(|r| (r - mean).powi(2))
                .sum::<f64>() / returns.len() as f64;
            let std_dev = variance.sqrt();

            if std_dev > 0.0 {
                // Annualized: assume ~100 trades per year
                self.results.sharpe_ratio = mean / std_dev * (100.0_f64).sqrt();
            }
        }

        // Average holding time
        let total_hold_time: i64 = trades.iter()
            .map(|t| (t.exit_time - t.entry_time).num_seconds())
            .sum();
        self.results.avg_holding_time_secs = total_hold_time as f64 / trades.len() as f64;

        // Update symbol win rates
        for stats in self.results.trades_by_symbol.values_mut() {
            if stats.total_trades > 0 {
                stats.win_rate = stats.winning_trades as f64 / stats.total_trades as f64;
            }
        }
    }
}

// ============================================================================
// Paper Trading
// ============================================================================

/// Paper trading logger - records signals without executing
pub struct PaperTrader {
    config: VolatilityArbConfig,
    vol_engine: VolatilityArbEngine,
    signals: Vec<PaperSignal>,
    pending_signals: HashMap<String, PaperSignal>,
    log_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperSignal {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub market_id: String,
    pub condition_id: String,
    pub direction: String,
    pub entry_price: Decimal,
    pub fair_value: Decimal,
    pub price_edge: Decimal,
    pub vol_edge_pct: f64,
    pub confidence: f64,
    pub recommended_shares: u64,
    pub buffer_pct: Decimal,
    pub our_volatility: f64,
    pub implied_volatility: f64,
    pub time_remaining_secs: u64,
    // Resolution tracking
    pub resolution_time: Option<DateTime<Utc>>,
    pub actual_outcome: Option<bool>,
    pub would_have_won: Option<bool>,
    pub theoretical_pnl: Option<Decimal>,
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

    /// Update volatility estimate
    pub fn update_volatility(&mut self, symbol: &str, kline_vol: f64) {
        self.vol_engine.update_kline_volatility(symbol, kline_vol);
    }

    /// Check for signal and record if found
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
        // Check if we already have a pending signal for this market
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
        ).unwrap_or(0.003);

        let paper_signal = PaperSignal {
            timestamp: Utc::now(),
            symbol: symbol.to_string(),
            market_id: market_id.to_string(),
            condition_id: condition_id.to_string(),
            direction: if signal.buy_yes { "YES".into() } else { "NO".into() },
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

        // Log signal
        self.log_signal(&paper_signal);

        // Store pending signal
        self.pending_signals.insert(market_id.to_string(), paper_signal.clone());

        info!(
            symbol,
            direction = paper_signal.direction,
            entry_price = %paper_signal.entry_price,
            fair_value = %paper_signal.fair_value,
            price_edge = %paper_signal.price_edge,
            vol_edge_pct = paper_signal.vol_edge_pct,
            confidence = paper_signal.confidence,
            "📝 Paper signal recorded"
        );

        Some(paper_signal)
    }

    /// Record market resolution
    pub fn record_resolution(&mut self, market_id: &str, outcome: bool) {
        if let Some(mut signal) = self.pending_signals.remove(market_id) {
            signal.resolution_time = Some(Utc::now());
            signal.actual_outcome = Some(outcome);

            // Did we predict correctly?
            let would_have_won = if signal.direction == "YES" {
                outcome
            } else {
                !outcome
            };
            signal.would_have_won = Some(would_have_won);

            // Calculate theoretical PnL
            let entry_price = signal.entry_price;
            let shares = signal.recommended_shares;
            let exit_price = if would_have_won { Decimal::ONE } else { Decimal::ZERO };
            let cost = entry_price * Decimal::from(shares);
            let revenue = exit_price * Decimal::from(shares);
            let fees = cost * self.config.pm_fee_rate;
            let pnl = revenue - cost - fees;
            signal.theoretical_pnl = Some(pnl);

            info!(
                market_id,
                direction = signal.direction,
                outcome = if outcome { "YES" } else { "NO" },
                would_have_won,
                theoretical_pnl = %pnl,
                "📊 Paper trade resolved"
            );

            // Log resolution
            self.log_resolution(&signal);

            self.signals.push(signal);
        }
    }

    /// Get paper trading statistics
    pub fn statistics(&self) -> PaperTradingStats {
        let resolved: Vec<_> = self.signals.iter()
            .filter(|s| s.would_have_won.is_some())
            .collect();

        if resolved.is_empty() {
            return PaperTradingStats::default();
        }

        let total = resolved.len() as u64;
        let wins = resolved.iter().filter(|s| s.would_have_won == Some(true)).count() as u64;
        let total_pnl: Decimal = resolved.iter()
            .filter_map(|s| s.theoretical_pnl)
            .sum();

        let avg_vol_edge = resolved.iter()
            .map(|s| s.vol_edge_pct)
            .sum::<f64>() / resolved.len() as f64;

        let avg_confidence = resolved.iter()
            .map(|s| s.confidence)
            .sum::<f64>() / resolved.len() as f64;

        PaperTradingStats {
            total_signals: total,
            winning_signals: wins,
            win_rate: wins as f64 / total as f64,
            theoretical_pnl: total_pnl,
            avg_vol_edge,
            avg_confidence,
            pending_signals: self.pending_signals.len() as u64,
        }
    }

    /// Get all recorded signals
    pub fn signals(&self) -> &[PaperSignal] {
        &self.signals
    }

    /// Get pending signals
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

    /// Export all signals to CSV
    pub fn export_csv<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let mut file = File::create(path).map_err(|e| e.to_string())?;

        // Header
        writeln!(file, "timestamp,symbol,market_id,direction,entry_price,fair_value,price_edge,vol_edge_pct,confidence,our_vol,implied_vol,buffer_pct,time_remaining,outcome,won,pnl")
            .map_err(|e| e.to_string())?;

        for s in &self.signals {
            writeln!(
                file,
                "{},{},{},{},{},{},{},{:.4},{:.4},{:.6},{:.6},{},{},{},{},{}",
                s.timestamp.format("%Y-%m-%d %H:%M:%S"),
                s.symbol,
                s.market_id,
                s.direction,
                s.entry_price,
                s.fair_value,
                s.price_edge,
                s.vol_edge_pct,
                s.confidence,
                s.our_volatility,
                s.implied_volatility,
                s.buffer_pct,
                s.time_remaining_secs,
                s.actual_outcome.map_or("pending".to_string(), |o| if o { "YES".to_string() } else { "NO".to_string() }),
                s.would_have_won.map_or("pending".to_string(), |w| if w { "WIN".to_string() } else { "LOSS".to_string() }),
                s.theoretical_pnl.map_or("0".to_string(), |p| p.to_string()),
            ).map_err(|e| e.to_string())?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaperTradingStats {
    pub total_signals: u64,
    pub winning_signals: u64,
    pub win_rate: f64,
    pub theoretical_pnl: Decimal,
    pub avg_vol_edge: f64,
    pub avg_confidence: f64,
    pub pending_signals: u64,
}

// ============================================================================
// Report Generation
// ============================================================================

impl BacktestResults {
    /// Generate a text report
    pub fn report(&self) -> String {
        let mut report = String::new();

        report.push_str("╔══════════════════════════════════════════════════════════════╗\n");
        report.push_str("║              VOLATILITY ARBITRAGE BACKTEST REPORT            ║\n");
        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");

        report.push_str(&format!("║ Period: {} to {}\n",
            self.start_time.format("%Y-%m-%d"),
            self.end_time.format("%Y-%m-%d")));

        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        report.push_str("║ PERFORMANCE SUMMARY                                          ║\n");
        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");

        report.push_str(&format!("║ Total Trades:      {:>10}                              ║\n", self.total_trades));
        report.push_str(&format!("║ Winning Trades:    {:>10}                              ║\n", self.winning_trades));
        report.push_str(&format!("║ Win Rate:          {:>10.2}%                             ║\n", self.win_rate * 100.0));
        report.push_str(&format!("║ Total PnL:         ${:>9.2}                             ║\n", self.total_pnl));
        report.push_str(&format!("║ Total Volume:      ${:>9.2}                             ║\n", self.total_volume));
        report.push_str(&format!("║ Avg PnL/Trade:     ${:>9.2}                             ║\n", self.avg_pnl_per_trade));

        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        report.push_str("║ RISK METRICS                                                 ║\n");
        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");

        report.push_str(&format!("║ Max Drawdown:      {:>10.2}%                             ║\n", self.max_drawdown * dec!(100)));
        report.push_str(&format!("║ Sharpe Ratio:      {:>10.2}                              ║\n", self.sharpe_ratio));
        report.push_str(&format!("║ Profit Factor:     {:>10.2}                              ║\n", self.profit_factor));

        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        report.push_str("║ WIN/LOSS ANALYSIS                                            ║\n");
        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");

        report.push_str(&format!("║ Average Win:       ${:>9.2}                             ║\n", self.avg_win));
        report.push_str(&format!("║ Average Loss:      ${:>9.2}                             ║\n", self.avg_loss));
        report.push_str(&format!("║ Largest Win:       ${:>9.2}                             ║\n", self.largest_win));
        report.push_str(&format!("║ Largest Loss:      ${:>9.2}                             ║\n", self.largest_loss));

        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        report.push_str("║ BY SYMBOL                                                    ║\n");
        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");

        for (symbol, stats) in &self.trades_by_symbol {
            report.push_str(&format!(
                "║ {:8} | Trades: {:>4} | Win: {:>5.1}% | PnL: ${:>8.2}        ║\n",
                symbol, stats.total_trades, stats.win_rate * 100.0, stats.total_pnl
            ));
        }

        report.push_str("╚══════════════════════════════════════════════════════════════╝\n");

        report
    }

    /// Export results to JSON
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kline_volatility() {
        let klines = vec![
            KlineRecord {
                timestamp: Utc::now(),
                symbol: "BTCUSDT".into(),
                open: dec!(100),
                high: dec!(101),
                low: dec!(99),
                close: dec!(100),
                volume: dec!(1000),
            },
            KlineRecord {
                timestamp: Utc::now(),
                symbol: "BTCUSDT".into(),
                open: dec!(100),
                high: dec!(102),
                low: dec!(99),
                close: dec!(101),
                volume: dec!(1000),
            },
            KlineRecord {
                timestamp: Utc::now(),
                symbol: "BTCUSDT".into(),
                open: dec!(101),
                high: dec!(102),
                low: dec!(100),
                close: dec!(100.5),
                volume: dec!(1000),
            },
        ];

        let vol = calculate_kline_volatility(&klines, 12);
        assert!(vol > 0.0);
        assert!(vol < 0.1); // Should be reasonable
    }

    #[test]
    fn test_paper_trader() {
        let config = VolatilityArbConfig::default();
        let mut trader = PaperTrader::new(config, None);

        // Set volatility
        trader.update_volatility("BTCUSDT", 0.003);

        // Check for signal
        let signal = trader.check_and_record(
            "BTCUSDT",
            "market_123",
            "condition_456",
            dec!(94500),
            dec!(94000),
            dec!(0.70),
            dec!(0.71),
            300,
            Some(0.0025),
        );

        // May or may not generate signal depending on edge
        println!("Signal: {:?}", signal);

        // Get stats
        let stats = trader.statistics();
        println!("Stats: {:?}", stats);
    }
}
