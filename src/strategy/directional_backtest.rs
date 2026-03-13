//! Directional backtest engine for momentum-driven binary option trading.
//!
//! Uses weighted momentum (10s/30s/60s) → fair value estimation → edge filtering
//! to enter positions, mirroring the live MomentumDetector logic. Holds to
//! settlement by default (binary options settle at $1.00 or $0.00).
//!
//! Binance spot price serves as Chainlink proxy (>99.9% correlation on 5m/15m).
//!
//! Usage:
//!   ploy strategy backtest directional --symbols BTCUSDT --save --json

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};

use crate::adapters::SpotPrice;
use crate::domain::Side;
use crate::strategy::backtest::BacktestResults;
use crate::strategy::backtest_feed::{MarketFeed, UpdateType};
use crate::strategy::backtest_recorder::{BacktestRecorder, NullRecorder};
use crate::strategy::execution_sim::ExecutionSimulator;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::momentum::Direction;

mod entry_lifecycle;
mod position_lifecycle;
mod reporting;

// ─────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────

/// Configuration for a directional backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionalBacktestConfig {
    /// Symbols to backtest (e.g. ["BTCUSDT", "ETHUSDT"])
    pub symbols: Vec<String>,
    /// Starting equity in USD
    pub initial_capital: Decimal,
    /// Position size in shares per trade
    pub shares_per_trade: u64,
    /// Maximum concurrent positions per symbol
    pub max_concurrent_positions: usize,
    /// Minimum edge to enter (fair_value - pm_ask - fees), e.g. 0.05 = 5%
    pub entry_threshold: f64,
    /// Don't buy YES above this price (e.g. 0.85)
    pub max_entry_price: Decimal,
    /// Don't buy YES below this price (e.g. 0.15)
    pub min_entry_price: Decimal,
    /// Minimum absolute momentum to trigger signal (e.g. 0.003 = 0.3%)
    pub min_momentum: Decimal,
    /// Time stop: exit if <N secs remaining AND position is underwater (e.g. 30)
    pub time_stop_secs: u64,
    /// Maximum loss per position in USD
    pub hard_stop_usd: Decimal,
    /// Hold winners to settlement (default true — let them run)
    pub hold_to_settlement: bool,
    /// Cooldown between entries on same symbol (seconds)
    pub cooldown_secs: u64,
    /// Minimum time remaining to enter a position (seconds).
    pub min_time_remaining_secs: u64,
    /// Maximum time remaining to enter (seconds).
    /// Only enter when outcome is becoming clearer.
    pub max_time_remaining_secs: u64,
    /// Use price_to_beat in fair value calculation
    pub use_price_to_beat: bool,
}

impl Default for DirectionalBacktestConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            initial_capital: dec!(10000),
            shares_per_trade: 100,
            max_concurrent_positions: 3,
            entry_threshold: 0.05,
            max_entry_price: dec!(0.85),
            min_entry_price: dec!(0.15),
            min_momentum: dec!(0.003), // 0.3% minimum move
            time_stop_secs: 30,
            hard_stop_usd: dec!(5),
            hold_to_settlement: true,
            cooldown_secs: 60,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            use_price_to_beat: true,
        }
    }
}

impl DirectionalBacktestConfig {
    pub fn with_symbols(symbols: Vec<String>) -> Self {
        Self {
            symbols,
            ..Default::default()
        }
    }
}

// ─────────────────────────────────────────────────────────────
// Position tracking
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DirectionalPosition {
    symbol: String,
    direction: Direction,
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    shares: u64,
    #[allow(dead_code)]
    event_slug: String,
    /// Window open price (Binance proxy for Chainlink S0)
    s0: Decimal,
    /// When the event window settles
    event_end_time: DateTime<Utc>,
    /// Model probability at entry
    entry_p_hat: f64,
    /// EV_net at entry for diagnostics
    entry_ev_net: f64,
    /// Realized vol at entry
    entry_sigma: f64,
    /// Latest PM price for mark-to-market
    latest_pm_price: Decimal,
}

/// A closed trade with directional-specific diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionalClosedTrade {
    pub symbol: String,
    pub direction: String,
    pub entry_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub shares: u64,
    pub pnl: Decimal,
    pub won: bool,
    pub holding_secs: i64,
    pub exit_reason: String,
    // Directional-specific fields
    pub entry_p_hat: f64,
    pub entry_ev_net: f64,
    pub s0: Decimal,
    pub entry_sigma: f64,
}

// ─────────────────────────────────────────────────────────────
// Active event window info
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ActiveWindowInfo {
    event_slug: String,
    /// S0 = price_to_beat from EventState
    s0: Decimal,
    end_time: DateTime<Utc>,
}

