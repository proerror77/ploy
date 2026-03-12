use std::collections::HashMap;

use chrono::{DateTime, Utc};
use futures::executor::block_on;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::domain::{OrderStatus, Quote, Side};
use crate::strategy::backtest::{BacktestResults, BacktestTrade, SymbolStats};
use crate::strategy::backtest_feed::{
    BookAskLevel, MarketFeed, MarketUpdate as HistoricalMarketUpdate, UpdateType,
};
use crate::strategy::backtest_recorder::{
    BacktestRecorder, BacktestSignal, NullRecorder, PendingTrade, SignalType,
};
use crate::strategy::crypto::{horizon_for_series, series_ids_for_symbol};
use crate::strategy::fee_model::FeeModel;
use crate::strategy::pm_5m_directional::Pm5mDirectionalStrategy;
use crate::strategy::runtime_specs::runtime_configs::build_pm_5m_directional_runtime_config;
use crate::strategy::traits::{
    MarketUpdate as StrategyMarketUpdate, OrderUpdate, Strategy, StrategyAction,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pm5mDirectionalBacktestConfig {
    pub symbols: Vec<String>,
    pub initial_capital: Decimal,
}

impl Pm5mDirectionalBacktestConfig {
    pub fn with_symbols(symbols: Vec<String>) -> Self {
        Self {
            symbols,
            initial_capital: Decimal::from(10_000u64),
        }
    }
}

#[derive(Debug, Clone)]
struct FeedEventState {
    event_slug: String,
    symbol: String,
    end_time: Option<DateTime<Utc>>,
    price_to_beat: Option<Decimal>,
    outcome: Option<bool>,
    up_token_id: Option<String>,
    down_token_id: Option<String>,
    discovered: bool,
}

#[derive(Debug, Clone, Copy)]
struct QuoteState {
    best_bid: Option<Decimal>,
    best_ask: Option<Decimal>,
    bid_size: Option<Decimal>,
    ask_size: Option<Decimal>,
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct BookAskState {
    levels: Vec<BookAskLevel>,
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct OpenTrade {
    symbol: String,
    event_slug: String,
    token_id: String,
    side: Side,
    entry_time: DateTime<Utc>,
    entry_price: Decimal,
    entry_fee: Decimal,
    shares: u64,
    entry_p_hat: Option<f64>,
    entry_ev_net: Option<f64>,
    entry_sigma: Option<f64>,
    s0: Option<Decimal>,
}

pub struct Pm5mDirectionalBacktestEngine {
    config: Pm5mDirectionalBacktestConfig,
    strategy: Pm5mDirectionalStrategy,
    recorder: Box<dyn BacktestRecorder>,
    event_states: HashMap<String, FeedEventState>,
    token_to_event: HashMap<String, String>,
    quotes: HashMap<String, QuoteState>,
    book_asks: HashMap<String, BookAskState>,
    latest_spot: HashMap<String, Decimal>,
    open_trades: HashMap<String, OpenTrade>,
    closed_trades: Vec<BacktestTrade>,
    fee_model: FeeModel,
    total_volume: Decimal,
    realized_pnl: Decimal,
    equity_curve: Vec<(DateTime<Utc>, Decimal)>,
}

impl Pm5mDirectionalBacktestEngine {
    pub fn new(
        config: Pm5mDirectionalBacktestConfig,
        recorder: Box<dyn BacktestRecorder>,
    ) -> anyhow::Result<Self> {
        let strategy_config_toml = build_pm_5m_directional_runtime_config(&config.symbols)?;
        let mut config_value: toml::Value = toml::from_str(&strategy_config_toml)?;
        let root = config_value
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("pm_5m_directional runtime config must be a table"))?;
        let strategy_cfg = root
            .entry("pm_5m_directional")
            .or_insert_with(|| toml::Value::Table(Default::default()))
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("[pm_5m_directional] must be a table"))?;
        strategy_cfg.insert(
            "initial_nav_usd".to_string(),
            toml::Value::Float(config.initial_capital.to_f64().unwrap_or(10_000.0)),
        );
        let strategy_config_toml = toml::to_string(&config_value)?;
        let strategy = Pm5mDirectionalStrategy::from_toml(
            "pm_5m_directional_backtest".to_string(),
            &strategy_config_toml,
            true,
        )?;

        Ok(Self {
            config,
            strategy,
            recorder,
            event_states: HashMap::new(),
            token_to_event: HashMap::new(),
            quotes: HashMap::new(),
            book_asks: HashMap::new(),
            latest_spot: HashMap::new(),
            open_trades: HashMap::new(),
            closed_trades: Vec::new(),
            fee_model: FeeModel::crypto(),
            total_volume: Decimal::ZERO,
            realized_pnl: Decimal::ZERO,
            equity_curve: Vec::new(),
        })
    }

    pub fn new_without_recorder(config: Pm5mDirectionalBacktestConfig) -> anyhow::Result<Self> {
        Self::new(config, Box::new(NullRecorder))
    }

    pub fn closed_trades(&self) -> &[BacktestTrade] {
        &self.closed_trades
    }

    pub fn take_recorder(&mut self) -> Box<dyn BacktestRecorder> {
        std::mem::replace(&mut self.recorder, Box::new(NullRecorder))
    }

    pub fn run<F: MarketFeed>(&mut self, feed: &mut F) -> BacktestResults {
        let mut start_time: Option<DateTime<Utc>> = None;
        let mut end_time: Option<DateTime<Utc>> = None;

        while let Some(update) = feed.next_update() {
            start_time.get_or_insert(update.timestamp);
            end_time = Some(update.timestamp);
            self.process_update(update);
        }

        if let Some(final_ts) = end_time {
            self.close_remaining_open_trades(final_ts);
        }

        let start_time = start_time.unwrap_or_else(Utc::now);
        let end_time = end_time.unwrap_or(start_time);
        self.build_results(start_time, end_time)
    }

    fn process_update(&mut self, update: HistoricalMarketUpdate) {
        let ts = update.timestamp;
        match update.update_type {
            UpdateType::SpotTrade { price, .. } => {
                self.latest_spot.insert(update.symbol.clone(), price);
                self.dispatch_market_update(StrategyMarketUpdate::BinancePrice {
                    symbol: update.symbol.clone(),
                    price,
                    timestamp: ts,
                });
            }
            UpdateType::BinanceL2 {
                obi_1,
                obi_2,
                obi_3,
                obi_5,
                obi_10,
                obi_20,
                bid_volume_5,
                ask_volume_5,
                spread_bps,
            } => {
                self.dispatch_market_update(StrategyMarketUpdate::BinanceL2 {
                    symbol: update.symbol.clone(),
                    obi_1,
                    obi_2,
                    obi_3,
                    obi_5,
                    obi_10,
                    obi_20,
                    bid_volume_5,
                    ask_volume_5,
                    spread_bps,
                    timestamp: ts,
                });
            }
            UpdateType::PmQuote {
                event_slug,
                token_id,
                side,
                best_bid,
                best_ask,
                bid_size,
                ask_size,
            } => {
                {
                    let state = self
                        .event_states
                        .entry(event_slug.clone())
                        .or_insert_with(|| FeedEventState {
                            event_slug: event_slug.clone(),
                            symbol: update.symbol.clone(),
                            end_time: None,
                            price_to_beat: None,
                            outcome: None,
                            up_token_id: None,
                            down_token_id: None,
                            discovered: false,
                        });
                    match side {
                        Side::Up => state.up_token_id = Some(token_id.clone()),
                        Side::Down => state.down_token_id = Some(token_id.clone()),
                    }
                }
                self.token_to_event
                    .insert(token_id.clone(), event_slug.clone());

                let merged_quote = {
                    let quote = self
                        .quotes
                        .entry(token_id.clone())
                        .or_insert(QuoteState {
                            best_bid: None,
                            best_ask: None,
                            bid_size: None,
                            ask_size: None,
                            timestamp: ts,
                        });
                    quote.best_bid = best_bid;
                    quote.best_ask = best_ask;
                    if bid_size.is_some() {
                        quote.bid_size = bid_size;
                    }
                    if ask_size.is_some() {
                        quote.ask_size = ask_size;
                    }
                    quote.timestamp = ts;
                    Quote {
                        side,
                        best_bid: quote.best_bid,
                        best_ask: quote.best_ask,
                        bid_size: quote.bid_size,
                        ask_size: quote.ask_size,
                        timestamp: ts,
                    }
                };
                if matches!(self.book_asks.get(&token_id), Some(book) if book.timestamp < ts) {
                    self.book_asks.remove(&token_id);
                }

                self.maybe_discover_event(&event_slug);
                self.dispatch_market_update(StrategyMarketUpdate::PolymarketQuote {
                    token_id,
                    side,
                    quote: merged_quote,
                    timestamp: ts,
                });
            }
            UpdateType::LobSnapshot {
                event_slug,
                token_id,
                side,
                ask_depth_shares,
                best_ask_size_shares,
                ask_levels,
                best_ask,
            } => {
                self.token_to_event
                    .insert(token_id.clone(), event_slug.clone());
                self.book_asks.insert(
                    token_id.clone(),
                    BookAskState {
                        levels: ask_levels.clone(),
                        timestamp: ts,
                    },
                );
                let merged_quote = {
                    let quote = self
                        .quotes
                        .entry(token_id.clone())
                        .or_insert(QuoteState {
                            best_bid: None,
                            best_ask: None,
                            bid_size: None,
                            ask_size: None,
                            timestamp: ts,
                        });
                    let best_level_size =
                        Decimal::from(best_ask_size_shares.min(ask_depth_shares));
                    quote.ask_size = Some(best_level_size);
                    if best_ask.is_some() {
                        quote.best_ask = best_ask;
                    }
                    quote.timestamp = ts;
                    Quote {
                        side,
                        best_bid: quote.best_bid,
                        best_ask: quote.best_ask,
                        bid_size: quote.bid_size,
                        ask_size: quote.ask_size,
                        timestamp: ts,
                    }
                };
                self.maybe_discover_event(&event_slug);
                self.dispatch_market_update(StrategyMarketUpdate::PolymarketQuote {
                    token_id,
                    side,
                    quote: merged_quote,
                    timestamp: ts,
                });
            }
            UpdateType::EventState {
                event_slug,
                end_time,
                price_to_beat,
                outcome,
            } => {
                {
                    let state = self
                        .event_states
                        .entry(event_slug.clone())
                        .or_insert_with(|| FeedEventState {
                            event_slug: event_slug.clone(),
                            symbol: update.symbol.clone(),
                            end_time: None,
                            price_to_beat: None,
                            outcome: None,
                            up_token_id: None,
                            down_token_id: None,
                            discovered: false,
                        });
                    if end_time.is_some() {
                        state.end_time = end_time;
                    }
                    if price_to_beat.is_some() {
                        state.price_to_beat = price_to_beat;
                    }
                    if outcome.is_some() {
                        state.outcome = outcome;
                    }
                }

                self.maybe_discover_event(&event_slug);
                if let Some(outcome) = outcome {
                    let settle_ts = self
                        .event_states
                        .get(&event_slug)
                        .and_then(|state| state.end_time)
                        .unwrap_or(ts);
                    self.publish_settlement_quotes(&event_slug, outcome, settle_ts);
                    self.dispatch_market_update(StrategyMarketUpdate::EventExpired {
                        event_id: event_slug.clone(),
                    });
                    self.settle_event(&event_slug, outcome, settle_ts);
                    self.event_states.remove(&event_slug);
                }
            }
        }

        let _ = block_on(self.strategy.on_tick(ts));
        self.record_equity_point(ts);
    }

    fn dispatch_market_update(&mut self, update: StrategyMarketUpdate) {
        if let Ok(actions) = block_on(self.strategy.on_market_update(&update)) {
            self.handle_actions(actions, update.timestamp());
        }
    }

    fn publish_settlement_quotes(
        &mut self,
        event_slug: &str,
        outcome_up: bool,
        timestamp: DateTime<Utc>,
    ) {
        let Some(state) = self.event_states.get(event_slug).cloned() else {
            return;
        };
        let payouts = [
            (state.up_token_id, Side::Up, if outcome_up { Decimal::ONE } else { Decimal::ZERO }),
            (
                state.down_token_id,
                Side::Down,
                if outcome_up { Decimal::ZERO } else { Decimal::ONE },
            ),
        ];
        for (token_id, side, payout) in payouts {
            let Some(token_id) = token_id else {
                continue;
            };
            self.dispatch_market_update(StrategyMarketUpdate::PolymarketQuote {
                token_id,
                side,
                quote: Quote {
                    side,
                    best_bid: Some(payout),
                    best_ask: Some(payout),
                    bid_size: Some(Decimal::ONE),
                    ask_size: Some(Decimal::ONE),
                    timestamp,
                },
                timestamp,
            });
        }
    }

    fn maybe_discover_event(&mut self, event_slug: &str) {
        let Some(state) = self.event_states.get(event_slug) else {
            return;
        };
        if state.discovered {
            return;
        }
        let (Some(end_time), Some(price_to_beat), Some(up_token_id), Some(down_token_id)) = (
            state.end_time,
            state.price_to_beat,
            state.up_token_id.clone(),
            state.down_token_id.clone(),
        ) else {
            return;
        };
        let series_id = series_ids_for_symbol(&state.symbol)
            .into_iter()
            .find(|series_id| horizon_for_series(series_id) == "5m")
            .unwrap_or_else(|| "10684".to_string());
        let event_id = state.event_slug.clone();
        let title = state.event_slug.clone();
        if let Some(state_mut) = self.event_states.get_mut(event_slug) {
            state_mut.discovered = true;
        }
        self.dispatch_market_update(StrategyMarketUpdate::EventDiscovered {
            event_id,
            series_id,
            up_token: up_token_id,
            down_token: down_token_id,
            end_time,
            price_to_beat: Some(price_to_beat),
            title: Some(title),
            condition_id: None,
        });
    }

    fn handle_actions(&mut self, actions: Vec<StrategyAction>, timestamp: DateTime<Utc>) {
        for action in actions {
            if let StrategyAction::SubmitIntent { intent } = action {
                self.record_entry_signal(&intent, timestamp);
                self.simulate_ioc_fill(intent, timestamp);
            }
        }
    }

    fn record_entry_signal(
        &mut self,
        intent: &crate::strategy::traits::StrategyOrderIntent,
        timestamp: DateTime<Utc>,
    ) {
        let event_slug = intent
            .metadata
            .get("event_id")
            .cloned()
            .or_else(|| self.token_to_event.get(&intent.token_id).cloned());
        let time_remaining_secs = event_slug
            .as_deref()
            .and_then(|event| self.event_states.get(event))
            .and_then(|state| state.end_time)
            .map(|end_time| (end_time - timestamp).num_seconds().max(0) as f64);

        self.recorder.record_entry(&BacktestSignal {
            signal_type: SignalType::Entry,
            symbol: intent.market_slug.clone(),
            direction: intent.side.as_str().to_string(),
            timestamp,
            p_hat: parse_f64(intent.metadata.get("p_hat")),
            ev_net: parse_f64(intent.metadata.get("edge")),
            sigma: parse_f64(intent.metadata.get("sigma")),
            market_price: Some(intent.limit_price),
            spot_price: self.latest_spot.get(&intent.market_slug).copied(),
            s0: event_slug
                .as_deref()
                .and_then(|event| self.event_states.get(event))
                .and_then(|state| state.price_to_beat),
            time_remaining_secs,
            filter_reason: None,
            exit_reason: None,
            exit_price: None,
        });
    }

    fn simulate_ioc_fill(
        &mut self,
        intent: crate::strategy::traits::StrategyOrderIntent,
        timestamp: DateTime<Utc>,
    ) {
        let client_order_id = intent.client_order_id.clone();
        let token_id = intent.token_id.clone();
        let symbol = intent.market_slug.clone();
        let side = intent.side;
        let shares = intent.shares;
        let limit_price = intent.limit_price;
        let metadata = intent.metadata.clone();
        let current_quote = self.quotes.get(&token_id).copied();
        let order_id = client_order_id.clone();

        let Some(quote) = current_quote else {
            self.send_order_update(OrderUpdate {
                order_id,
                client_order_id: Some(client_order_id),
                status: OrderStatus::Rejected,
                filled_qty: 0,
                avg_fill_price: None,
                timestamp,
                error: Some("missing_quote".to_string()),
            });
            return;
        };

        let Some(best_ask) = quote.best_ask else {
            self.send_order_update(OrderUpdate {
                order_id,
                client_order_id: Some(client_order_id),
                status: OrderStatus::Rejected,
                filled_qty: 0,
                avg_fill_price: None,
                timestamp,
                error: Some("missing_best_ask".to_string()),
            });
            return;
        };

        if best_ask > limit_price {
            self.send_order_update(OrderUpdate {
                order_id,
                client_order_id: Some(client_order_id),
                status: OrderStatus::Expired,
                filled_qty: 0,
                avg_fill_price: None,
                timestamp,
                error: Some("ioc_limit_not_crossed".to_string()),
            });
            return;
        }

        let fill = self.simulate_replay_fill(&token_id, &quote, shares, limit_price);
        let filled_qty = fill.filled_qty;

        if filled_qty == 0 {
            self.send_order_update(OrderUpdate {
                order_id,
                client_order_id: Some(client_order_id),
                status: OrderStatus::Expired,
                filled_qty: 0,
                avg_fill_price: None,
                timestamp,
                error: Some("ioc_no_liquidity".to_string()),
            });
            return;
        }

        let event_slug = metadata
            .get("event_id")
            .cloned()
            .or_else(|| self.token_to_event.get(&token_id).cloned())
            .unwrap_or_else(|| symbol.clone());
        self.open_trades.insert(
            token_id.clone(),
            OpenTrade {
                symbol: symbol.clone(),
                event_slug,
                token_id: token_id.clone(),
                side,
                entry_time: timestamp,
                entry_price: fill.avg_fill_price,
                entry_fee: fill.fee_paid,
                shares: filled_qty,
                entry_p_hat: parse_f64(metadata.get("effective_p").or_else(|| metadata.get("p_hat"))),
                entry_ev_net: parse_f64(metadata.get("edge")),
                entry_sigma: parse_f64(metadata.get("sigma")),
                s0: metadata
                    .get("event_id")
                    .and_then(|event| self.event_states.get(event))
                    .and_then(|state| state.price_to_beat),
            },
        );
        self.total_volume += fill.avg_fill_price * Decimal::from(filled_qty);

        if filled_qty < shares {
            self.send_order_update(OrderUpdate {
                order_id: order_id.clone(),
                client_order_id: Some(client_order_id.clone()),
                status: OrderStatus::PartiallyFilled,
                filled_qty,
                avg_fill_price: Some(fill.avg_fill_price),
                timestamp,
                error: None,
            });
            self.send_order_update(OrderUpdate {
                order_id,
                client_order_id: Some(client_order_id),
                status: OrderStatus::Expired,
                filled_qty,
                avg_fill_price: Some(fill.avg_fill_price),
                timestamp,
                error: None,
            });
        } else {
            self.send_order_update(OrderUpdate {
                order_id,
                client_order_id: Some(client_order_id),
                status: OrderStatus::Filled,
                filled_qty,
                avg_fill_price: Some(fill.avg_fill_price),
                timestamp,
                error: None,
            });
        }
    }

    fn send_order_update(&mut self, update: OrderUpdate) {
        if let Ok(actions) = block_on(self.strategy.on_order_update(&update)) {
            self.handle_actions(actions, update.timestamp);
        }
    }

    fn settle_event(&mut self, event_slug: &str, outcome_up: bool, timestamp: DateTime<Utc>) {
        let winning_side = if outcome_up { Side::Up } else { Side::Down };
        let token_ids: Vec<String> = self
            .open_trades
            .values()
            .filter(|trade| trade.event_slug == event_slug)
            .map(|trade| trade.token_id.clone())
            .collect();

        for token_id in token_ids {
            let Some(trade) = self.open_trades.remove(&token_id) else {
                continue;
            };
            let exit_price = if trade.side == winning_side {
                Decimal::ONE
            } else {
                Decimal::ZERO
            };
            let (pnl, pnl_pct) = self.realized_trade_pnl(&trade, exit_price);
            self.realized_pnl += pnl;
            self.recorder.record_exit(&BacktestSignal {
                signal_type: SignalType::Exit,
                symbol: trade.symbol.clone(),
                direction: trade.side.as_str().to_string(),
                timestamp,
                p_hat: trade.entry_p_hat,
                ev_net: trade.entry_ev_net,
                sigma: trade.entry_sigma,
                market_price: Some(exit_price),
                spot_price: self.latest_spot.get(&trade.symbol).copied(),
                s0: trade.s0,
                time_remaining_secs: Some(0.0),
                filter_reason: None,
                exit_reason: Some("settlement".to_string()),
                exit_price: Some(exit_price),
            });
            self.recorder.record_trade(&PendingTrade {
                symbol: trade.symbol.clone(),
                direction: trade.side.as_str().to_string(),
                entry_time: trade.entry_time,
                exit_time: timestamp,
                entry_price: trade.entry_price,
                exit_price,
                shares: i32::try_from(trade.shares).unwrap_or(i32::MAX),
                pnl,
                won: trade.side == winning_side,
                holding_secs: (timestamp - trade.entry_time).num_seconds(),
                exit_reason: "settlement".to_string(),
                entry_p_hat: trade.entry_p_hat,
                entry_ev_net: trade.entry_ev_net,
                entry_sigma: trade.entry_sigma,
                s0: trade.s0,
            });

            self.closed_trades.push(BacktestTrade {
                entry_time: trade.entry_time,
                exit_time: timestamp,
                symbol: trade.symbol.clone(),
                market_id: trade.event_slug.clone(),
                direction: trade.side.as_str().to_string(),
                entry_price: trade.entry_price,
                exit_price,
                shares: trade.shares,
                pnl,
                pnl_pct,
                won: trade.side == winning_side,
                fair_value: Decimal::from_f64(trade.entry_p_hat.unwrap_or(0.0))
                    .unwrap_or(Decimal::ZERO),
                price_edge: Decimal::from_f64(trade.entry_ev_net.unwrap_or(0.0))
                    .unwrap_or(Decimal::ZERO),
                vol_edge_pct: 0.0,
                confidence: trade.entry_p_hat.unwrap_or(0.0),
                buffer_pct: Decimal::from_f64(trade.entry_ev_net.unwrap_or(0.0))
                    .unwrap_or(Decimal::ZERO),
                our_volatility: trade.entry_sigma.unwrap_or(0.0),
                implied_volatility: 0.0,
            });
        }
        self.record_equity_point(timestamp);
    }

    fn close_remaining_open_trades(&mut self, timestamp: DateTime<Utc>) {
        let token_ids: Vec<String> = self.open_trades.keys().cloned().collect();
        for token_id in token_ids {
            let Some(trade) = self.open_trades.remove(&token_id) else {
                continue;
            };
            let exit_price = self.executable_exit_price(&token_id, trade.entry_price);
            let (pnl, pnl_pct) = self.realized_trade_pnl(&trade, exit_price);
            self.realized_pnl += pnl;
            self.recorder.record_exit(&BacktestSignal {
                signal_type: SignalType::Exit,
                symbol: trade.symbol.clone(),
                direction: trade.side.as_str().to_string(),
                timestamp,
                p_hat: trade.entry_p_hat,
                ev_net: trade.entry_ev_net,
                sigma: trade.entry_sigma,
                market_price: Some(exit_price),
                spot_price: self.latest_spot.get(&trade.symbol).copied(),
                s0: trade.s0,
                time_remaining_secs: Some(0.0),
                filter_reason: None,
                exit_reason: Some("backtest_end_mark".to_string()),
                exit_price: Some(exit_price),
            });
            self.recorder.record_trade(&PendingTrade {
                symbol: trade.symbol.clone(),
                direction: trade.side.as_str().to_string(),
                entry_time: trade.entry_time,
                exit_time: timestamp,
                entry_price: trade.entry_price,
                exit_price,
                shares: i32::try_from(trade.shares).unwrap_or(i32::MAX),
                pnl,
                won: pnl >= Decimal::ZERO,
                holding_secs: (timestamp - trade.entry_time).num_seconds(),
                exit_reason: "backtest_end_mark".to_string(),
                entry_p_hat: trade.entry_p_hat,
                entry_ev_net: trade.entry_ev_net,
                entry_sigma: trade.entry_sigma,
                s0: trade.s0,
            });

            self.closed_trades.push(BacktestTrade {
                entry_time: trade.entry_time,
                exit_time: timestamp,
                symbol: trade.symbol.clone(),
                market_id: trade.event_slug.clone(),
                direction: trade.side.as_str().to_string(),
                entry_price: trade.entry_price,
                exit_price,
                shares: trade.shares,
                pnl,
                pnl_pct,
                won: pnl >= Decimal::ZERO,
                fair_value: Decimal::from_f64(trade.entry_p_hat.unwrap_or(0.0))
                    .unwrap_or(Decimal::ZERO),
                price_edge: Decimal::from_f64(trade.entry_ev_net.unwrap_or(0.0))
                    .unwrap_or(Decimal::ZERO),
                vol_edge_pct: 0.0,
                confidence: trade.entry_p_hat.unwrap_or(0.0),
                buffer_pct: Decimal::from_f64(trade.entry_ev_net.unwrap_or(0.0))
                    .unwrap_or(Decimal::ZERO),
                our_volatility: trade.entry_sigma.unwrap_or(0.0),
                implied_volatility: 0.0,
            });
        }
        self.record_equity_point(timestamp);
    }

    fn fill_fee(&self, shares: u64, price: Decimal) -> Decimal {
        self.fee_model.fee_shares(Decimal::from(shares), price)
    }

    fn simulate_replay_fill(
        &mut self,
        token_id: &str,
        quote: &QuoteState,
        requested_shares: u64,
        limit_price: Decimal,
    ) -> SimulatedFill {
        let use_book = self
            .book_asks
            .get(token_id)
            .map(|book| book.timestamp >= quote.timestamp)
            .unwrap_or(false);
        if !use_book {
            self.book_asks.remove(token_id);
        }

        let fee_model = self.fee_model.clone();
        if let Some(book) = self.book_asks.get_mut(token_id) {
            let mut remaining = requested_shares;
            let mut filled_qty = 0u64;
            let mut notional = Decimal::ZERO;
            let mut fee_paid = Decimal::ZERO;

            for level in &mut book.levels {
                if level.price > limit_price || remaining == 0 {
                    break;
                }
                let take = remaining.min(level.size_shares);
                if take == 0 {
                    continue;
                }
                let take_dec = Decimal::from(take);
                notional += level.price * take_dec;
                fee_paid += fee_model.fee_shares(Decimal::from(take), level.price);
                filled_qty += take;
                remaining -= take;
                level.size_shares -= take;
            }
            book.levels.retain(|level| level.size_shares > 0);

            let next_best = book.levels.first().copied();
            let book_depleted = book.levels.is_empty();
            if let Some(quote_state) = self.quotes.get_mut(token_id) {
                quote_state.best_ask = next_best.map(|level| level.price);
                quote_state.ask_size =
                    Some(Decimal::from(next_best.map(|level| level.size_shares).unwrap_or(0u64)));
            }
            if book_depleted {
                self.book_asks.remove(token_id);
            }

            if filled_qty > 0 {
                return SimulatedFill {
                    filled_qty,
                    avg_fill_price: notional / Decimal::from(filled_qty),
                    fee_paid,
                };
            }
        }

        let available = quote
            .ask_size
            .unwrap_or(Decimal::ZERO)
            .floor()
            .to_u64()
            .unwrap_or(0);
        let filled_qty = requested_shares.min(available);
        let avg_fill_price = quote.best_ask.unwrap_or(limit_price);
        let remaining = available.saturating_sub(filled_qty);
        if let Some(quote_state) = self.quotes.get_mut(token_id) {
            quote_state.ask_size = Some(Decimal::from(remaining));
            if remaining == 0 {
                quote_state.best_ask = None;
            }
        }
        SimulatedFill {
            filled_qty,
            avg_fill_price,
            fee_paid: self.fill_fee(filled_qty, avg_fill_price),
        }
    }

    fn realized_trade_pnl(&self, trade: &OpenTrade, exit_price: Decimal) -> (Decimal, Decimal) {
        let shares = Decimal::from(trade.shares);
        let exit_fee = self.fill_fee(trade.shares, exit_price);
        let entry_cost = trade.entry_price * shares + trade.entry_fee;
        let exit_value = exit_price * shares - exit_fee;
        let pnl = exit_value - entry_cost;
        let pnl_pct = if entry_cost > Decimal::ZERO {
            pnl / entry_cost
        } else {
            Decimal::ZERO
        };
        (pnl, pnl_pct)
    }

    fn unrealized_trade_pnl(&self, trade: &OpenTrade) -> Decimal {
        let exit_price = self.executable_exit_price(&trade.token_id, trade.entry_price);
        self.realized_trade_pnl(trade, exit_price).0
    }

    fn executable_exit_price(&self, token_id: &str, fallback: Decimal) -> Decimal {
        self.quotes
            .get(token_id)
            .map(|quote| quote.best_bid.unwrap_or(Decimal::ZERO))
            .unwrap_or(fallback)
    }

    fn current_equity(&self) -> Decimal {
        let unrealized_open_pnl: Decimal = self
            .open_trades
            .values()
            .map(|trade| self.unrealized_trade_pnl(trade))
            .sum();
        self.config.initial_capital + self.realized_pnl + unrealized_open_pnl
    }

    fn record_equity_point(&mut self, timestamp: DateTime<Utc>) {
        let equity = self.current_equity();
        match self.equity_curve.last_mut() {
            Some((last_ts, last_equity)) if *last_ts == timestamp => {
                *last_equity = equity;
            }
            Some((_, last_equity)) if *last_equity == equity => {}
            _ => self.equity_curve.push((timestamp, equity)),
        }
    }

    fn build_results(&self, start_time: DateTime<Utc>, end_time: DateTime<Utc>) -> BacktestResults {
        let total_trades = self.closed_trades.len() as u64;
        let winning_trades = self.closed_trades.iter().filter(|trade| trade.won).count() as u64;
        let losing_trades = total_trades.saturating_sub(winning_trades);
        let total_pnl: Decimal = self.closed_trades.iter().map(|trade| trade.pnl).sum();
        let avg_pnl_per_trade = if total_trades > 0 {
            total_pnl / Decimal::from(total_trades)
        } else {
            Decimal::ZERO
        };

        let mut trades_by_symbol: HashMap<String, SymbolStats> = HashMap::new();
        let mut largest_win = Decimal::ZERO;
        let mut largest_loss = Decimal::ZERO;
        let mut total_holding_secs = 0i64;
        let mut gross_profit = Decimal::ZERO;
        let mut gross_loss = Decimal::ZERO;
        let mut win_sum = Decimal::ZERO;
        let mut loss_sum = Decimal::ZERO;

        for trade in &self.closed_trades {
            total_holding_secs += (trade.exit_time - trade.entry_time).num_seconds();
            largest_win = largest_win.max(trade.pnl);
            largest_loss = largest_loss.min(trade.pnl);
            if trade.pnl > Decimal::ZERO {
                gross_profit += trade.pnl;
                win_sum += trade.pnl;
            } else if trade.pnl < Decimal::ZERO {
                gross_loss += -trade.pnl;
                loss_sum += trade.pnl;
            }

            let stats = trades_by_symbol
                .entry(trade.symbol.clone())
                .or_insert(SymbolStats {
                    total_trades: 0,
                    winning_trades: 0,
                    win_rate: 0.0,
                    total_pnl: Decimal::ZERO,
                });
            stats.total_trades += 1;
            if trade.won {
                stats.winning_trades += 1;
            }
            stats.total_pnl += trade.pnl;
            stats.win_rate = if stats.total_trades > 0 {
                stats.winning_trades as f64 / stats.total_trades as f64
            } else {
                0.0
            };
        }

        let win_count = winning_trades.max(1);
        let loss_count = losing_trades.max(1);
        let avg_win = if winning_trades > 0 {
            win_sum / Decimal::from(win_count)
        } else {
            Decimal::ZERO
        };
        let avg_loss = if losing_trades > 0 {
            loss_sum / Decimal::from(loss_count)
        } else {
            Decimal::ZERO
        };
        let profit_factor = if gross_loss > Decimal::ZERO {
            (gross_profit / gross_loss).to_f64().unwrap_or(0.0)
        } else if gross_profit > Decimal::ZERO {
            f64::INFINITY
        } else {
            0.0
        };
        let avg_holding_time_secs = if total_trades > 0 {
            total_holding_secs as f64 / total_trades as f64
        } else {
            0.0
        };

        BacktestResults {
            start_time,
            end_time,
            total_trades,
            winning_trades,
            losing_trades,
            win_rate: if total_trades > 0 {
                winning_trades as f64 / total_trades as f64
            } else {
                0.0
            },
            total_pnl,
            total_volume: self.total_volume,
            avg_pnl_per_trade,
            max_drawdown: calculate_max_drawdown(&self.equity_curve, self.config.initial_capital),
            sharpe_ratio: calculate_equity_sharpe(&self.equity_curve),
            profit_factor,
            avg_win,
            avg_loss,
            largest_win,
            largest_loss,
            avg_holding_time_secs,
            trades_by_symbol,
            trades: self.closed_trades.clone(),
            equity_curve: if self.equity_curve.is_empty() {
                vec![(end_time, self.config.initial_capital + total_pnl)]
            } else {
                self.equity_curve.clone()
            },
        }
    }
}

fn parse_f64(raw: Option<&String>) -> Option<f64> {
    raw.and_then(|value| value.parse::<f64>().ok())
}

fn calculate_equity_sharpe(equity_curve: &[(DateTime<Utc>, Decimal)]) -> f64 {
    const SHARPE_SAMPLE_SECS: i64 = 60;

    if equity_curve.len() < 2 {
        return 0.0;
    }

    let mut sampled_equity = Vec::new();
    let mut sample_ts = equity_curve[0].0;
    let end_ts = equity_curve[equity_curve.len() - 1].0;
    let mut idx = 0usize;
    let mut current_equity = equity_curve[0].1;

    while sample_ts <= end_ts {
        while idx + 1 < equity_curve.len() && equity_curve[idx + 1].0 <= sample_ts {
            idx += 1;
            current_equity = equity_curve[idx].1;
        }
        sampled_equity.push(current_equity);
        sample_ts += chrono::Duration::seconds(SHARPE_SAMPLE_SECS);
    }
    if sampled_equity.last().copied() != Some(current_equity) {
        sampled_equity.push(current_equity);
    }

    let mut returns = Vec::new();
    for window in sampled_equity.windows(2) {
        let prev_equity = window[0];
        let next_equity = window[1];
        if prev_equity <= Decimal::ZERO || next_equity <= Decimal::ZERO {
            continue;
        }
        let Some(prev_value) = prev_equity.to_f64() else {
            continue;
        };
        let Some(next_value) = next_equity.to_f64() else {
            continue;
        };
        returns.push((next_value / prev_value) - 1.0);
    }

    if returns.len() < 2 {
        return 0.0;
    }

    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance =
        returns.iter().map(|ret| (ret - mean).powi(2)).sum::<f64>() / (returns.len() as f64 - 1.0);
    if variance <= f64::EPSILON {
        return 0.0;
    }
    let std_dev = variance.sqrt();
    if std_dev <= f64::EPSILON {
        return 0.0;
    }
    let periods_per_year = (365.0 * 24.0 * 60.0 * 60.0) / SHARPE_SAMPLE_SECS as f64;
    (mean / std_dev) * periods_per_year.sqrt()
}

fn calculate_max_drawdown(
    equity_curve: &[(DateTime<Utc>, Decimal)],
    initial_capital: Decimal,
) -> Decimal {
    let mut peak = initial_capital;
    let mut max_drawdown = Decimal::ZERO;
    for (_, equity) in equity_curve {
        if *equity > peak {
            peak = *equity;
        }
        if peak > Decimal::ZERO {
            let drawdown = (peak - *equity) / peak;
            if drawdown > max_drawdown {
                max_drawdown = drawdown;
            }
        }
    }
    max_drawdown
}

#[derive(Debug, Clone, Copy)]
struct SimulatedFill {
    filled_qty: u64,
    avg_fill_price: Decimal,
    fee_paid: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::backtest_feed::{HistoricalFeed, MarketUpdate, UpdateType};
    use rust_decimal_macros::dec;
    use std::collections::VecDeque;

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_100_000 + secs, 0).unwrap()
    }

    fn mock_feed(updates: Vec<MarketUpdate>) -> HistoricalFeed {
        HistoricalFeed {
            updates: VecDeque::from(updates),
        }
    }

    #[test]
    fn replays_profitable_up_round_to_settlement() {
        let config = Pm5mDirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".to_string()]);
        let mut engine =
            Pm5mDirectionalBacktestEngine::new_without_recorder(config).expect("engine");

        let event_slug = "btc-updown-5m-test";
        let end_time = ts(300);
        let mut updates = vec![MarketUpdate {
            timestamp: ts(0),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::EventState {
                event_slug: event_slug.to_string(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        }];

        for i in 1..=35 {
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".to_string(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100) + Decimal::from(i) * dec!(0.05),
                    quantity: Some(dec!(1)),
                },
            });
        }

        updates.push(MarketUpdate {
            timestamp: ts(34),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::BinanceL2 {
                obi_1: dec!(0.38),
                obi_2: dec!(0.36),
                obi_3: dec!(0.35),
                obi_5: dec!(0.35),
                obi_10: dec!(0.28),
                obi_20: dec!(0.22),
                bid_volume_5: dec!(200),
                ask_volume_5: dec!(100),
                spread_bps: dec!(1),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(35),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: event_slug.to_string(),
                token_id: format!("{event_slug}:UP"),
                side: Side::Up,
                best_bid: Some(dec!(0.39)),
                best_ask: Some(dec!(0.40)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(35),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: event_slug.to_string(),
                token_id: format!("{event_slug}:DOWN"),
                side: Side::Down,
                best_bid: Some(dec!(0.59)),
                best_ask: Some(dec!(0.60)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(35),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::LobSnapshot {
                event_slug: event_slug.to_string(),
                token_id: format!("{event_slug}:UP"),
                side: Side::Up,
                ask_depth_shares: 100,
                best_ask_size_shares: 100,
                ask_levels: vec![BookAskLevel {
                    price: dec!(0.40),
                    size_shares: 100,
                }],
                best_ask: Some(dec!(0.40)),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(36),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::SpotTrade {
                price: dec!(101.90),
                quantity: Some(dec!(1)),
            },
        });
        updates.push(MarketUpdate {
            timestamp: end_time,
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::EventState {
                event_slug: event_slug.to_string(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: Some(true),
            },
        });

        let mut feed = mock_feed(updates);
        let results = engine.run(&mut feed);

        assert_eq!(results.total_trades, 1);
        assert_eq!(results.winning_trades, 1);
        assert!(results.total_pnl > Decimal::ZERO);

        let trades = engine.closed_trades();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].direction, "UP");
        assert_eq!(trades[0].exit_price, Decimal::ONE);
        assert!(trades[0].won);
        let expected_fee = FeeModel::crypto().fee_shares(Decimal::from(trades[0].shares), dec!(0.40));
        let expected_pnl =
            (Decimal::ONE - dec!(0.40)) * Decimal::from(trades[0].shares) - expected_fee;
        assert_eq!(trades[0].pnl, expected_pnl);
    }

    #[test]
    fn low_initial_capital_blocks_trades_below_min_shares() {
        let mut config = Pm5mDirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".to_string()]);
        config.initial_capital = dec!(100);
        let mut engine =
            Pm5mDirectionalBacktestEngine::new_without_recorder(config).expect("engine");

        let event_slug = "btc-updown-5m-low-capital";
        let end_time = ts(300);
        let mut updates = vec![MarketUpdate {
            timestamp: ts(0),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::EventState {
                event_slug: event_slug.to_string(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        }];

        for i in 1..=35 {
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".to_string(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100) + Decimal::from(i) * dec!(0.05),
                    quantity: Some(dec!(1)),
                },
            });
        }

        updates.push(MarketUpdate {
            timestamp: ts(34),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::BinanceL2 {
                obi_1: dec!(0.38),
                obi_2: dec!(0.36),
                obi_3: dec!(0.35),
                obi_5: dec!(0.35),
                obi_10: dec!(0.28),
                obi_20: dec!(0.22),
                bid_volume_5: dec!(200),
                ask_volume_5: dec!(100),
                spread_bps: dec!(1),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(35),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: event_slug.to_string(),
                token_id: format!("{event_slug}:UP"),
                side: Side::Up,
                best_bid: Some(dec!(0.39)),
                best_ask: Some(dec!(0.40)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(35),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: event_slug.to_string(),
                token_id: format!("{event_slug}:DOWN"),
                side: Side::Down,
                best_bid: Some(dec!(0.59)),
                best_ask: Some(dec!(0.60)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(35),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::LobSnapshot {
                event_slug: event_slug.to_string(),
                token_id: format!("{event_slug}:UP"),
                side: Side::Up,
                ask_depth_shares: 100,
                best_ask_size_shares: 100,
                ask_levels: vec![BookAskLevel {
                    price: dec!(0.40),
                    size_shares: 100,
                }],
                best_ask: Some(dec!(0.40)),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(36),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::SpotTrade {
                price: dec!(101.90),
                quantity: Some(dec!(1)),
            },
        });
        updates.push(MarketUpdate {
            timestamp: end_time,
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::EventState {
                event_slug: event_slug.to_string(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: Some(true),
            },
        });

        let mut feed = mock_feed(updates);
        let results = engine.run(&mut feed);

        assert_eq!(results.total_trades, 0);
        assert!(engine.closed_trades().is_empty());
    }

    #[test]
    fn settlement_uses_event_end_time_not_resolved_time() {
        let config = Pm5mDirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".to_string()]);
        let mut engine =
            Pm5mDirectionalBacktestEngine::new_without_recorder(config).expect("engine");

        let event_slug = "btc-updown-5m-endtime";
        let end_time = ts(300);
        let mut updates = vec![MarketUpdate {
            timestamp: ts(0),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::EventState {
                event_slug: event_slug.to_string(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        }];

        for i in 1..=35 {
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".to_string(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100) + Decimal::from(i) * dec!(0.05),
                    quantity: Some(dec!(1)),
                },
            });
        }

        updates.push(MarketUpdate {
            timestamp: ts(34),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::BinanceL2 {
                obi_1: dec!(0.38),
                obi_2: dec!(0.36),
                obi_3: dec!(0.35),
                obi_5: dec!(0.35),
                obi_10: dec!(0.28),
                obi_20: dec!(0.22),
                bid_volume_5: dec!(200),
                ask_volume_5: dec!(100),
                spread_bps: dec!(1),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(35),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: event_slug.to_string(),
                token_id: format!("{event_slug}:UP"),
                side: Side::Up,
                best_bid: Some(dec!(0.39)),
                best_ask: Some(dec!(0.40)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(35),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: event_slug.to_string(),
                token_id: format!("{event_slug}:DOWN"),
                side: Side::Down,
                best_bid: Some(dec!(0.59)),
                best_ask: Some(dec!(0.60)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(35),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::LobSnapshot {
                event_slug: event_slug.to_string(),
                token_id: format!("{event_slug}:UP"),
                side: Side::Up,
                ask_depth_shares: 100,
                best_ask_size_shares: 100,
                ask_levels: vec![BookAskLevel {
                    price: dec!(0.40),
                    size_shares: 100,
                }],
                best_ask: Some(dec!(0.40)),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(36),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::SpotTrade {
                price: dec!(101.90),
                quantity: Some(dec!(1)),
            },
        });
        updates.push(MarketUpdate {
            timestamp: ts(900),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::EventState {
                event_slug: event_slug.to_string(),
                end_time: None,
                price_to_beat: None,
                outcome: Some(true),
            },
        });

        let mut feed = mock_feed(updates);
        let results = engine.run(&mut feed);

        assert_eq!(results.total_trades, 1);
        assert_eq!(results.trades[0].exit_time, end_time);
        assert_eq!(results.equity_curve.last().map(|point| point.0), Some(end_time));
    }

    #[test]
    fn lob_snapshot_routes_to_exact_token_not_earliest_event() {
        let config = Pm5mDirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".to_string()]);
        let mut engine =
            Pm5mDirectionalBacktestEngine::new_without_recorder(config).expect("engine");

        let early = "btc-updown-5m-early";
        let late = "btc-updown-5m-late";

        engine.process_update(MarketUpdate {
            timestamp: ts(0),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::EventState {
                event_slug: early.to_string(),
                end_time: Some(ts(300)),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });
        engine.process_update(MarketUpdate {
            timestamp: ts(1),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: early.to_string(),
                token_id: format!("{early}:UP"),
                side: Side::Up,
                best_bid: Some(dec!(0.40)),
                best_ask: Some(dec!(0.41)),
                bid_size: Some(dec!(20)),
                ask_size: Some(dec!(20)),
            },
        });
        engine.process_update(MarketUpdate {
            timestamp: ts(1),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: early.to_string(),
                token_id: format!("{early}:DOWN"),
                side: Side::Down,
                best_bid: Some(dec!(0.58)),
                best_ask: Some(dec!(0.59)),
                bid_size: Some(dec!(20)),
                ask_size: Some(dec!(20)),
            },
        });

        engine.process_update(MarketUpdate {
            timestamp: ts(60),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::EventState {
                event_slug: late.to_string(),
                end_time: Some(ts(600)),
                price_to_beat: Some(dec!(101)),
                outcome: None,
            },
        });
        engine.process_update(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: late.to_string(),
                token_id: format!("{late}:UP"),
                side: Side::Up,
                best_bid: Some(dec!(0.44)),
                best_ask: Some(dec!(0.45)),
                bid_size: Some(dec!(10)),
                ask_size: Some(dec!(10)),
            },
        });
        engine.process_update(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: late.to_string(),
                token_id: format!("{late}:DOWN"),
                side: Side::Down,
                best_bid: Some(dec!(0.54)),
                best_ask: Some(dec!(0.55)),
                bid_size: Some(dec!(10)),
                ask_size: Some(dec!(10)),
            },
        });

        engine.process_update(MarketUpdate {
            timestamp: ts(62),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::LobSnapshot {
                event_slug: late.to_string(),
                token_id: format!("{late}:UP"),
                side: Side::Up,
                ask_depth_shares: 77,
                best_ask_size_shares: 25,
                ask_levels: vec![
                    BookAskLevel {
                        price: dec!(0.45),
                        size_shares: 25,
                    },
                    BookAskLevel {
                        price: dec!(0.46),
                        size_shares: 52,
                    },
                ],
                best_ask: Some(dec!(0.45)),
            },
        });

        assert_eq!(
            engine.quotes.get(&format!("{late}:UP")).and_then(|q| q.ask_size),
            Some(dec!(25))
        );
        assert_eq!(
            engine.quotes.get(&format!("{early}:UP")).and_then(|q| q.ask_size),
            Some(dec!(20))
        );
    }

    #[test]
    fn simulate_replay_fill_walks_book_and_uses_vwap() {
        let config = Pm5mDirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".to_string()]);
        let mut engine =
            Pm5mDirectionalBacktestEngine::new_without_recorder(config).expect("engine");
        let token_id = "btc-updown-5m-vwap:UP".to_string();

        engine.quotes.insert(
            token_id.clone(),
            QuoteState {
                best_bid: Some(dec!(0.39)),
                best_ask: Some(dec!(0.40)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                timestamp: ts(10),
            },
        );
        engine.book_asks.insert(
            token_id.clone(),
            BookAskState {
                levels: vec![
                    BookAskLevel {
                        price: dec!(0.40),
                        size_shares: 30,
                    },
                    BookAskLevel {
                        price: dec!(0.41),
                        size_shares: 40,
                    },
                    BookAskLevel {
                        price: dec!(0.42),
                        size_shares: 50,
                    },
                ],
                timestamp: ts(10),
            },
        );

        let quote = engine.quotes.get(&token_id).copied().expect("quote");
        let fill = engine.simulate_replay_fill(
            &token_id,
            &quote,
            60,
            dec!(0.41),
        );

        assert_eq!(fill.filled_qty, 60);
        assert_eq!(fill.avg_fill_price, dec!(0.405));
        let expected_fee = engine.fill_fee(30, dec!(0.40)) + engine.fill_fee(30, dec!(0.41));
        assert_eq!(fill.fee_paid, expected_fee);
    }

    #[test]
    fn replay_fill_consumes_book_liquidity_between_orders() {
        let config = Pm5mDirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".to_string()]);
        let mut engine =
            Pm5mDirectionalBacktestEngine::new_without_recorder(config).expect("engine");
        let token_id = "btc-updown-5m-consume:UP".to_string();

        engine.quotes.insert(
            token_id.clone(),
            QuoteState {
                best_bid: Some(dec!(0.39)),
                best_ask: Some(dec!(0.40)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(30)),
                timestamp: ts(20),
            },
        );
        engine.book_asks.insert(
            token_id.clone(),
            BookAskState {
                levels: vec![
                    BookAskLevel {
                        price: dec!(0.40),
                        size_shares: 30,
                    },
                    BookAskLevel {
                        price: dec!(0.41),
                        size_shares: 10,
                    },
                ],
                timestamp: ts(20),
            },
        );

        let fill_one = engine.simulate_replay_fill(
            &token_id,
            &engine.quotes.get(&token_id).copied().expect("quote"),
            25,
            dec!(0.41),
        );
        let fill_two = engine.simulate_replay_fill(
            &token_id,
            &engine.quotes.get(&token_id).copied().expect("quote"),
            25,
            dec!(0.41),
        );

        assert_eq!(fill_one.filled_qty, 25);
        assert_eq!(fill_two.filled_qty, 15);
        assert_eq!(fill_two.avg_fill_price, dec!(0.4066666666666666666666666667));
        assert!(engine.book_asks.get(&token_id).is_none());
        assert_eq!(
            engine.quotes.get(&token_id).and_then(|quote| quote.ask_size),
            Some(Decimal::ZERO)
        );
    }

    #[test]
    fn newer_quote_invalidates_stale_book_ladder() {
        let config = Pm5mDirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".to_string()]);
        let mut engine =
            Pm5mDirectionalBacktestEngine::new_without_recorder(config).expect("engine");
        let event_slug = "btc-updown-5m-stale-book";
        let token_id = format!("{event_slug}:UP");

        engine.process_update(MarketUpdate {
            timestamp: ts(10),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::LobSnapshot {
                event_slug: event_slug.to_string(),
                token_id: token_id.clone(),
                side: Side::Up,
                ask_depth_shares: 30,
                best_ask_size_shares: 30,
                ask_levels: vec![BookAskLevel {
                    price: dec!(0.40),
                    size_shares: 30,
                }],
                best_ask: Some(dec!(0.40)),
            },
        });
        engine.process_update(MarketUpdate {
            timestamp: ts(11),
            symbol: "BTCUSDT".to_string(),
            update_type: UpdateType::PmQuote {
                event_slug: event_slug.to_string(),
                token_id: token_id.clone(),
                side: Side::Up,
                best_bid: Some(dec!(0.44)),
                best_ask: Some(dec!(0.45)),
                bid_size: Some(dec!(10)),
                ask_size: Some(dec!(5)),
            },
        });

        let fill = engine.simulate_replay_fill(
            &token_id,
            &engine.quotes.get(&token_id).copied().expect("quote"),
            5,
            dec!(0.45),
        );

        assert_eq!(fill.filled_qty, 5);
        assert_eq!(fill.avg_fill_price, dec!(0.45));
        assert!(engine.book_asks.get(&token_id).is_none());
    }

    #[test]
    fn record_equity_point_marks_open_trade_to_best_bid() {
        let config = Pm5mDirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".to_string()]);
        let mut engine =
            Pm5mDirectionalBacktestEngine::new_without_recorder(config).expect("engine");
        let token_id = "btc-updown-5m-mtm:UP".to_string();
        let shares = 10u64;
        let entry_fee = engine.fill_fee(shares, dec!(0.40));

        engine.open_trades.insert(
            token_id.clone(),
            OpenTrade {
                symbol: "BTCUSDT".to_string(),
                event_slug: "btc-updown-5m-mtm".to_string(),
                token_id: token_id.clone(),
                side: Side::Up,
                entry_time: ts(5),
                entry_price: dec!(0.40),
                entry_fee,
                shares,
                entry_p_hat: Some(0.65),
                entry_ev_net: Some(0.05),
                entry_sigma: Some(0.02),
                s0: Some(dec!(100)),
            },
        );
        engine.quotes.insert(
            token_id,
            QuoteState {
                best_bid: Some(dec!(0.32)),
                best_ask: Some(dec!(0.33)),
                bid_size: Some(dec!(10)),
                ask_size: Some(dec!(10)),
                timestamp: ts(6),
            },
        );

        engine.record_equity_point(ts(6));

        let expected_unrealized =
            (dec!(0.32) * Decimal::from(shares) - engine.fill_fee(shares, dec!(0.32)))
                - (dec!(0.40) * Decimal::from(shares) + entry_fee);
        assert_eq!(engine.equity_curve.len(), 1);
        assert_eq!(
            engine.equity_curve[0].1,
            engine.config.initial_capital + expected_unrealized
        );
        assert!(
            calculate_max_drawdown(&engine.equity_curve, engine.config.initial_capital)
                > Decimal::ZERO
        );
    }

    #[test]
    fn record_equity_point_does_not_mark_to_ask_when_bid_missing() {
        let config = Pm5mDirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".to_string()]);
        let mut engine =
            Pm5mDirectionalBacktestEngine::new_without_recorder(config).expect("engine");
        let token_id = "btc-updown-5m-no-bid:UP".to_string();
        let shares = 10u64;
        let entry_fee = engine.fill_fee(shares, dec!(0.40));

        engine.open_trades.insert(
            token_id.clone(),
            OpenTrade {
                symbol: "BTCUSDT".to_string(),
                event_slug: "btc-updown-5m-no-bid".to_string(),
                token_id: token_id.clone(),
                side: Side::Up,
                entry_time: ts(5),
                entry_price: dec!(0.40),
                entry_fee,
                shares,
                entry_p_hat: Some(0.65),
                entry_ev_net: Some(0.05),
                entry_sigma: Some(0.02),
                s0: Some(dec!(100)),
            },
        );
        engine.quotes.insert(
            token_id,
            QuoteState {
                best_bid: None,
                best_ask: Some(dec!(0.33)),
                bid_size: None,
                ask_size: Some(dec!(10)),
                timestamp: ts(6),
            },
        );

        engine.record_equity_point(ts(6));

        let expected_unrealized = Decimal::ZERO - (dec!(0.40) * Decimal::from(shares) + entry_fee);
        assert_eq!(
            engine.equity_curve[0].1,
            engine.config.initial_capital + expected_unrealized
        );
    }

    #[test]
    fn equity_sharpe_uses_fixed_resample_cadence() {
        let sparse = vec![
            (ts(0), dec!(10000)),
            (ts(60), dec!(10100)),
            (ts(120), dec!(9900)),
        ];
        let dense = vec![
            (ts(0), dec!(10000)),
            (ts(20), dec!(10020)),
            (ts(40), dec!(10040)),
            (ts(60), dec!(10100)),
            (ts(80), dec!(10050)),
            (ts(100), dec!(10010)),
            (ts(120), dec!(9900)),
        ];

        assert_eq!(
            calculate_equity_sharpe(&sparse),
            calculate_equity_sharpe(&dense)
        );
    }
}
