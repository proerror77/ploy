//! `pattern_memory` strategy.
//!
//! Objective B (Polymarket 5m): estimate `P(price_at_resolution > price_to_beat)` using
//! associative memory over recent kline return patterns.

use super::engine::{PatternMemory, Posterior};
use crate::domain::{OrderRequest, Quote, Side};
use crate::error::{PloyError, Result};
use crate::strategy::multi_outcome::{ExpectedValue, POLYMARKET_FEE_RATE};
use crate::strategy::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyEvent, StrategyEventType, StrategyStateInfo,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, HashSet};

const PATTERN_LEN: usize = 20;
const TF_5M: &str = "5m";
const TF_15M: &str = "15m";

#[derive(Debug, Clone)]
struct MarketMapping {
    symbol: String,
    series_id: String,
}

#[derive(Debug, Clone)]
struct TimingConfig {
    target_remaining_secs: i64,
    tolerance_secs: i64,
    min_remaining_secs: i64,
}

#[derive(Debug, Clone)]
struct Filter15mConfig {
    enabled: bool,
    min_confidence: f64,
    min_n_eff: f64,
}

#[derive(Debug, Clone)]
struct PatternConfig {
    corr_threshold: f64,
    alpha: f64,
    beta: f64,
    min_matches: usize,
    min_n_eff: f64,
    min_confidence: f64,
    age_decay_lambda: f64,
    max_samples: usize,
}

#[derive(Debug, Clone)]
struct TradeConfig {
    shares: u64,
    max_entry_price: Decimal,
    min_net_ev: Decimal,
    cooldown_secs: i64,
}

#[derive(Debug, Clone)]
struct Config {
    markets: Vec<MarketMapping>,
    timing: TimingConfig,
    pattern: PatternConfig,
    filter_15m: Filter15mConfig,
    trade: TradeConfig,
}

#[derive(Debug, Clone)]
struct QuoteState {
    side: Side,
    best_bid: Option<Decimal>,
    best_ask: Option<Decimal>,
    ts: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct EventState {
    event_id: String,
    series_id: String,
    up_token: String,
    down_token: String,
    end_time: DateTime<Utc>,
    price_to_beat: Option<Decimal>,
    title: Option<String>,
}

#[derive(Debug, Clone)]
struct LastDecision {
    event_id: String,
    symbol: String,
    p_up: f64,
    conf: f64,
    required_return: f64,
    matches: usize,
    n_eff: f64,
    tf15_conf: Option<f64>,
    tf15_dir_ok: Option<bool>,
    at: DateTime<Utc>,
}

pub struct PatternMemoryStrategy {
    id: String,
    dry_run: bool,
    cfg: Config,
    enabled: bool,

    // Config-derived maps.
    symbol_by_series: HashMap<String, String>,
    series_by_symbol: HashMap<String, String>,

    // Live state.
    mem_5m: HashMap<String, PatternMemory<PATTERN_LEN>>,
    mem_15m: HashMap<String, PatternMemory<PATTERN_LEN>>,
    quotes: HashMap<String, QuoteState>, // token_id -> quote
    events: HashMap<String, HashMap<String, EventState>>, // symbol -> (event_id -> event)
    traded_events: HashSet<String>,
    cooldowns: HashMap<String, DateTime<Utc>>, // symbol -> last trade time

    last_decision: HashMap<String, LastDecision>, // symbol -> decision
}

impl PatternMemoryStrategy {
    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        use toml::Value;

        let config: Value = toml::from_str(config_str)
            .map_err(|e| PloyError::Internal(format!("Invalid TOML: {e}")))?;