// ─────────────────────────────────────────────────────────────
// Engine
// ─────────────────────────────────────────────────────────────

pub struct DirectionalBacktestEngine {
    config: DirectionalBacktestConfig,
    fee_model: FeeModel,
    execution_sim: ExecutionSimulator,
    recorder: Box<dyn BacktestRecorder>,
    // Market state
    spot_prices: HashMap<String, SpotPrice>,
    pm_asks_by_event: HashMap<String, (Option<Decimal>, Option<Decimal>)>,
    // Active events: symbol -> concurrent windows (5m + 15m can overlap)
    active_events: HashMap<String, Vec<ActiveWindowInfo>>,
    // Positions & trades
    positions: Vec<DirectionalPosition>,
    closed_trades: Vec<DirectionalClosedTrade>,
    // Accounting
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
    equity_curve: Vec<(DateTime<Utc>, Decimal)>,
    last_entry_time: HashMap<String, DateTime<Utc>>,
    // Data range
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
    // Throttle: last timestamp we ran entry/exit logic per symbol
    last_logic_ts: HashMap<String, DateTime<Utc>>,
}

impl DirectionalBacktestEngine {
    pub fn new(config: DirectionalBacktestConfig, recorder: Box<dyn BacktestRecorder>) -> Self {
        let equity = config.initial_capital;
        Self {
            config,
            fee_model: FeeModel::crypto(),
            execution_sim: ExecutionSimulator::new(),
            recorder,
            spot_prices: HashMap::new(),
            pm_asks_by_event: HashMap::new(),
            active_events: HashMap::new(),
            positions: Vec::new(),
            closed_trades: Vec::new(),
            equity,
            peak_equity: equity,
            max_drawdown: Decimal::ZERO,
            equity_curve: Vec::new(),
            last_entry_time: HashMap::new(),
            data_range_start: None,
            data_range_end: None,
            last_logic_ts: HashMap::new(),
        }
    }

    pub fn new_without_recorder(config: DirectionalBacktestConfig) -> Self {
        Self::new(config, Box::new(NullRecorder))
    }

    pub fn config(&self) -> &DirectionalBacktestConfig {
        &self.config
    }

    pub fn closed_trades(&self) -> &[DirectionalClosedTrade] {
        &self.closed_trades
    }

    /// Take ownership of the recorder back from the engine.
    /// Useful for calling async methods (like `flush_async`/`finalize`) after `run()`.
    pub fn take_recorder(&mut self) -> Box<dyn BacktestRecorder> {
        std::mem::replace(&mut self.recorder, Box::new(NullRecorder))
    }

    // ─── Main loop ──────────────────────────────────────────

    /// Consume the feed and return aggregate results.
    pub fn run<F: MarketFeed>(&mut self, feed: &mut F) -> BacktestResults {
        while let Some(update) = feed.next_update() {
            // Track data range
            if self.data_range_start.is_none() {
                self.data_range_start = Some(update.timestamp);
            }
            self.data_range_end = Some(update.timestamp);

            // Prune expired events (end_time has passed without settlement)
            for events in self.active_events.values_mut() {
                events.retain(|e| e.end_time > update.timestamp);
            }

            match &update.update_type {
                UpdateType::SpotTrade { price, quantity } => {
                    self.handle_spot_trade(&update.symbol, *price, *quantity, update.timestamp);
                }
                UpdateType::PmQuote {
                    event_slug,
                    side,
                    best_ask,
                    ..
                } => {
                    self.handle_pm_quote(
                        &update.symbol,
                        event_slug,
                        *side,
                        *best_ask,
                        update.timestamp,
                    );
                }
                UpdateType::EventState {
                    event_slug,
                    end_time,
                    price_to_beat,
                    outcome,
                } => {
                    // Binary settlement — only close positions matching this event
                    if let Some(won) = outcome {
                        self.resolve_positions(&update.symbol, event_slug, *won, update.timestamp);
                        // Remove only the settled event, not all events for the symbol
                        if let Some(events) = self.active_events.get_mut(&update.symbol) {
                            events.retain(|e| e.event_slug != *event_slug);
                        }
                        self.pm_asks_by_event.remove(event_slug);
                    }

                    // Track active window: store S0 (price_to_beat) for probability calc
                    // Multiple events per symbol are allowed (5m + 15m overlap)
                    if outcome.is_none() {
                        if let (Some(end), Some(s0)) = (end_time, price_to_beat) {
                            let events =
                                self.active_events.entry(update.symbol.clone()).or_default();
                            // Don't add duplicate events
                            if !events.iter().any(|e| e.event_slug == *event_slug) {
                                events.push(ActiveWindowInfo {
                                    event_slug: event_slug.clone(),
                                    s0: *s0,
                                    end_time: *end,
                                });
                            }
                        }
                    }
                }
                UpdateType::LobSnapshot { .. } => {
                    // LOB depth not used by directional backtest
                }
                UpdateType::BinanceL2 { .. } => {
                    // Binance L2 features are ignored by the directional backtest.
                }
            }
        }

        // Force-close any remaining positions at latest PM price (data exhausted)
        self.close_remaining_positions();
        let _ = self.recorder.flush();
        self.build_results()
    }

