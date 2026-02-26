//! Volatility Arbitrage Strategy
//!
//! This strategy exploits mispricing in Polymarket 15-minute crypto binary options
//! by comparing market-implied volatility with our estimated realized volatility.
//!
//! ## Mathematical Foundation
//!
//! For a binary option paying $1 if S > K at expiration:
//!
//! ```text
//! P(YES) = N(d2)
//! d2 = [ln(S/K) - σ²T/2] / (σ√T)
//!
//! Simplified for small buffer:
//! d2 ≈ buffer% / (σ × √T)
//! ```
//!
//! ## Edge Source
//!
//! If market prices YES at $0.70 implying σ_implied = 0.4%
//! But our estimate is σ_realized = 0.25%
//! Then YES is underpriced → BUY YES
//!
//! ## Key Insight
//!
//! We're not predicting direction. We're predicting VOLATILITY.
//! If we estimate volatility better than the market, we have edge.

use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::f64::consts::PI;
use tracing::{debug, info};

// ============================================================================
// Configuration
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolatilityArbConfig {
    // === Volatility Estimation Weights ===
    /// Weight for K-line historical volatility (0.0 - 1.0)
    pub kline_weight: f64,
    /// Weight for tick-based volatility (0.0 - 1.0)
    pub tick_weight: f64,
    /// Number of K-line periods to use for vol estimation
    pub vol_lookback_periods: usize,

    // === Trading Thresholds ===
    /// Minimum volatility edge to trade (e.g., 0.20 = 20% vol difference)
    pub min_vol_edge_pct: f64,
    /// Minimum buffer from threshold to trade (avoid coin-flip situations)
    pub min_buffer_pct: Decimal,
    /// Maximum buffer from threshold (no edge when outcome is certain)
    pub max_buffer_pct: Decimal,
    /// Minimum price edge after fees to trade
    pub min_price_edge: Decimal,

    // === Time Windows ===
    /// Minimum seconds remaining to trade
    pub min_time_remaining_secs: u64,
    /// Maximum seconds remaining to trade
    pub max_time_remaining_secs: u64,
    /// Optimal time window for trading (highest edge)
    pub optimal_time_range: (u64, u64),

    // === Risk Management ===
    /// Maximum position size in USD per trade
    pub max_position_usd: Decimal,
    /// Kelly fraction for position sizing (0.25 = quarter Kelly)
    pub kelly_fraction: f64,
    /// Combined volatility level above which we reduce Kelly sizing
    #[serde(default = "default_high_vol_threshold")]
    pub high_vol_threshold: f64,
    /// Multiplier applied to Kelly sizing in high volatility regimes
    #[serde(default = "default_high_vol_kelly_multiplier")]
    pub high_vol_kelly_multiplier: f64,
    /// Maximum total exposure per symbol
    pub max_symbol_exposure_usd: Decimal,
    /// Cooldown between trades on same market
    pub cooldown_secs: u64,

    // === Fee Structure ===
    /// Polymarket trading fee rate
    pub pm_fee_rate: Decimal,

    // === Symbols to Trade ===
    pub symbols: Vec<String>,
}

impl Default for VolatilityArbConfig {
    fn default() -> Self {
        Self {
            // Volatility estimation: 70% K-line, 30% tick
            kline_weight: 0.70,
            tick_weight: 0.30,
            vol_lookback_periods: 12, // 12 x 15-min = 3 hours

            // Trading thresholds
            min_vol_edge_pct: 0.15,      // 15% volatility edge minimum
            min_buffer_pct: dec!(0.001), // 0.1% minimum buffer
            max_buffer_pct: dec!(0.02),  // 2% maximum buffer
            min_price_edge: dec!(0.03),  // 3% price edge minimum

            // Time windows (seconds)
            min_time_remaining_secs: 120,   // 2 minutes minimum
            max_time_remaining_secs: 600,   // 10 minutes maximum
            optimal_time_range: (180, 420), // 3-7 minutes optimal

            // Risk management
            max_position_usd: dec!(50),
            kelly_fraction: 0.25, // Quarter Kelly
            high_vol_threshold: default_high_vol_threshold(),
            high_vol_kelly_multiplier: default_high_vol_kelly_multiplier(),
            max_symbol_exposure_usd: dec!(100),
            cooldown_secs: 300, // 5 minute cooldown

            // Fees
            pm_fee_rate: dec!(0.02),

            // Default symbols
            symbols: vec!["BTCUSDT".into(), "ETHUSDT".into(), "SOLUSDT".into()],
        }
    }
}