        let markets = config
            .get("markets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| PloyError::Internal("Missing [[markets]]".to_string()))?;

        let mut parsed_markets: Vec<MarketMapping> = Vec::new();
        for m in markets {
            let symbol = m
                .get("symbol")
                .and_then(|v| v.as_str())
                .ok_or_else(|| PloyError::Internal("markets.symbol missing".to_string()))?;
            let series_id = m
                .get("series_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| PloyError::Internal("markets.series_id missing".to_string()))?;
            parsed_markets.push(MarketMapping {
                symbol: symbol.to_string(),
                series_id: series_id.to_string(),
            });
        }

        let empty = Value::Table(Default::default());
        let pattern = config.get("pattern").unwrap_or(&empty);
        let filter_15m = config.get("filter_15m").unwrap_or(&empty);
        let trade = config.get("trade").unwrap_or(&empty);
        let timing = config.get("timing").unwrap_or(&empty);

        let cfg = Config {
            markets: parsed_markets,
            timing: TimingConfig {
                target_remaining_secs: timing
                    .get("target_remaining_secs")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(300) as i64,
                tolerance_secs: timing
                    .get("tolerance_secs")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(45) as i64,
                min_remaining_secs: timing
                    .get("min_remaining_secs")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(60) as i64,
            },
            pattern: PatternConfig {
                corr_threshold: pattern
                    .get("corr_threshold")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.70),
                alpha: pattern
                    .get("alpha")
                    .and_then(|v| v.as_float())
                    .unwrap_or(5.0),
                beta: pattern
                    .get("beta")
                    .and_then(|v| v.as_float())
                    .unwrap_or(5.0),
                min_matches: pattern
                    .get("min_matches")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(10) as usize,
                min_n_eff: pattern
                    .get("min_n_eff")
                    .and_then(|v| v.as_float())
                    .unwrap_or(5.0),
                min_confidence: pattern
                    .get("min_confidence")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.60),
                age_decay_lambda: pattern
                    .get("age_decay_lambda")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.001),
                max_samples: pattern
                    .get("max_samples")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(2000) as usize,
            },
            filter_15m: Filter15mConfig {
                enabled: filter_15m
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
                min_confidence: filter_15m
                    .get("min_confidence")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.55),
                min_n_eff: filter_15m
                    .get("min_n_eff")
                    .and_then(|v| v.as_float())
                    .unwrap_or(1.0),
            },
            trade: TradeConfig {
                shares: trade
                    .get("shares")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(100) as u64,
                max_entry_price: Decimal::from_f64(
                    trade
                        .get("max_entry_price")
                        .and_then(|v| v.as_float())
                        .unwrap_or(0.55),
                )
                .unwrap_or(dec!(0.55)),
                min_net_ev: Decimal::from_f64(
                    trade
                        .get("min_net_ev")
                        .and_then(|v| v.as_float())
                        .unwrap_or(0.0),
                )
                .unwrap_or(Decimal::ZERO),
                cooldown_secs: trade
                    .get("cooldown_secs")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(30) as i64,
            },
        };

        let mut symbol_by_series = HashMap::new();
        let mut series_by_symbol = HashMap::new();
        for m in &cfg.markets {
            symbol_by_series.insert(m.series_id.clone(), m.symbol.clone());
            series_by_symbol.insert(m.symbol.clone(), m.series_id.clone());
        }

        Ok(Self {
            id,
            dry_run,
            cfg,
            enabled: true,
            symbol_by_series,
            series_by_symbol,
            mem_5m: HashMap::new(),
            mem_15m: HashMap::new(),
            quotes: HashMap::new(),
            events: HashMap::new(),
            traded_events: HashSet::new(),
            cooldowns: HashMap::new(),
            last_decision: HashMap::new(),
        })
    }

    fn symbol_for_series(&self, series_id: &str) -> Option<&str> {
        self.symbol_by_series.get(series_id).map(|s| s.as_str())
    }

    fn direction_from_p_up(p_up: f64) -> Side {
        if p_up >= 0.5 {
            Side::Up
        } else {
            Side::Down
        }
    }

    fn confidence_from_p_up(p_up: f64) -> f64 {
        p_up.max(1.0 - p_up)
    }

    fn required_return(spot: Decimal, price_to_beat: Decimal) -> Option<f64> {
        if spot <= Decimal::ZERO {
            return None;
        }
        let rr = (price_to_beat - spot) / spot;
        rr.to_f64()
    }

    fn kline_return(open: Decimal, close: Decimal) -> Option<f64> {
        if open <= Decimal::ZERO {
            return None;
        }
        ((close - open) / open).to_f64()
    }

    fn in_cooldown(&self, symbol: &str, now: DateTime<Utc>) -> bool {
        let Some(last) = self.cooldowns.get(symbol) else {
            return false;
        };
        now.signed_duration_since(*last).num_seconds() < self.cfg.trade.cooldown_secs
    }

    fn pick_event<'a>(&'a self, symbol: &str, now: DateTime<Utc>) -> Option<&'a EventState> {
        let events = self.events.get(symbol)?;
        let mut best: Option<(&EventState, i64)> = None;
        for ev in events.values() {
            let rem = (ev.end_time - now).num_seconds();
            if rem <= 0 {
                continue;
            }
            if rem < self.cfg.timing.min_remaining_secs {
                continue;
            }
            let diff = (rem - self.cfg.timing.target_remaining_secs).abs();
            if diff > self.cfg.timing.tolerance_secs {
                continue;
            }
            match best {
                None => best = Some((ev, diff)),
                Some((_b, best_diff)) if diff < best_diff => best = Some((ev, diff)),
                _ => {}
            }
        }
        best.map(|(ev, _)| ev)
    }

    fn update_quote(&mut self, token_id: &str, side: Side, quote: &Quote, ts: DateTime<Utc>) {
        self.quotes.insert(
            token_id.to_string(),
            QuoteState {
                side,
                best_bid: quote.best_bid,
                best_ask: quote.best_ask,
                ts,
            },
        );
    }

    fn ev_for_side(entry_price: Decimal, p_win: f64) -> Option<ExpectedValue> {
        let p = Decimal::from_f64(p_win)?;
        Some(ExpectedValue::calculate(
            entry_price,
            p,
            Some(POLYMARKET_FEE_RATE),
        ))
    }

    fn should_trade_posterior(&self, post: &Posterior) -> bool {
        let conf = Self::confidence_from_p_up(post.p_up);
        post.matches >= self.cfg.pattern.min_matches
            && post.n_eff >= self.cfg.pattern.min_n_eff
            && conf >= self.cfg.pattern.min_confidence
    }

    fn filter_15m_ok(&self, symbol: &str, required_return: f64) -> (bool, f64, Posterior) {
        if !self.cfg.filter_15m.enabled {
            return (
                true,
                1.0,
                Posterior {
                    p_up: 0.5,
                    up_w: 0.0,
                    down_w: 0.0,
                    n_eff: 0.0,
                    matches: 0,
                },
            );
        }

        let Some(mem) = self.mem_15m.get(symbol) else {
            return (
                false,
                0.0,
                Posterior {
                    p_up: 0.5,
                    up_w: 0.0,
                    down_w: 0.0,
                    n_eff: 0.0,
                    matches: 0,
                },
            );
        };

        let post = mem.posterior_for_required_return(
            required_return,
            self.cfg.pattern.corr_threshold,
            self.cfg.pattern.alpha,
            self.cfg.pattern.beta,
            self.cfg.pattern.age_decay_lambda,
        );
        let conf = Self::confidence_from_p_up(post.p_up);
        let ok = post.n_eff >= self.cfg.filter_15m.min_n_eff
            && conf >= self.cfg.filter_15m.min_confidence;
        (ok, conf, post)
    }

    async fn maybe_trade_on_5m_close(
        &mut self,
        symbol: &str,
        spot: Decimal,
        now: DateTime<Utc>,
    ) -> Option<Vec<StrategyAction>> {
        if !self.enabled {
            return None;
        }
        if self.in_cooldown(symbol, now) {
            return None;
        }

        // Avoid decision spam during startup/backfill before any events are loaded.
        let has_events = self
            .events
            .get(symbol)
            .map(|m| !m.is_empty())
            .unwrap_or(false);
        if !has_events {
            return None;
        }

        let event = match self.pick_event(symbol, now) {
            Some(ev) => ev.clone(),
            None => {
                return Some(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Decision".to_string()),
                        format!(
                            "{} 5m close: no matching event (target_rem={}±{}s, min_rem={}s)",
                            symbol,
                            self.cfg.timing.target_remaining_secs,
                            self.cfg.timing.tolerance_secs,
                            self.cfg.timing.min_remaining_secs
                        ),
                    ),
                }]);
            }
        };

        if self.traded_events.contains(&event.event_id) {
            return None;
        }

        let rem = (event.end_time - now).num_seconds();

        // For Polymarket "Up or Down" markets the threshold is the *start price* of the window,
        // which is not included as a numeric value in Gamma metadata. In that case we model
        // `required_return = 0` (end > start).
        let (price_to_beat, required_return) = match event.price_to_beat {
            Some(thr) => (Some(thr), Self::required_return(spot, thr)?),
            None => (None, 0.0),
        };

        let mem5 = match self.mem_5m.get(symbol) {
            Some(m) => m,
            None => {
                return Some(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Decision".to_string()),
                        format!("{} 5m close: 5m memory not ready (rem={}s)", symbol, rem),
                    ),
                }]);
            }
        };

        let samples5 = mem5.samples_len();
        let post5 = mem5.posterior_for_required_return(
            required_return,
            self.cfg.pattern.corr_threshold,
            self.cfg.pattern.alpha,
            self.cfg.pattern.beta,
            self.cfg.pattern.age_decay_lambda,
        );

        let dir5 = Self::direction_from_p_up(post5.p_up);
        let conf5 = Self::confidence_from_p_up(post5.p_up);
        let p_win_dir5 = match dir5 {
            Side::Up => post5.p_up,
            Side::Down => 1.0 - post5.p_up,
        };

        let (filter_ok, conf15, post15) = self.filter_15m_ok(symbol, required_return);
        let dir15 = Self::direction_from_p_up(post15.p_up);
        let dir_ok = if self.cfg.filter_15m.enabled {
            dir15 == dir5
        } else {
            true
        };

        self.last_decision.insert(
            symbol.to_string(),
            LastDecision {
                event_id: event.event_id.clone(),
                symbol: symbol.to_string(),
                p_up: post5.p_up,
                conf: conf5,
                required_return,
                matches: post5.matches,
                n_eff: post5.n_eff,
                tf15_conf: if self.cfg.filter_15m.enabled {
                    Some(conf15)
                } else {
                    None
                },
                tf15_dir_ok: if self.cfg.filter_15m.enabled {
                    Some(dir_ok)
                } else {
                    None
                },
                at: now,
            },
        );

        let filter_desc = if self.cfg.filter_15m.enabled {
            format!(
                "15m_ok={} 15m_conf={:.1}% 15m_dir={} 15m_dir_ok={}",
                filter_ok,
                conf15 * 100.0,
                dir15,
                dir_ok
            )
        } else {
            "15m_filter=off".to_string()
        };

        if !self.should_trade_posterior(&post5) {
            return Some(vec![StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::Custom("Decision".to_string()),
                    format!(
                        "{} 5m close: event={} rem={}s {} dir5={} p_win={:.1}% conf5={:.1}% n_eff5={:.2} matches5={} samples5={} r_req={:.3}% => SKIP: evidence (need matches>={} n_eff>={:.2} conf>={:.1}%)",
                        symbol,
                        event.event_id,
                        rem,
                        filter_desc,
                        dir5,
                        p_win_dir5 * 100.0,
                        conf5 * 100.0,
                        post5.n_eff,
                        post5.matches,
                        samples5,
                        required_return * 100.0,
                        self.cfg.pattern.min_matches,
                        self.cfg.pattern.min_n_eff,
                        self.cfg.pattern.min_confidence * 100.0,
                    ),
                ),
            }]);
        }

        if self.cfg.filter_15m.enabled {
            if !filter_ok {
                return Some(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Decision".to_string()),
                        format!(
                            "{} 5m close: event={} rem={}s {} dir5={} => SKIP: 15m filter (need n_eff>={:.2} conf>={:.1}%, got n_eff={:.2} conf={:.1}%)",
                            symbol,
                            event.event_id,
                            rem,
                            filter_desc,
                            dir5,
                            self.cfg.filter_15m.min_n_eff,
                            self.cfg.filter_15m.min_confidence * 100.0,
                            post15.n_eff,
                            conf15 * 100.0,
                        ),
                    ),
                }]);
            }
            if !dir_ok {
                return Some(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Decision".to_string()),
                        format!(
                            "{} 5m close: event={} rem={}s {} dir5={} => SKIP: 15m dir mismatch",
                            symbol, event.event_id, rem, filter_desc, dir5
                        ),
                    ),
                }]);
            }
        }

        let (token_id, p_win) = match dir5 {
            Side::Up => (event.up_token.clone(), post5.p_up),
            Side::Down => (event.down_token.clone(), 1.0 - post5.p_up),
        };

        let ask = match self.quotes.get(&token_id).and_then(|q| q.best_ask) {
            Some(a) => a,
            None => {
                return Some(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Decision".to_string()),
                        format!(
                            "{} 5m close: event={} rem={}s {} dir5={} => SKIP: no quote for token={}",
                            symbol,
                            event.event_id,
                            rem,
                            filter_desc,
                            dir5,
                            &token_id[..8.min(token_id.len())]
                        ),
                    ),
                }]);
            }
        };

        if ask > self.cfg.trade.max_entry_price {
            return Some(vec![StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::Custom("Decision".to_string()),
                    format!(
                        "{} 5m close: event={} rem={}s {} dir5={} ask={:.1}c => SKIP: ask too high (max {:.1}c)",
                        symbol,
                        event.event_id,
                        rem,
                        filter_desc,
                        dir5,
                        ask * dec!(100),
                        self.cfg.trade.max_entry_price * dec!(100),
                    ),
                ),
            }]);
        }

        let ev = match Self::ev_for_side(ask, p_win) {
            Some(v) => v,
            None => {
                return Some(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Decision".to_string()),
                        format!(
                            "{} 5m close: event={} rem={}s {} dir5={} ask={:.1}c => SKIP: EV calc failed",
                            symbol,
                            event.event_id,
                            rem,
                            filter_desc,
                            dir5,
                            ask * dec!(100),
                        ),
                    ),
                }]);
            }
        };

        if ev.net_ev < self.cfg.trade.min_net_ev {
            return Some(vec![StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::Custom("Decision".to_string()),
                    format!(
                        "{} 5m close: event={} rem={}s {} dir5={} ask={:.1}c net_ev={:.4} => SKIP: net_ev < min_net_ev ({:.4})",
                        symbol,
                        event.event_id,
                        rem,
                        filter_desc,
                        dir5,
                        ask * dec!(100),
                        ev.net_ev,
                        self.cfg.trade.min_net_ev,
                    ),
                ),
            }]);
        }

        if !ev.is_positive_ev {
            return Some(vec![StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::Custom("Decision".to_string()),
                    format!(
                        "{} 5m close: event={} rem={}s {} dir5={} ask={:.1}c net_ev={:.4} => SKIP: negative EV",
                        symbol,
                        event.event_id,
                        rem,
                        filter_desc,
                        dir5,
                        ask * dec!(100),
                        ev.net_ev,
                    ),
                ),
            }]);
        }

        let client_order_id = format!(
            "{}_{}_{}_{}_{}",
            self.id,
            symbol,
            event.event_id,
            dir5.as_str().to_lowercase(),
            now.timestamp_millis()
        );

        let order = OrderRequest::buy_limit(token_id.clone(), dir5, self.cfg.trade.shares, ask);

        let mut actions: Vec<StrategyAction> = Vec::new();
        let thr_display = price_to_beat.unwrap_or(spot);
        let thr_src = if price_to_beat.is_some() {
            "fixed"
        } else {
            "dynamic"
        };
        let tf15_conf_display = if self.cfg.filter_15m.enabled {
            format!("{:.1}", conf15 * 100.0)
        } else {
            "NA".to_string()
        };
        let tf15_dir_display = if self.cfg.filter_15m.enabled {
            dir15.to_string()
        } else {
            "NA".to_string()
        };

        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::SignalDetected,
                format!(
                    "{} pattern_memory {} event={} rem={}s p_win={:.1}% conf={:.1}% n_eff={:.2} matches={} samples5={} r_req={:.3}% spot={:.2} thr_{}={:.2} ask={:.1}c net_ev={:.4} 15m_conf={} 15m_dir={}",
                    symbol,
                    dir5,
                    event.event_id,
                    rem,
                    p_win * 100.0,
                    conf5 * 100.0,
                    post5.n_eff,
                    post5.matches,
                    samples5,
                    required_return * 100.0,
                    spot,
                    thr_src,
                    thr_display,
                    ask * dec!(100),
                    ev.net_ev,
                    tf15_conf_display,
                    tf15_dir_display,
                ),
            ),
        });

        actions.push(StrategyAction::SubmitOrder {
            client_order_id,
            order,
            priority: 7,
        });

        self.traded_events.insert(event.event_id.clone());
        self.cooldowns.insert(symbol.to_string(), now);

        Some(actions)
    }
}