    // ─── Event handlers ──────────────────────────────────────

    fn handle_spot_trade(
        &mut self,
        symbol: &str,
        price: Decimal,
        quantity: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        self.spot_prices
            .entry(symbol.to_string())
            .and_modify(|sp| sp.update(price, quantity, ts))
            .or_insert_with(|| SpotPrice::new(price, quantity, ts));
    }

    fn handle_pm_quote(
        &mut self,
        symbol: &str,
        event_slug: &str,
        quote_side: Side,
        best_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        // Update latest asks (per event_slug)
        let entry = self
            .pm_asks_by_event
            .entry(event_slug.to_string())
            .or_insert((None, None));
        match quote_side {
            Side::Up => {
                if best_ask.is_some() {
                    entry.0 = best_ask;
                }
            }
            Side::Down => {
                if best_ask.is_some() {
                    entry.1 = best_ask;
                }
            }
        }

        // Update position mark-to-market (cheap — just price assignment)
        for pos in &mut self.positions {
            if pos.symbol == symbol && pos.event_slug == event_slug {
                match pos.direction {
                    Direction::Up => {
                        if quote_side == Side::Up {
                            if let Some(ask) = best_ask {
                                pos.latest_pm_price = ask;
                            }
                        }
                    }
                    Direction::Down => {
                        if quote_side == Side::Down {
                            if let Some(ask) = best_ask {
                                pos.latest_pm_price = ask;
                            }
                        }
                    }
                }
            }
        }

        // Throttle entry/exit logic to once per second per symbol.
        // PM quotes arrive ~30-40/sec — running probability model on every tick is wasteful.
        let should_run_logic = match self.last_logic_ts.get(symbol) {
            Some(last) => (ts - *last).num_seconds() >= 1,
            None => true,
        };
        if !should_run_logic {
            return;
        }
        self.last_logic_ts.insert(symbol.to_string(), ts);

        // Try directional entry
        self.try_directional_entry(symbol, ts);

        // Check exits for existing positions
        self.check_exits(ts);

        // Record equity curve
        self.record_equity(ts);
    }

    // ─── Equity tracking ─────────────────────────────────────

    fn record_equity(&mut self, ts: DateTime<Utc>) {
        if self.equity > self.peak_equity {
            self.peak_equity = self.equity;
        }
        let drawdown = if self.peak_equity > Decimal::ZERO {
            (self.peak_equity - self.equity) / self.peak_equity
        } else {
            Decimal::ZERO
        };
        if drawdown > self.max_drawdown {
            self.max_drawdown = drawdown;
        }

        // Sample equity curve (max 1 point per second to avoid bloat)
        let should_record = self
            .equity_curve
            .last()
            .map(|(last_ts, _)| (ts - *last_ts).num_seconds() >= 1)
            .unwrap_or(true);
        if should_record {
            self.equity_curve.push((ts, self.equity));
        }
    }
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::backtest_feed::{HistoricalFeed, MarketUpdate};
    use std::collections::VecDeque;

    fn mock_feed(updates: Vec<MarketUpdate>) -> HistoricalFeed {
        HistoricalFeed {
            updates: VecDeque::from(updates),
        }
    }

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn test_empty_feed() {
        let config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);
        let mut feed = mock_feed(vec![]);
        let results = engine.run(&mut feed);