fn default_high_vol_threshold() -> f64 {
    0.005
}

fn default_high_vol_kelly_multiplier() -> f64 {
    0.7
}

// ============================================================================
// Core Types
// ============================================================================

/// Volatility estimate with confidence
#[derive(Debug, Clone)]
pub struct VolatilityEstimate {
    /// Annualized volatility (but we use it for 15-min windows)
    pub kline_vol: f64,
    /// Tick-based volatility (60-second rolling)
    pub tick_vol: f64,
    /// Combined weighted estimate
    pub combined_vol: f64,
    /// Confidence in our estimate (0.0 - 1.0)
    pub confidence: f64,
    /// Sample size used
    pub sample_size: usize,
}

/// Market pricing information
#[derive(Debug, Clone)]
pub struct MarketPricing {
    /// Current YES price
    pub yes_price: Decimal,
    /// Current NO price
    pub no_price: Decimal,
    /// Best ask for YES
    pub yes_ask: Decimal,
    /// Best bid for YES
    pub yes_bid: Decimal,
    /// Spread
    pub spread: Decimal,
    /// Implied volatility from YES price
    pub implied_vol: f64,
}

/// Trading signal from volatility arbitrage
#[derive(Debug, Clone)]
pub struct VolArbSignal {
    /// Symbol (e.g., "BTCUSDT")
    pub symbol: String,
    /// Market ID
    pub market_id: String,
    /// Condition ID for trading
    pub condition_id: String,
    /// Direction: true = buy YES, false = buy NO
    pub buy_yes: bool,
    /// Our fair value for YES
    pub fair_value: Decimal,
    /// Current market YES price
    pub market_price: Decimal,
    /// Price edge (fair - market for YES, market - fair for NO)
    pub price_edge: Decimal,
    /// Volatility edge (our_vol vs implied_vol)
    pub vol_edge_pct: f64,
    /// Recommended position size
    pub position_size: u64,
    /// Confidence score (0.0 - 1.0)
    pub confidence: f64,
    /// Time remaining in seconds
    pub time_remaining_secs: u64,
    /// Current spot price
    pub spot_price: Decimal,
    /// Threshold price from market question
    pub threshold_price: Decimal,
    /// Buffer percentage
    pub buffer_pct: Decimal,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
}

/// Trade outcome for tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolArbTrade {
    pub signal: VolArbSignalRecord,
    pub entry_price: Decimal,
    pub exit_price: Option<Decimal>,
    pub shares: u64,
    pub pnl: Option<Decimal>,
    pub outcome: Option<bool>, // true = won, false = lost
    pub entry_time: DateTime<Utc>,
    pub exit_time: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolArbSignalRecord {
    pub symbol: String,
    pub buy_yes: bool,
    pub fair_value: Decimal,
    pub market_price: Decimal,
    pub price_edge: Decimal,
    pub vol_edge_pct: f64,
    pub confidence: f64,
    pub buffer_pct: Decimal,
    pub time_remaining_secs: u64,
}

/// Strategy statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VolArbStats {
    pub total_trades: u64,
    pub winning_trades: u64,
    pub total_pnl: Decimal,
    pub total_volume: Decimal,
    pub avg_edge: f64,
    pub avg_vol_edge: f64,
    pub win_rate: f64,
    pub sharpe_ratio: f64,
    pub trades_by_symbol: HashMap<String, u64>,
    pub pnl_by_symbol: HashMap<String, Decimal>,
}

// ============================================================================
// Mathematical Functions
// ============================================================================

/// Standard normal CDF approximation (Abramowitz and Stegun)
fn norm_cdf(x: f64) -> f64 {
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let z = x.abs() / 2.0_f64.sqrt();

    let t = 1.0 / (1.0 + p * z);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-z * z).exp();

    0.5 * (1.0 + sign * y)
}

/// Standard normal PDF
fn norm_pdf(x: f64) -> f64 {
    (-x * x / 2.0).exp() / (2.0 * PI).sqrt()
}

