//! Thin replay harness for the 5-minute crypto repricing core.
//!
//! This module deliberately stays narrow:
//! - consumes `HistoricalFeed` / `MarketFeed`
//! - reuses the pure repricing core for fair value and entry decisions
//! - keeps execution assumptions simple and explicit
//! - does not depend on recorder, CLI, or the legacy backtest report shell

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::domain::Side;
use crate::strategy::backtest_feed::{MarketFeed, UpdateType};
use crate::strategy::crypto_repricing::{
    direction_score, estimate_remaining_fair_value, evaluate_entry_candidate,
    BinanceFeatureSnapshot, CryptoRepricingConfig, FairValueEstimate, QuotePair, QuoteWithDepth,
    RepricingSide,
};
use crate::strategy::fee_model::FeeModel;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoRepricingReplayConfig {
    pub strategy: CryptoRepricingConfig,
    pub initial_capital: Decimal,
    pub expected_window_secs: u64,
    pub window_tolerance_secs: u64,
    pub max_positions_per_symbol: usize,
    pub price_history_keep_secs: u64,
    pub max_holding_secs: u64,
    pub take_profit_gap_fraction: Decimal,
    pub signal_stop_gap_fraction: Decimal,
}

impl Default for CryptoRepricingReplayConfig {
    fn default() -> Self {
        Self {
            strategy: CryptoRepricingConfig::default(),
            initial_capital: dec!(10000),
            expected_window_secs: 300,
            window_tolerance_secs: 15,
            max_positions_per_symbol: 1,
            price_history_keep_secs: 600,
            max_holding_secs: 30,
            take_profit_gap_fraction: dec!(0.70),
            signal_stop_gap_fraction: dec!(0.50),
        }
    }
}