        assert_eq!(results.total_trades, 0);
        assert_eq!(results.total_pnl, Decimal::ZERO);
    }

    #[test]
    fn test_settlement_binary_payout() {
        // Setup: create a position via momentum signal, then settle it.
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.0; // Accept any positive edge
        config.min_entry_price = dec!(0.01);
        config.max_entry_price = dec!(0.99);
        config.shares_per_trade = 100;
        config.min_momentum = dec!(0.001); // Low threshold for test
        config.min_time_remaining_secs = 30;
        config.max_time_remaining_secs = 600;

        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);

        let base = ts(0);
        let end_time = ts(300); // 5 min window

        let mut updates = vec![];

        // Event opens: S0 = 100
        updates.push(MarketUpdate {
            timestamp: base,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });

        // Build spot price history with UPWARD momentum (100.00 → 101.50)
        // Need enough points spread over 60s for weighted_momentum to work
        for i in 1..=60 {
            let price = dec!(100) + Decimal::from(i) * dec!(0.025);
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price,
                    quantity: Some(dec!(1)),
                },
            });
        }

        // PM quote with cheap UP ask — momentum is up, so should buy UP
        updates.push(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.40)),
            },
        });

        // Settlement: UP wins
        updates.push(MarketUpdate {
            timestamp: end_time,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: Some(true),
            },
        });

        let mut feed = mock_feed(updates);
        let results = engine.run(&mut feed);

        assert!(results.total_trades >= 1, "Expected at least 1 trade");

        let trades = engine.closed_trades();
        if !trades.is_empty() {
            let t = &trades[0];
            assert_eq!(t.exit_reason, "settlement");
            assert_eq!(t.direction, "UP");
            assert!(t.won, "UP trade should win when UP settles");
            assert!(t.pnl > Decimal::ZERO, "PnL should be positive");
            assert_eq!(t.exit_price, Decimal::ONE, "Settlement pays $1.00");
        }
    }

    #[test]
    fn test_entry_edge_filter() {
        // High entry threshold should reject entries
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.99; // Impossibly high edge requirement
        config.shares_per_trade = 100;
        config.min_momentum = dec!(0.001);
        config.min_time_remaining_secs = 30;
        config.max_time_remaining_secs = 600;

        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);

        let base = ts(0);
        let end_time = ts(300);
        let mut updates = vec![];

        updates.push(MarketUpdate {
            timestamp: base,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });

        for i in 1..=60 {
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100) + Decimal::from(i) * dec!(0.02),
                    quantity: Some(dec!(1)),
                },
            });
        }

        updates.push(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.50)),
            },
        });

        let mut feed = mock_feed(updates);
        let results = engine.run(&mut feed);

        assert_eq!(
            results.total_trades, 0,
            "No trades should pass 99% edge threshold"
        );
    }

    #[test]
    fn test_hold_to_settlement() {
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.0;
        config.hold_to_settlement = true;
        config.hard_stop_usd = dec!(999);
        config.min_entry_price = dec!(0.01);
        config.max_entry_price = dec!(0.99);
        config.shares_per_trade = 10;
        config.min_momentum = dec!(0.001);
        config.min_time_remaining_secs = 30;
        config.max_time_remaining_secs = 600;

        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);

        let base = ts(0);
        let end_time = ts(300);
        let mut updates = vec![];

        updates.push(MarketUpdate {
            timestamp: base,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });

        // Upward momentum
        for i in 1..=60 {
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100) + Decimal::from(i) * dec!(0.025),
                    quantity: Some(dec!(1)),
                },
            });
        }

        // Entry quote
        updates.push(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.30)),
            },
        });

        // Adverse PM quote but NO settlement
        updates.push(MarketUpdate {
            timestamp: ts(100),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.20)),
            },
        });

        updates.push(MarketUpdate {
            timestamp: ts(200),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.15)),
            },
        });

        let mut feed = mock_feed(updates);
        let _results = engine.run(&mut feed);

        let trades = engine.closed_trades();
        if !trades.is_empty() {
            assert_eq!(
                trades[0].exit_reason, "data_exhausted",
                "Should hold to settlement, closed only because feed ended"
            );
        }
    }

    #[test]
    fn test_hard_stop() {
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.0;
        config.hold_to_settlement = false;
        config.hard_stop_usd = dec!(1); // Very tight stop: $1
        config.min_entry_price = dec!(0.01);
        config.max_entry_price = dec!(0.99);
        config.shares_per_trade = 100;
        config.min_momentum = dec!(0.001);
        config.min_time_remaining_secs = 30;
        config.max_time_remaining_secs = 600;

        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);

        let base = ts(0);
        let end_time = ts(300);
        let mut updates = vec![];

        updates.push(MarketUpdate {
            timestamp: base,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });

        // Upward momentum to trigger entry
        for i in 1..=60 {
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100) + Decimal::from(i) * dec!(0.025),
                    quantity: Some(dec!(1)),
                },
            });
        }

        // Entry at 0.40
        updates.push(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.40)),
            },
        });

        // Price crashes to 0.10 — unrealized loss = 100 * (0.10 - ~0.40) ≈ -$30 > $1 stop
        updates.push(MarketUpdate {
            timestamp: ts(100),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.10)),
            },
        });

        let mut feed = mock_feed(updates);
        let _results = engine.run(&mut feed);

        let trades = engine.closed_trades();
        let hard_stopped = trades.iter().any(|t| t.exit_reason == "hard_stop");
        assert!(
            hard_stopped || trades.is_empty(),
            "Expected hard_stop exit or no entry (if edge filter blocked)"
        );
    }
}
