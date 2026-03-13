//! `pattern_memory` strategy.
//!
//! Objective B (Polymarket 5m): estimate `P(price_at_resolution > price_to_beat)` using
//! associative memory over recent kline return patterns.

use super::engine::{PatternMemory, Posterior};
use crate::domain::Domain;
use crate::domain::{OrderType, Side, TimeInForce};
use crate::error::Result;
use crate::strategy::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyEvent, StrategyEventType, StrategyOrderIntent, StrategyStateInfo,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, HashSet};

mod config_loader;
#[path = "decision_runtime.rs"]
mod decision_runtime;

use self::config_loader::Config;

const PATTERN_LEN: usize = 20;
const TF_5M: &str = "5m";
const TF_15M: &str = "15m";

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct QuoteState {
    side: Side,
    best_bid: Option<Decimal>,
    best_ask: Option<Decimal>,
    ts: DateTime<Utc>,
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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
    #[allow(dead_code)]
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

impl PatternMemoryStrategy {}

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
                condition_id: _,
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
                        let mem = self.mem_5m.entry(symbol.clone()).or_insert_with(|| {
                            PatternMemory::<PATTERN_LEN>::new().with_max_samples(max_s)
                        });
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
                        let mem = self.mem_15m.entry(symbol.clone()).or_insert_with(|| {
                            PatternMemory::<PATTERN_LEN>::new().with_max_samples(max_s)
                        });
                        mem.ingest_return(r, *timestamp);
                    }
                    _ => {}
                }
            }

            // pattern_memory doesn't need trade ticks / spot prices.
            MarketUpdate::BinancePrice { .. } => {}
            MarketUpdate::BinanceL2 { .. } => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use rust_decimal_macros::dec;

    fn pattern_memory_test_config() -> &'static str {
        r#"
[[markets]]
symbol = "BTC"
series_id = "series-btc"

[pattern]
min_matches = 0
min_n_eff = 0.0
min_confidence = 0.0

[filter_15m]
enabled = false

[trade]
shares = 42
max_entry_price = 0.55
min_net_ev = 0.0
"#
    }

    #[tokio::test]
    async fn maybe_trade_on_5m_close_emits_canonical_submit_intent() {
        let now = Utc::now();
        let mut strategy = PatternMemoryStrategy::from_toml(
            "pattern-memory-test".to_string(),
            pattern_memory_test_config(),
            true,
        )
        .expect("strategy config should parse");

        strategy
            .mem_5m
            .insert("BTC".to_string(), PatternMemory::<PATTERN_LEN>::new());
        strategy.events.insert(
            "BTC".to_string(),
            HashMap::from([(
                "event-1".to_string(),
                EventState {
                    event_id: "event-1".to_string(),
                    series_id: "series-btc".to_string(),
                    up_token: "token-up".to_string(),
                    down_token: "token-down".to_string(),
                    end_time: now + Duration::seconds(300),
                    price_to_beat: None,
                    title: Some("btc-up-down".to_string()),
                },
            )]),
        );
        strategy.quotes.insert(
            "token-up".to_string(),
            QuoteState {
                side: Side::Up,
                best_bid: Some(dec!(0.09)),
                best_ask: Some(dec!(0.10)),
                ts: now,
            },
        );

        let actions = strategy
            .maybe_trade_on_5m_close("BTC", dec!(100000), now)
            .await
            .expect("strategy should emit entry actions");

        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], StrategyAction::LogEvent { .. }));

        let intent = match &actions[1] {
            StrategyAction::SubmitIntent { intent } => intent,
            other => panic!("expected submit intent, got {other:?}"),
        };

        assert!(intent
            .client_order_id
            .starts_with("pattern-memory-test_BTC_event-1_up_"));
        assert_eq!(intent.domain, Domain::Crypto);
        assert_eq!(intent.market_slug, "event-1");
        assert_eq!(intent.token_id, "token-up");
        assert_eq!(intent.side, Side::Up);
        assert!(intent.is_buy);
        assert_eq!(intent.shares, 42);
        assert_eq!(intent.limit_price, dec!(0.10));
        assert_eq!(intent.priority, 7);
        assert!(intent.metadata.is_empty());
    }
}
