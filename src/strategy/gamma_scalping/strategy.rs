//! Gamma scalping strategy for Polymarket crypto binary options.
//!
//! Profits from realized volatility exceeding implied volatility by maintaining
//! delta-neutral straddle positions and rebalancing as the underlying moves.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};

use crate::domain::{Quote, Side};
use crate::error::Result;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::traits::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction, StrategyStateInfo,
};

use super::config::GammaScalpingConfig;
use super::rebalancer::{Rebalancer, Straddle};

mod decision_flow;
mod runtime_support;
mod state_view;

/// Metadata for a tracked event (discovered from Polymarket).
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct EventContext {
    event_id: String,
    series_id: String,
    symbol: String,
    up_token: String,
    down_token: String,
    end_time: DateTime<Utc>,
    price_to_beat: Option<Decimal>,
}

/// Tracks a pending order so we can match fills back to straddles.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct PendingOrder {
    client_order_id: String,
    event_id: String,
    token_id: String,
    side: Side,
    is_entry: bool,
    shares: u64,
    price: Decimal,
}

/// Gamma scalping strategy.
#[allow(dead_code)]
pub struct GammaScalpingStrategy {
    config: GammaScalpingConfig,
    /// Active straddle positions keyed by event_id
    straddles: HashMap<String, Straddle>,
    /// Pending orders keyed by client_order_id
    pending_orders: HashMap<String, PendingOrder>,
    /// Kline close prices for realized vol calculation, keyed by symbol
    kline_history: HashMap<String, VecDeque<f64>>,
    /// Latest quotes keyed by token_id
    quote_cache: HashMap<String, Quote>,
    /// Discovered events keyed by event_id
    active_events: HashMap<String, EventContext>,
    /// Latest spot prices keyed by symbol
    spot_prices: HashMap<String, f64>,
    rebalancer: Rebalancer,
    fee_model: FeeModel,
    realized_pnl: Decimal,
    daily_loss: Decimal,
    trade_count: u32,
    last_cooldown: Option<DateTime<Utc>>,
    active: bool,
}

impl GammaScalpingStrategy {
    pub fn new(config: GammaScalpingConfig) -> Self {
        let rebalancer = Rebalancer::new(&config);
        Self {
            config,
            straddles: HashMap::new(),
            pending_orders: HashMap::new(),
            kline_history: HashMap::new(),
            quote_cache: HashMap::new(),
            active_events: HashMap::new(),
            spot_prices: HashMap::new(),
            rebalancer,
            fee_model: FeeModel::crypto(),
            realized_pnl: Decimal::ZERO,
            daily_loss: Decimal::ZERO,
            trade_count: 0,
            last_cooldown: None,
            active: true,
        }
    }
}

#[async_trait]
impl Strategy for GammaScalpingStrategy {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn name(&self) -> &str {
        "gamma_scalping"
    }

    fn description(&self) -> &str {
        "Gamma scalping on Polymarket crypto binary options"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        let mut feeds = vec![
            DataFeed::BinanceKlines {
                symbols: self.config.symbols.clone(),
                intervals: vec![self.config.kline_interval.clone()],
                closed_only: true,
            },
            DataFeed::BinanceSpot {
                symbols: self.config.symbols.clone(),
            },
            DataFeed::Tick { interval_ms: 5000 },
        ];

        if !self.config.series_ids.is_empty() {
            feeds.push(DataFeed::PolymarketEvents {
                series_ids: self.config.series_ids.clone(),
            });
        }

        // Subscribe to quotes for all known tokens
        let tokens: Vec<String> = self
            .active_events
            .values()
            .flat_map(|e| vec![e.up_token.clone(), e.down_token.clone()])
            .collect();
        if !tokens.is_empty() {
            feeds.push(DataFeed::PolymarketQuotes { tokens });
        }

        feeds
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        self.handle_market_update(update).await
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        self.handle_order_update(update).await
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        self.handle_tick(now).await
    }

    fn state(&self) -> StrategyStateInfo {
        self.state_info()
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.position_snapshots()
    }

    fn is_active(&self) -> bool {
        self.active && self.config.enabled
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.shutdown_actions().await
    }

    fn reset(&mut self) {
        self.reset_runtime();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn evaluate_entry_emits_submit_intents() {
        let mut config = GammaScalpingConfig::default();
        config.dry_run = false;
        config.vol_lookback_periods = 5;
        let mut strategy = GammaScalpingStrategy::new(config);
        strategy.spot_prices.insert("BTCUSDT".to_string(), 100.0);
        strategy.kline_history.insert(
            "BTCUSDT".to_string(),
            VecDeque::from(vec![100.0, 110.0, 90.0, 120.0, 80.0, 130.0]),
        );
        strategy.quote_cache.insert(
            "token-up".to_string(),
            Quote {
                side: Side::Up,
                best_bid: Some(dec!(0.30)),
                best_ask: Some(dec!(0.31)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                timestamp: Utc::now(),
            },
        );
        strategy.quote_cache.insert(
            "token-down".to_string(),
            Quote {
                side: Side::Down,
                best_bid: Some(dec!(0.28)),
                best_ask: Some(dec!(0.29)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                timestamp: Utc::now(),
            },
        );

        let ctx = EventContext {
            event_id: "event-1".to_string(),
            series_id: "btc-series".to_string(),
            symbol: "BTCUSDT".to_string(),
            up_token: "token-up".to_string(),
            down_token: "token-down".to_string(),
            end_time: Utc::now() + chrono::Duration::seconds(600),
            price_to_beat: Some(dec!(100)),
        };

        let actions = strategy
            .evaluate_entry(&ctx, Utc::now())
            .expect("entry actions");

        assert!(matches!(
            actions.first(),
            Some(StrategyAction::SubmitIntent { .. })
        ));
        assert!(matches!(
            actions.get(1),
            Some(StrategyAction::SubmitIntent { .. })
        ));
    }
}