impl CryptoRepricingReplayConfig {
    pub fn with_symbols(symbols: Vec<String>) -> Self {
        Self {
            strategy: CryptoRepricingConfig::with_symbols(symbols),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoRepricingReplayTrade {
    pub symbol: String,
    pub event_slug: String,
    pub side: String,
    pub entry_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub shares: u64,
    pub pnl: Decimal,
    pub won: bool,
    pub holding_secs: i64,
    pub exit_reason: String,
    pub entry_fair_probability: f64,
    pub exit_fair_probability: Option<f64>,
    pub entry_direction_score: f64,
    pub exit_direction_score: Option<f64>,
    pub entry_gap: Decimal,
    pub exit_gap: Option<Decimal>,
    pub strike: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoRepricingReplayResults {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub total_trades: u64,
    pub winning_trades: u64,
    pub losing_trades: u64,
    pub win_rate: f64,
    pub total_pnl: Decimal,
    pub avg_pnl_per_trade: Decimal,
    pub largest_win: Decimal,
    pub largest_loss: Decimal,
    pub avg_holding_secs: f64,
    pub max_drawdown: Decimal,
    pub trades: Vec<CryptoRepricingReplayTrade>,
    pub equity_curve: Vec<(DateTime<Utc>, Decimal)>,
}

impl Default for CryptoRepricingReplayResults {
    fn default() -> Self {
        Self {
            start_time: Utc::now(),
            end_time: Utc::now(),
            total_trades: 0,
            winning_trades: 0,
            losing_trades: 0,
            win_rate: 0.0,
            total_pnl: Decimal::ZERO,
            avg_pnl_per_trade: Decimal::ZERO,
            largest_win: Decimal::ZERO,
            largest_loss: Decimal::ZERO,
            avg_holding_secs: 0.0,
            max_drawdown: Decimal::ZERO,
            trades: Vec::new(),
            equity_curve: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveEvent {
    event_slug: String,
    strike: Decimal,
    end_time: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
struct EventQuoteState {
    up_bid: Option<Decimal>,
    up_ask: Option<Decimal>,
    up_depth: Option<u64>,
    down_bid: Option<Decimal>,
    down_ask: Option<Decimal>,
    down_depth: Option<u64>,
}

impl EventQuoteState {
    fn update_pm_quote(
        &mut self,
        side: Side,
        best_bid: Option<Decimal>,
        best_ask: Option<Decimal>,
    ) {
        match side {
            Side::Up => {
                if best_bid.is_some() {
                    self.up_bid = best_bid;
                }
                if best_ask.is_some() {
                    self.up_ask = best_ask;
                }
            }
            Side::Down => {
                if best_bid.is_some() {
                    self.down_bid = best_bid;
                }
                if best_ask.is_some() {
                    self.down_ask = best_ask;
                }
            }
        }
    }

    fn update_lob_snapshot(
        &mut self,
        side: &str,
        ask_depth_shares: u64,
        best_ask: Option<Decimal>,
    ) {
        match side {
            "UP" => {
                self.up_depth = Some(ask_depth_shares);
                if best_ask.is_some() {
                    self.up_ask = best_ask;
                }
            }
            "DOWN" => {
                self.down_depth = Some(ask_depth_shares);
                if best_ask.is_some() {
                    self.down_ask = best_ask;
                }
            }
            _ => {
                self.up_depth = Some(ask_depth_shares);
                self.down_depth = Some(ask_depth_shares);
            }
        }
    }

    fn quote_pair(&self) -> QuotePair {
        QuotePair {
            yes: QuoteWithDepth {
                best_bid: self.up_bid,
                best_ask: self.up_ask,
                ask_depth_shares: self.up_depth,
            },
            no: QuoteWithDepth {
                best_bid: self.down_bid,
                best_ask: self.down_ask,
                ask_depth_shares: self.down_depth,
            },
        }
    }

    fn bid_for(&self, side: RepricingSide, tick_size: Decimal) -> Option<Decimal> {
        match side {
            RepricingSide::Yes => self
                .up_bid
                .or_else(|| self.up_ask.map(|ask| (ask - tick_size).max(tick_size))),
            RepricingSide::No => self
                .down_bid
                .or_else(|| self.down_ask.map(|ask| (ask - tick_size).max(tick_size))),
        }
    }
}

#[derive(Debug, Clone)]
struct OpenPosition {
    symbol: String,
    event_slug: String,
    side: RepricingSide,
    entry_time: DateTime<Utc>,
    entry_price: Decimal,
    shares: u64,
    strike: Decimal,
    end_time: DateTime<Utc>,
    latest_bid: Decimal,
    entry_fair_probability: f64,
    entry_direction_score: f64,
    entry_gap: Decimal,
}

pub struct CryptoRepricingReplayEngine {
    config: CryptoRepricingReplayConfig,
    fee_model: FeeModel,
    spot_history: HashMap<String, VecDeque<(DateTime<Utc>, Decimal)>>,
    binance_l2: HashMap<String, BinanceFeatureSnapshot>,
    active_events: HashMap<String, Vec<ActiveEvent>>,
    quotes_by_event: HashMap<String, EventQuoteState>,
    positions: Vec<OpenPosition>,
    closed_trades: Vec<CryptoRepricingReplayTrade>,
    cash: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
    equity_curve: Vec<(DateTime<Utc>, Decimal)>,
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
}

impl CryptoRepricingReplayEngine {
    pub fn new(config: CryptoRepricingReplayConfig) -> Self {
        let cash = config.initial_capital;
        Self {
            config,
            fee_model: FeeModel::crypto(),
            spot_history: HashMap::new(),
            binance_l2: HashMap::new(),
            active_events: HashMap::new(),
            quotes_by_event: HashMap::new(),
            positions: Vec::new(),
            closed_trades: Vec::new(),
            cash,
            peak_equity: cash,
            max_drawdown: Decimal::ZERO,
            equity_curve: Vec::new(),
            data_range_start: None,
            data_range_end: None,
        }
    }

    pub fn config(&self) -> &CryptoRepricingReplayConfig {
        &self.config
    }

    pub fn closed_trades(&self) -> &[CryptoRepricingReplayTrade] {
        &self.closed_trades
    }

    pub fn run<F: MarketFeed>(&mut self, feed: &mut F) -> CryptoRepricingReplayResults {
        while let Some(update) = feed.next_update() {
            if !self.symbol_enabled(&update.symbol) {
                continue;
            }

            if self.data_range_start.is_none() {
                self.data_range_start = Some(update.timestamp);
            }
            self.data_range_end = Some(update.timestamp);

            if let Some(events) = self.active_events.get_mut(&update.symbol) {
                events.retain(|event| event.end_time > update.timestamp);
            }

            match &update.update_type {
                UpdateType::SpotTrade { price, .. } => {
                    self.handle_spot_trade(&update.symbol, *price, update.timestamp);
                    self.maybe_run_symbol_logic(&update.symbol, update.timestamp);
                }
                UpdateType::PmQuote {
                    event_slug,
                    side,
                    best_bid,
                    best_ask,
                    ..
                } => {
                    self.handle_pm_quote(&update.symbol, event_slug, *side, *best_bid, *best_ask);
                    self.maybe_run_symbol_logic(&update.symbol, update.timestamp);
                }
                UpdateType::EventState {
                    event_slug,
                    end_time,
                    price_to_beat,
                    outcome,
                } => {
                    self.handle_event_state(
                        &update.symbol,
                        event_slug,
                        *end_time,
                        *price_to_beat,
                        *outcome,
                        update.timestamp,
                    );
                }
                UpdateType::LobSnapshot {
                    side,
                    ask_depth_shares,
                    best_ask,
                } => {
                    self.handle_lob_snapshot(&update.symbol, side, *ask_depth_shares, *best_ask);
                    self.maybe_run_symbol_logic(&update.symbol, update.timestamp);
                }
                UpdateType::BinanceL2 {
                    obi_5,
                    obi_10,
                    bid_volume_5,
                    ask_volume_5,
                    spread_bps,
                } => {
                    self.binance_l2.insert(
                        update.symbol.clone(),
                        BinanceFeatureSnapshot {
                            obi_5: Some(*obi_5),
                            obi_10: Some(*obi_10),
                            bid_volume_5: Some(*bid_volume_5),
                            ask_volume_5: Some(*ask_volume_5),
                            spread_bps: Some(*spread_bps),
                        },
                    );
                    self.maybe_run_symbol_logic(&update.symbol, update.timestamp);
                }
            }
        }

        self.close_remaining_positions();
        self.build_results()
    }

    fn symbol_enabled(&self, symbol: &str) -> bool {
        self.config.strategy.symbols.is_empty()
            || self
                .config
                .strategy
                .symbols
                .iter()
                .any(|configured| configured == symbol)
    }

    fn handle_spot_trade(&mut self, symbol: &str, price: Decimal, ts: DateTime<Utc>) {
        let history = self.spot_history.entry(symbol.to_string()).or_default();
        history.push_back((ts, price));
        let cutoff = ts - chrono::Duration::seconds(self.config.price_history_keep_secs as i64);
        while history
            .front()
            .map(|(sample_ts, _)| *sample_ts < cutoff)
            .unwrap_or(false)
        {
            history.pop_front();
        }
    }

    fn handle_pm_quote(
        &mut self,
        symbol: &str,
        event_slug: &str,
        side: Side,
        best_bid: Option<Decimal>,
        best_ask: Option<Decimal>,
    ) {
        self.quotes_by_event
            .entry(event_slug.to_string())
            .or_default()
            .update_pm_quote(side, best_bid, best_ask);

        for position in &mut self.positions {
            if position.symbol != symbol || position.event_slug != event_slug {
                continue;
            }
            if let Some(state) = self.quotes_by_event.get(event_slug) {
                if let Some(bid) = state.bid_for(position.side, self.config.strategy.tick_size) {
                    position.latest_bid = bid;
                }
            }
        }
    }

    fn handle_lob_snapshot(
        &mut self,
        symbol: &str,
        side: &str,
        ask_depth_shares: u64,
        best_ask: Option<Decimal>,
    ) {
        if let Some(events) = self.active_events.get(symbol) {
            for event in events {
                self.quotes_by_event
                    .entry(event.event_slug.clone())
                    .or_default()
                    .update_lob_snapshot(side, ask_depth_shares, best_ask);
            }
        }
    }

    fn handle_event_state(
        &mut self,
        symbol: &str,
        event_slug: &str,
        end_time: Option<DateTime<Utc>>,
        price_to_beat: Option<Decimal>,
        outcome: Option<bool>,
        ts: DateTime<Utc>,
    ) {
        if let Some(up_won) = outcome {
            self.resolve_positions(symbol, event_slug, up_won, ts);
            if let Some(events) = self.active_events.get_mut(symbol) {
                events.retain(|event| event.event_slug != event_slug);
            }
            self.quotes_by_event.remove(event_slug);
            self.record_equity(ts);
            return;
        }

        let (Some(end_time), Some(strike)) = (end_time, price_to_beat) else {
            return;
        };

        let duration_secs = (end_time - ts).num_seconds();
        let allowed = (duration_secs - self.config.expected_window_secs as i64).abs()
            <= self.config.window_tolerance_secs as i64;
        if !allowed {
            return;
        }

        let events = self.active_events.entry(symbol.to_string()).or_default();
        if !events.iter().any(|event| event.event_slug == event_slug) {
            events.push(ActiveEvent {
                event_slug: event_slug.to_string(),
                strike,
                end_time,
            });
        }
    }

    fn maybe_run_symbol_logic(&mut self, symbol: &str, ts: DateTime<Utc>) {
        let events = match self.active_events.get(symbol) {
            Some(events) if !events.is_empty() => events.clone(),
            _ => {
                self.record_equity(ts);
                return;
            }
        };

        for event in events {
            if let Some(position_idx) = self.positions.iter().position(|position| {
                position.symbol == symbol && position.event_slug == event.event_slug
            }) {
                self.maybe_exit_position(position_idx, ts);
            } else if self.open_positions_for_symbol(symbol) < self.config.max_positions_per_symbol
            {
                self.try_entry(symbol, &event, ts);
            }
        }

        self.record_equity(ts);
    }

    fn open_positions_for_symbol(&self, symbol: &str) -> usize {
        self.positions
            .iter()
            .filter(|position| position.symbol == symbol)
            .count()
    }

    fn try_entry(&mut self, symbol: &str, event: &ActiveEvent, ts: DateTime<Utc>) {
        let history = match self.spot_history.get(symbol) {
            Some(history) if !history.is_empty() => history,
            _ => return,
        };
        let spot = match history.back() {
            Some((_, spot)) => *spot,
            None => return,
        };
        let quotes = match self.quotes_by_event.get(&event.event_slug) {
            Some(quotes) => quotes,
            None => return,
        };

        let remaining_secs = (event.end_time - ts).num_seconds();
        let fair = match estimate_remaining_fair_value(
            &self.config.strategy,
            history,
            ts,
            spot,
            event.strike,
            remaining_secs,
        ) {
            Some(fair) => fair,
            None => return,
        };

        let l2 = self.binance_l2.get(symbol).copied().unwrap_or_default();
        let dir_score = direction_score(l2, fair);
        let decision = match evaluate_entry_candidate(
            &self.config.strategy,
            &self.fee_model,
            quotes.quote_pair(),
            fair,
            dir_score,
            remaining_secs,
        ) {
            Ok(decision) => decision,
            Err(_) => return,
        };

        let shares = self.config.strategy.shares_per_trade;
        let entry_notional = Decimal::from(shares) * decision.quote_price;
        let entry_fee = self
            .fee_model
            .fee_shares(Decimal::from(shares), decision.quote_price)
            * decision.quote_price;
        let total_entry_cost = entry_notional + entry_fee;
        if total_entry_cost > self.cash {
            return;
        }

        let latest_bid = match quotes.bid_for(decision.side, self.config.strategy.tick_size) {
            Some(bid) => bid,
            None => return,
        };

        self.cash -= total_entry_cost;
        self.positions.push(OpenPosition {
            symbol: symbol.to_string(),
            event_slug: event.event_slug.clone(),
            side: decision.side,
            entry_time: ts,
            entry_price: decision.quote_price,
            shares,
            strike: event.strike,
            end_time: event.end_time,
            latest_bid,
            entry_fair_probability: decision.fair_probability,
            entry_direction_score: decision.direction_score,
            entry_gap: decision.gross_gap,
        });
    }

    fn maybe_exit_position(&mut self, idx: usize, ts: DateTime<Utc>) {
        if idx >= self.positions.len() {
            return;
        }

        let event_slug = self.positions[idx].event_slug.clone();
        let quotes = match self.quotes_by_event.get(&event_slug) {
            Some(quotes) => quotes.clone(),
            None => return,
        };

        let side = self.positions[idx].side;
        let exit_bid = match quotes.bid_for(side, self.config.strategy.tick_size) {
            Some(bid) => bid,
            None => return,
        };
        self.positions[idx].latest_bid = exit_bid;

        let remaining_secs = (self.positions[idx].end_time - ts).num_seconds();
        if remaining_secs <= self.config.strategy.hard_flat_secs as i64 {
            self.close_position(idx, exit_bid, "hard_flat", ts, None, None, true);
            return;
        }

        let symbol = self.positions[idx].symbol.clone();
        let history = match self.spot_history.get(&symbol) {
            Some(history) if !history.is_empty() => history,
            _ => return,
        };
        let spot = match history.back() {
            Some((_, spot)) => *spot,
            None => return,
        };

        let fair = match estimate_remaining_fair_value(
            &self.config.strategy,
            history,
            ts,
            spot,
            self.positions[idx].strike,
            remaining_secs,
        ) {
            Some(fair) => fair,
            None => return,
        };
        let l2 = self.binance_l2.get(&symbol).copied().unwrap_or_default();
        let dir_score = direction_score(l2, fair);
        let fair_probability = fair_probability_for_side(side, fair);
        let current_gap =
            Decimal::from_f64_retain(fair_probability).unwrap_or(dec!(0.5)) - exit_bid;
        let gap_closed_fraction = gap_closed_fraction(self.positions[idx].entry_gap, current_gap);
        let holding_secs = (ts - self.positions[idx].entry_time).num_seconds();

        if gap_closed_fraction >= self.config.take_profit_gap_fraction
            && exit_bid > self.positions[idx].entry_price
        {
            self.close_position(
                idx,
                exit_bid,
                "take_profit",
                ts,
                Some(fair_probability),
                Some(dir_score),
                true,
            );
            return;
        }

        let dir_reversed = match side {
            RepricingSide::Yes => dir_score < 0.0,
            RepricingSide::No => dir_score > 0.0,
        };
        if dir_reversed && gap_closed_fraction >= self.config.signal_stop_gap_fraction {
            self.close_position(
                idx,
                exit_bid,
                "signal_stop",
                ts,
                Some(fair_probability),
                Some(dir_score),
                true,
            );
            return;
        }

        if holding_secs >= self.config.max_holding_secs as i64
            && exit_bid <= self.positions[idx].entry_price
        {
            self.close_position(
                idx,
                exit_bid,
                "time_stop",
                ts,
                Some(fair_probability),
                Some(dir_score),
                true,
            );
        }
    }

    fn resolve_positions(
        &mut self,
        symbol: &str,
        event_slug: &str,
        up_won: bool,
        ts: DateTime<Utc>,
    ) {
        let mut to_close = Vec::new();
        for (idx, position) in self.positions.iter().enumerate() {
            if position.symbol == symbol && position.event_slug == event_slug {
                let payout = match (position.side, up_won) {
                    (RepricingSide::Yes, true) | (RepricingSide::No, false) => Decimal::ONE,
                    _ => Decimal::ZERO,
                };
                to_close.push((idx, payout));
            }
        }
        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, payout) in to_close {
            self.close_position(
                idx,
                payout,
                "settlement",
                ts,
                Some(payout.to_f64().unwrap_or(0.0)),
                None,
                false,
            );
        }
    }

    fn close_position(
        &mut self,
        idx: usize,
        exit_price: Decimal,
        reason: &str,
        ts: DateTime<Utc>,
        exit_fair_probability: Option<f64>,
        exit_direction_score: Option<f64>,
        charge_exit_fee: bool,
    ) {
        let position = self.positions.remove(idx);
        let exit_proceeds = Decimal::from(position.shares) * exit_price;
        let exit_fee = if charge_exit_fee {
            self.fee_model
                .fee_shares(Decimal::from(position.shares), exit_price)
                * exit_price
        } else {
            Decimal::ZERO
        };
        let proceeds_after_fee = exit_proceeds - exit_fee;
        self.cash += proceeds_after_fee;

        let entry_notional = Decimal::from(position.shares) * position.entry_price;
        let entry_fee = self
            .fee_model
            .fee_shares(Decimal::from(position.shares), position.entry_price)
            * position.entry_price;
        let pnl = proceeds_after_fee - entry_notional - entry_fee;
        let exit_gap = exit_fair_probability
            .and_then(Decimal::from_f64_retain)
            .map(|fair_probability| fair_probability - exit_price);

        self.closed_trades.push(CryptoRepricingReplayTrade {
            symbol: position.symbol,
            event_slug: position.event_slug,
            side: position.side.to_string(),
            entry_time: position.entry_time,
            exit_time: ts,
            entry_price: position.entry_price,
            exit_price,
            shares: position.shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs: (ts - position.entry_time).num_seconds(),
            exit_reason: reason.to_string(),
            entry_fair_probability: position.entry_fair_probability,
            exit_fair_probability,
            entry_direction_score: position.entry_direction_score,
            exit_direction_score,
            entry_gap: position.entry_gap,
            exit_gap,
            strike: position.strike,
        });
    }

    fn close_remaining_positions(&mut self) {
        let ts = self.data_range_end.unwrap_or_else(Utc::now);
        let indices: Vec<usize> = (0..self.positions.len()).rev().collect();
        for idx in indices {
            let exit_price = self.positions[idx].latest_bid;
            self.close_position(idx, exit_price, "data_exhausted", ts, None, None, true);
        }
        self.record_equity(ts);
    }

    fn record_equity(&mut self, ts: DateTime<Utc>) {
        let mark_to_market = self.positions.iter().fold(self.cash, |equity, position| {
            equity + Decimal::from(position.shares) * position.latest_bid
        });
        if mark_to_market > self.peak_equity {
            self.peak_equity = mark_to_market;
        }
        if self.peak_equity > Decimal::ZERO {
            let drawdown = (self.peak_equity - mark_to_market) / self.peak_equity;
            if drawdown > self.max_drawdown {
                self.max_drawdown = drawdown;
            }
        }
        self.equity_curve.push((ts, mark_to_market));
    }

    fn build_results(&self) -> CryptoRepricingReplayResults {
        let total_trades = self.closed_trades.len() as u64;
        let winning_trades = self.closed_trades.iter().filter(|trade| trade.won).count() as u64;
        let losing_trades = total_trades.saturating_sub(winning_trades);
        let total_pnl = self
            .closed_trades
            .iter()
            .fold(Decimal::ZERO, |sum, trade| sum + trade.pnl);
        let avg_pnl_per_trade = if total_trades > 0 {
            total_pnl / Decimal::from(total_trades)
        } else {
            Decimal::ZERO
        };
        let avg_holding_secs = if total_trades > 0 {
            self.closed_trades
                .iter()
                .map(|trade| trade.holding_secs as f64)
                .sum::<f64>()
                / total_trades as f64
        } else {
            0.0
        };
        let largest_win = self
            .closed_trades
            .iter()
            .map(|trade| trade.pnl)
            .max()
            .unwrap_or(Decimal::ZERO);
        let largest_loss = self
            .closed_trades
            .iter()
            .map(|trade| trade.pnl)
            .min()
            .unwrap_or(Decimal::ZERO);

        CryptoRepricingReplayResults {
            start_time: self.data_range_start.unwrap_or_else(Utc::now),
            end_time: self.data_range_end.unwrap_or_else(Utc::now),
            total_trades,
            winning_trades,
            losing_trades,
            win_rate: if total_trades > 0 {
                winning_trades as f64 / total_trades as f64
            } else {
                0.0
            },
            total_pnl,
            avg_pnl_per_trade,
            largest_win,
            largest_loss,
            avg_holding_secs,
            max_drawdown: self.max_drawdown,
            trades: self.closed_trades.clone(),
            equity_curve: self.equity_curve.clone(),
        }
    }
}

fn fair_probability_for_side(side: RepricingSide, fair: FairValueEstimate) -> f64 {
    match side {
        RepricingSide::Yes => fair.probability_yes,
        RepricingSide::No => fair.probability_no,
    }
}

fn gap_closed_fraction(entry_gap: Decimal, current_gap: Decimal) -> Decimal {
    if entry_gap <= Decimal::ZERO {
        Decimal::ZERO
    } else {
        ((entry_gap - current_gap) / entry_gap).clamp(Decimal::ZERO, dec!(2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::backtest_feed::{HistoricalFeed, MarketUpdate};
    use std::collections::VecDeque;

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    fn mock_feed(updates: Vec<MarketUpdate>) -> HistoricalFeed {
        HistoricalFeed {
            updates: VecDeque::from(updates),
        }
    }

    fn spot(secs: i64, price: Decimal) -> MarketUpdate {
        MarketUpdate {
            timestamp: ts(secs),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::SpotTrade {
                price,
                quantity: Some(dec!(1)),
            },
        }
    }

    fn l2(secs: i64, obi_5: Decimal, obi_10: Decimal) -> MarketUpdate {
        MarketUpdate {
            timestamp: ts(secs),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::BinanceL2 {
                obi_5,
                obi_10,
                bid_volume_5: dec!(1800),
                ask_volume_5: dec!(200),
                spread_bps: dec!(1),
            },
        }
    }

    fn event_open(secs: i64, end_secs: i64, strike: Decimal) -> MarketUpdate {
        MarketUpdate {
            timestamp: ts(secs),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-5m".into(),
                end_time: Some(ts(end_secs)),
                price_to_beat: Some(strike),
                outcome: None,
            },
        }
    }

    fn quote_update(secs: i64, side: Side, best_bid: Decimal, best_ask: Decimal) -> MarketUpdate {
        let token_side = side.as_str();
        MarketUpdate {
            timestamp: ts(secs),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-5m".into(),
                token_id: format!("btc-5m:{token_side}"),
                side,
                best_bid: Some(best_bid),
                best_ask: Some(best_ask),
            },
        }
    }

    #[test]
    fn replay_exits_on_take_profit_gap_closure() {
        let mut config = CryptoRepricingReplayConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.strategy.min_net_gap_after_cost = dec!(0.01);
        config.max_holding_secs = 300;

        let mut engine = CryptoRepricingReplayEngine::new(config);
        let mut feed = mock_feed(vec![
            event_open(0, 300, dec!(100.10)),
            spot(5, dec!(100.08)),
            spot(20, dec!(100.085)),
            spot(40, dec!(100.09)),
            l2(60, dec!(1.2), dec!(1.0)),
            quote_update(60, Side::Down, dec!(0.65), dec!(0.66)),
            quote_update(60, Side::Up, dec!(0.33), dec!(0.34)),
            spot(120, dec!(100.09)),
            l2(120, dec!(1.0), dec!(0.9)),
            quote_update(120, Side::Up, dec!(0.40), dec!(0.41)),
            quote_update(120, Side::Down, dec!(0.58), dec!(0.59)),
        ]);

        let results = engine.run(&mut feed);
        assert_eq!(results.total_trades, 1);
        assert_eq!(results.trades[0].exit_reason, "take_profit");
        assert_eq!(results.trades[0].side, "YES");
        assert!(results.trades[0].pnl > Decimal::ZERO);
    }

    #[test]
    fn replay_hard_flats_at_t_minus_45() {
        let mut config = CryptoRepricingReplayConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.strategy.min_net_gap_after_cost = dec!(0.01);
        config.max_holding_secs = 600;

        let mut engine = CryptoRepricingReplayEngine::new(config);
        let mut feed = mock_feed(vec![
            event_open(0, 300, dec!(100.10)),
            spot(5, dec!(100.08)),
            spot(20, dec!(100.085)),
            spot(40, dec!(100.09)),
            l2(60, dec!(1.2), dec!(1.0)),
            quote_update(60, Side::Down, dec!(0.65), dec!(0.66)),
            quote_update(60, Side::Up, dec!(0.33), dec!(0.34)),
            spot(256, dec!(100.091)),
            l2(256, dec!(0.8), dec!(0.6)),
            quote_update(256, Side::Up, dec!(0.35), dec!(0.36)),
            quote_update(256, Side::Down, dec!(0.63), dec!(0.64)),
        ]);

        let results = engine.run(&mut feed);
        assert_eq!(results.total_trades, 1);
        assert_eq!(results.trades[0].exit_reason, "hard_flat");
    }

    #[test]
    fn replay_filters_non_five_minute_windows() {
        let mut config = CryptoRepricingReplayConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.strategy.min_net_gap_after_cost = dec!(0.01);

        let mut engine = CryptoRepricingReplayEngine::new(config);
        let mut feed = mock_feed(vec![
            event_open(0, 600, dec!(100.10)),
            spot(5, dec!(100.08)),
            spot(20, dec!(100.085)),
            spot(40, dec!(100.09)),
            l2(60, dec!(1.2), dec!(1.0)),
            quote_update(60, Side::Down, dec!(0.65), dec!(0.66)),
            quote_update(60, Side::Up, dec!(0.33), dec!(0.34)),
        ]);

        let results = engine.run(&mut feed);
        assert_eq!(results.total_trades, 0);
    }
}