/// Inverse normal CDF approximation (Beasley-Springer-Moro)
fn norm_inv(p: f64) -> f64 {
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }

    let a = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.383577518672690e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    let b = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    let c = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    let d = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];

    let p_low = 0.02425;
    let p_high = 1.0 - p_low;

    if p < p_low {
        let q = (-2.0 * p.ln()).sqrt();
        (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
    } else if p <= p_high {
        let q = p - 0.5;
        let r = q * q;
        (((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q
            / (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
    }
}

/// Calculate fair YES price given buffer, volatility, and time
///
/// Uses simplified binary option pricing:
/// P(YES) = N(d2) where d2 = buffer / (σ * √T)
///
/// - buffer: (spot - threshold) / threshold as decimal (positive = above threshold)
/// - volatility: 15-minute volatility as decimal (e.g., 0.003 = 0.3%)
/// - time_remaining_fraction: fraction of 15-min window remaining (0.0 - 1.0)
pub fn calculate_fair_yes_price(buffer: f64, volatility: f64, time_remaining_fraction: f64) -> f64 {
    if volatility <= 0.0 || time_remaining_fraction <= 0.0 {
        // Edge case: no volatility or no time
        return if buffer > 0.0 { 1.0 } else { 0.0 };
    }

    // Adjust volatility for remaining time
    // σ_remaining = σ_15min * √(T_remaining / T_total)
    let adjusted_vol = volatility * time_remaining_fraction.sqrt();

    if adjusted_vol < 1e-10 {
        return if buffer > 0.0 { 1.0 } else { 0.0 };
    }

    // d2 = buffer / adjusted_vol
    let d2 = buffer / adjusted_vol;

    // P(YES) = N(d2)
    let prob = norm_cdf(d2);

    // Clamp to valid range
    prob.max(0.001).min(0.999)
}

/// Calculate implied volatility from market price
///
/// Given YES price, buffer, and time, solve for σ such that:
/// N(buffer / (σ * √T)) = YES_price
///
/// Uses Newton-Raphson iteration
pub fn calculate_implied_volatility(
    yes_price: f64,
    buffer: f64,
    time_remaining_fraction: f64,
) -> Option<f64> {
    if yes_price <= 0.0 || yes_price >= 1.0 {
        return None;
    }
    if time_remaining_fraction <= 0.0 {
        return None;
    }

    // Handle extreme cases
    if buffer.abs() < 1e-10 {
        // At the money - implied vol can be anything
        return Some(0.003); // Return reasonable default
    }

    // Initial guess based on inverting the formula
    // d2 = norm_inv(yes_price)
    // buffer / (σ * √T) = d2
    // σ = buffer / (d2 * √T)
    let d2_target = norm_inv(yes_price);

    if d2_target.abs() < 1e-10 {
        return Some(0.003); // Near ATM
    }

    let sqrt_t = time_remaining_fraction.sqrt();
    let initial_vol = (buffer / (d2_target * sqrt_t)).abs();

    // Newton-Raphson refinement
    let mut vol = initial_vol.max(0.0001).min(0.1);

    for _ in 0..20 {
        let adjusted_vol = vol * sqrt_t;
        if adjusted_vol < 1e-10 {
            break;
        }

        let d2 = buffer / adjusted_vol;
        let price = norm_cdf(d2);
        let error = price - yes_price;

        if error.abs() < 1e-8 {
            break;
        }

        // Vega (derivative of price w.r.t. vol)
        let vega = -norm_pdf(d2) * d2 / vol;

        if vega.abs() < 1e-10 {
            break;
        }

        vol -= error / vega;
        vol = vol.max(0.0001).min(0.1);
    }

    Some(vol)
}

/// Calculate Kelly fraction for position sizing
///
/// f* = (p * b - q) / b
/// where p = win probability, q = 1 - p, b = odds
///
/// For binary options: b = (1 - entry_price) / entry_price for YES
pub fn calculate_kelly_fraction(win_probability: f64, entry_price: f64) -> f64 {
    if entry_price <= 0.0 || entry_price >= 1.0 {
        return 0.0;
    }

    let p = win_probability;
    let q = 1.0 - p;
    let b = (1.0 - entry_price) / entry_price; // Odds

    let kelly = (p * b - q) / b;
    kelly.max(0.0) // Never go negative (don't short)
}

// ============================================================================
// Volatility Arbitrage Engine
// ============================================================================

pub struct VolatilityArbEngine {
    config: VolatilityArbConfig,
    /// K-line volatility cache: symbol -> 15-min volatility
    kline_vol_cache: HashMap<String, f64>,
    /// Recent trades for tracking
    recent_trades: Vec<VolArbTrade>,
    /// Last trade time per market
    last_trade_time: HashMap<String, DateTime<Utc>>,
    /// Current positions
    positions: HashMap<String, VolArbPosition>,
    /// Statistics
    stats: VolArbStats,
}

#[derive(Debug, Clone)]
pub struct VolArbPosition {
    pub market_id: String,
    pub condition_id: String,
    pub symbol: String,
    pub is_yes: bool,
    pub shares: u64,
    pub entry_price: Decimal,
    pub entry_time: DateTime<Utc>,
    pub signal: VolArbSignalRecord,
}

impl VolatilityArbEngine {
    pub fn new(config: VolatilityArbConfig) -> Self {
        Self {
            config,
            kline_vol_cache: HashMap::new(),
            recent_trades: Vec::new(),
            last_trade_time: HashMap::new(),
            positions: HashMap::new(),
            stats: VolArbStats::default(),
        }
    }

    /// Update K-line volatility for a symbol
    pub fn update_kline_volatility(&mut self, symbol: &str, volatility: f64) {
        self.kline_vol_cache.insert(symbol.to_string(), volatility);
        debug!(symbol, volatility, "Updated K-line volatility");
    }

    /// Get combined volatility estimate
    pub fn estimate_volatility(
        &self,
        symbol: &str,
        tick_volatility: Option<f64>,
    ) -> VolatilityEstimate {
        let kline_vol = self.kline_vol_cache.get(symbol).copied().unwrap_or(0.003);
        let tick_vol = tick_volatility.unwrap_or(kline_vol);

        // Combine vols by blending variances (more stable than linear vol blending).
        let weight_sum = self.config.kline_weight + self.config.tick_weight;
        let (wk, wt) = if weight_sum > 0.0 {
            (
                self.config.kline_weight / weight_sum,
                self.config.tick_weight / weight_sum,
            )
        } else {
            (0.5, 0.5)
        };
        let combined = (wk * kline_vol * kline_vol + wt * tick_vol * tick_vol).sqrt();

        // Confidence based on data availability
        let mut confidence = if self.kline_vol_cache.contains_key(symbol) {
            if tick_volatility.is_some() {
                0.9
            } else {
                0.7
            }
        } else {
            if tick_volatility.is_some() {
                0.5
            } else {
                0.3
            }
        };

        // Penalize confidence when kline/tick disagree (proxy for vol-of-vol / instability).
        if tick_volatility.is_some() {
            let denom = combined.max(1e-9);
            let disagreement = ((kline_vol - tick_vol).abs() / denom).min(1.0);
            let agreement_factor = (1.0 - disagreement).clamp(0.3, 1.0);
            confidence *= agreement_factor;
        }

        VolatilityEstimate {
            kline_vol,
            tick_vol,
            combined_vol: combined,
            confidence: confidence.clamp(0.0, 1.0),
            sample_size: self.config.vol_lookback_periods,
        }
    }

    /// Analyze market for arbitrage opportunity
    pub fn analyze_market(
        &self,
        symbol: &str,
        market_id: &str,
        condition_id: &str,
        spot_price: Decimal,
        threshold_price: Decimal,
        yes_price: Decimal,
        yes_ask: Decimal,
        time_remaining_secs: u64,
        tick_volatility: Option<f64>,
    ) -> Option<VolArbSignal> {
        // Check time window
        if time_remaining_secs < self.config.min_time_remaining_secs {
            debug!(time_remaining_secs, "Too little time remaining");
            return None;
        }
        if time_remaining_secs > self.config.max_time_remaining_secs {
            debug!(time_remaining_secs, "Too much time remaining");
            return None;
        }

        // Check cooldown
        if let Some(last_time) = self.last_trade_time.get(market_id) {
            let elapsed = Utc::now().signed_duration_since(*last_time).num_seconds() as u64;
            if elapsed < self.config.cooldown_secs {
                return None;
            }
        }

        // Calculate buffer
        let buffer_pct = if threshold_price > Decimal::ZERO {
            (spot_price - threshold_price) / threshold_price
        } else {
            return None;
        };

        // Check buffer range
        if buffer_pct.abs() < self.config.min_buffer_pct {
            debug!(%buffer_pct, "Buffer too small (coin flip)");
            return None;
        }
        if buffer_pct.abs() > self.config.max_buffer_pct {
            debug!(%buffer_pct, "Buffer too large (outcome certain)");
            return None;
        }

        // Get volatility estimate
        let vol_estimate = self.estimate_volatility(symbol, tick_volatility);

        // Calculate time remaining as fraction of 15-min window
        let time_fraction = (time_remaining_secs as f64) / 900.0;

        // Calculate implied volatility from market price
        let yes_price_f64 = yes_price.to_f64().unwrap_or(0.5);
        let buffer_f64 = buffer_pct.to_f64().unwrap_or(0.0);

        let implied_vol = calculate_implied_volatility(yes_price_f64, buffer_f64, time_fraction)?;

        // Calculate our fair value
        let fair_value_f64 =
            calculate_fair_yes_price(buffer_f64, vol_estimate.combined_vol, time_fraction);
        let fair_value = Decimal::from_f64(fair_value_f64).unwrap_or(dec!(0.5));

        // Calculate volatility edge
        let vol_edge_pct = (vol_estimate.combined_vol - implied_vol).abs() / implied_vol;

        // Check minimum volatility edge
        if vol_edge_pct < self.config.min_vol_edge_pct {
            debug!(
                vol_edge_pct,
                min = self.config.min_vol_edge_pct,
                "Insufficient vol edge"
            );
            return None;
        }

        // Determine direction and price edge
        let (buy_yes, price_edge, entry_price) = if vol_estimate.combined_vol < implied_vol {
            // Market overestimates volatility → YES is cheap → Buy YES
            let edge = fair_value - yes_ask;
            (true, edge, yes_ask)
        } else {
            // Market underestimates volatility → NO is cheap → Buy NO
            let no_price = Decimal::ONE - yes_price;
            let no_fair = Decimal::ONE - fair_value;
            let edge = no_fair - no_price;
            (false, edge, no_price)
        };

        // Check minimum price edge (after fees)
        let net_edge = price_edge - self.config.pm_fee_rate;
        if net_edge < self.config.min_price_edge {
            debug!(%net_edge, min = %self.config.min_price_edge, "Insufficient price edge");
            return None;
        }

        // Calculate confidence
        let time_confidence = if time_remaining_secs >= self.config.optimal_time_range.0
            && time_remaining_secs <= self.config.optimal_time_range.1
        {
            1.0
        } else {
            0.7
        };
        let confidence =
            (vol_estimate.confidence * time_confidence * (1.0 + vol_edge_pct)).min(1.0);

        // Calculate position size using Kelly criterion
        let win_prob = if buy_yes {
            fair_value_f64
        } else {
            1.0 - fair_value_f64
        };
        let kelly = calculate_kelly_fraction(win_prob, entry_price.to_f64().unwrap_or(0.5));
        let mut adjusted_kelly = kelly * self.config.kelly_fraction * confidence;
        if vol_estimate.combined_vol > self.config.high_vol_threshold {
            adjusted_kelly *= self.config.high_vol_kelly_multiplier;
        }
        adjusted_kelly = adjusted_kelly.clamp(0.0, 1.0);

        let max_shares = (self.config.max_position_usd / entry_price)
            .to_u64()
            .unwrap_or(100);
        let kelly_shares = (adjusted_kelly * max_shares as f64).round() as u64;
        let position_size = kelly_shares.max(10).min(max_shares);

        info!(
            symbol,
            %buffer_pct,
            our_vol = vol_estimate.combined_vol,
            implied_vol,
            vol_edge_pct,
            %fair_value,
            %entry_price,
            %price_edge,
            buy_yes,
            position_size,
            confidence,
            "Volatility arbitrage signal"
        );

        Some(VolArbSignal {
            symbol: symbol.to_string(),
            market_id: market_id.to_string(),
            condition_id: condition_id.to_string(),
            buy_yes,
            fair_value,
            market_price: if buy_yes {
                yes_ask
            } else {
                Decimal::ONE - yes_price
            },
            price_edge,
            vol_edge_pct,
            position_size,
            confidence,
            time_remaining_secs,
            spot_price,
            threshold_price,
            buffer_pct,
            timestamp: Utc::now(),
        })
    }

    /// Record a trade entry
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

    /// Record trade resolution
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

            // Update win rate
            if self.stats.total_trades > 0 {
                self.stats.win_rate =
                    self.stats.winning_trades as f64 / self.stats.total_trades as f64;
            }

            // Record trade
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

            // Keep only last 100 trades
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

    /// Get current statistics
    pub fn stats(&self) -> &VolArbStats {
        &self.stats
    }

    /// Get recent trades
    pub fn recent_trades(&self) -> &[VolArbTrade] {
        &self.recent_trades
    }

    /// Get current positions
    pub fn positions(&self) -> &HashMap<String, VolArbPosition> {
        &self.positions
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_norm_cdf() {
        // Test standard values
        assert!((norm_cdf(0.0) - 0.5).abs() < 0.001);
        assert!((norm_cdf(1.0) - 0.8413).abs() < 0.001);
        assert!((norm_cdf(-1.0) - 0.1587).abs() < 0.001);
        assert!((norm_cdf(2.0) - 0.9772).abs() < 0.001);
    }

    #[test]
    fn test_fair_yes_price() {
        // Buffer = 1%, Vol = 0.3%, Full time remaining
        // d2 = 0.01 / 0.003 = 3.33
        // N(3.33) ≈ 0.9996
        let price = calculate_fair_yes_price(0.01, 0.003, 1.0);
        assert!(price > 0.99);

        // Buffer = 0.1%, Vol = 0.3%, Full time
        // d2 = 0.001 / 0.003 = 0.33
        // N(0.33) ≈ 0.63
        let price = calculate_fair_yes_price(0.001, 0.003, 1.0);
        assert!(price > 0.6 && price < 0.7);

        // Negative buffer (below threshold)
        let price = calculate_fair_yes_price(-0.01, 0.003, 1.0);
        assert!(price < 0.01);
    }

    #[test]
    fn test_implied_volatility() {
        // Fair price 0.7, buffer 0.5%, half time remaining
        let implied = calculate_implied_volatility(0.7, 0.005, 0.5);
        assert!(implied.is_some());
        let vol = implied.unwrap();

        // Verify by calculating price back
        let price = calculate_fair_yes_price(0.005, vol, 0.5);
        assert!((price - 0.7).abs() < 0.01);
    }

    #[test]
    fn test_kelly_fraction() {
        // 60% win prob, entry at 0.50 (even odds)
        // b = 0.50 / 0.50 = 1.0
        // f = (0.6 * 1 - 0.4) / 1 = 0.2
        let kelly = calculate_kelly_fraction(0.6, 0.5);
        assert!((kelly - 0.2).abs() < 0.01);

        // 70% win prob, entry at 0.60
        // b = 0.40 / 0.60 = 0.667
        // f = (0.7 * 0.667 - 0.3) / 0.667 = 0.25
        let kelly = calculate_kelly_fraction(0.7, 0.6);
        assert!(kelly > 0.2 && kelly < 0.3);

        // No edge case
        let kelly = calculate_kelly_fraction(0.5, 0.5);
        assert!(kelly.abs() < 0.01);
    }

    #[test]
    fn test_vol_arb_engine() {
        let config = VolatilityArbConfig::default();
        let mut engine = VolatilityArbEngine::new(config);

        // Set up volatility
        engine.update_kline_volatility("BTCUSDT", 0.003);

        // Test signal generation
        let signal = engine.analyze_market(
            "BTCUSDT",
            "market_123",
            "condition_456",
            dec!(94500),  // Spot
            dec!(94000),  // Threshold
            dec!(0.70),   // YES price
            dec!(0.71),   // YES ask
            300,          // 5 minutes remaining
            Some(0.0025), // Tick volatility
        );

        // Signal should exist if there's vol edge
        // (depends on whether implied vol differs enough from our estimate)
        println!("Signal: {:?}", signal);
    }
}