#[async_trait]
impl Strategy for PatternMemoryStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "pattern_memory"
    }

    fn description(&self) -> &str {
        "Associative pattern memory on Binance klines -> Polymarket 5m UP/DOWN"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        let symbols: Vec<String> = self.cfg.markets.iter().map(|m| m.symbol.clone()).collect();
        let series_ids: Vec<String> = self
            .cfg
            .markets
            .iter()
            .map(|m| m.series_id.clone())
            .collect();

        vec![
            DataFeed::BinanceKlines {
                symbols,
                intervals: vec![TF_5M.to_string(), TF_15M.to_string()],
                closed_only: true,
            },
            DataFeed::PolymarketEvents { series_ids },
        ]
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        let mut actions: Vec<StrategyAction> = Vec::new();

        match update {
            MarketUpdate::PolymarketQuote {
                token_id,
                side,
                quote,
                timestamp,
            } => {
                self.update_quote(token_id, *side, quote, *timestamp);
            }

            MarketUpdate::EventDiscovered {
                event_id,
                series_id,
                up_token,
                down_token,
                end_time,
                price_to_beat,
                title,
            } => {
                let Some(symbol) = self.symbol_for_series(series_id) else {
                    return Ok(actions);
                };

                let state = EventState {
                    event_id: event_id.clone(),
                    series_id: series_id.clone(),
                    up_token: up_token.clone(),
                    down_token: down_token.clone(),
                    end_time: *end_time,
                    price_to_beat: *price_to_beat,
                    title: title.clone(),
                };

                self.events
                    .entry(symbol.to_string())
                    .or_default()
                    .insert(event_id.clone(), state);
            }

            MarketUpdate::EventExpired { event_id } => {
                for per_symbol in self.events.values_mut() {
                    per_symbol.remove(event_id);
                }
                self.traded_events.remove(event_id);
            }

            MarketUpdate::BinanceKline {
                symbol,
                interval,
                kline,
                timestamp,
            } => {
                if !kline.is_closed {
                    return Ok(actions);
                }

                let Some(r) = Self::kline_return(kline.open, kline.close) else {
                    return Ok(actions);
                };

                match interval.as_str() {
                    TF_5M => {
                        let max_s = self.cfg.pattern.max_samples;
                        let mem = self
                            .mem_5m
                            .entry(symbol.clone())
                            .or_insert_with(|| PatternMemory::<PATTERN_LEN>::new().with_max_samples(max_s));
                        mem.ingest_return(r, *timestamp);

                        if let Some(mut a) = self
                            .maybe_trade_on_5m_close(symbol, kline.close, *timestamp)
                            .await
                        {
                            actions.append(&mut a);
                        }
                    }
                    TF_15M => {
                        let max_s = self.cfg.pattern.max_samples;
                        let mem = self
                            .mem_15m
                            .entry(symbol.clone())
                            .or_insert_with(|| PatternMemory::<PATTERN_LEN>::new().with_max_samples(max_s));
                        mem.ingest_return(r, *timestamp);
                    }
                    _ => {}
                }
            }

            // pattern_memory doesn't need trade ticks / spot prices.
            MarketUpdate::BinancePrice { .. } => {}
        }

        Ok(actions)
    }

    async fn on_order_update(&mut self, _update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        Ok(Vec::new())
    }

    async fn on_tick(&mut self, _now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        Ok(Vec::new())
    }

    fn state(&self) -> StrategyStateInfo {
        let mut metrics: HashMap<String, String> = HashMap::new();

        for (sym, d) in &self.last_decision {
            metrics.insert(format!("{}_p_up", sym), format!("{:.4}", d.p_up));
            metrics.insert(format!("{}_conf", sym), format!("{:.4}", d.conf));
            metrics.insert(format!("{}_n_eff", sym), format!("{:.2}", d.n_eff));
            metrics.insert(format!("{}_matches", sym), format!("{}", d.matches));
            metrics.insert(
                format!("{}_r_req", sym),
                format!("{:.5}", d.required_return),
            );
        }

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: if self.enabled { "running" } else { "disabled" }.to_string(),
            enabled: self.enabled,
            active: false,
            position_count: 0,
            pending_order_count: 0,
            total_exposure: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            realized_pnl_today: Decimal::ZERO,
            last_update: Utc::now(),
            metrics,
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        Vec::new()
    }

    fn is_active(&self) -> bool {
        false
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.enabled = false;
        Ok(vec![StrategyAction::Alert {
            level: AlertLevel::Info,
            message: format!("{} shutdown (dry_run={})", self.id, self.dry_run),
        }])
    }

    fn reset(&mut self) {
        self.mem_5m.clear();
        self.mem_15m.clear();
        self.quotes.clear();
        self.events.clear();
        self.traded_events.clear();
        self.cooldowns.clear();
        self.last_decision.clear();
    }
}
