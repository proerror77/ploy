//! Strategy Adapters
//!
//! Adapters that wrap existing strategy implementations to implement the Strategy trait.
//! This enables using existing engines (MomentumEngine, SplitArbEngine) with the new
//! StrategyManager infrastructure.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{OnceCell, RwLock};
use tracing::{debug, info, warn};

use super::momentum::{Direction, ExitConfig, MomentumConfig};
use super::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyEvent, StrategyEventType, StrategyOrderIntent, StrategyStateInfo,
};
use crate::domain::{OrderType, Side, TimeInForce};
use crate::error::Result;
use crate::platform::Domain;
use crate::strategy::crypto::{all_updown_series_ids, symbol_and_window_for_series};
mod momentum_adapter;
mod split_arb_adapter;
pub use momentum_adapter::MomentumStrategyAdapter;
pub use split_arb_adapter::SplitArbStrategyAdapter;

fn crypto_submit_intent(
    client_order_id: String,
    market_slug: String,
    token_id: String,
    side: Side,
    is_buy: bool,
    shares: u64,
    limit_price: Decimal,
    priority: u8,
) -> StrategyAction {
    StrategyAction::SubmitIntent {
        intent: StrategyOrderIntent {
            client_order_id,
            domain: Domain::Crypto,
            market_slug,
            token_id,
            side,
            is_buy,
            shares,
            limit_price,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            priority,
            metadata: HashMap::new(),
        },
    }
}



// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_momentum_adapter_creation() {
        let config = MomentumConfig::default();
        let exit_config = ExitConfig::default();
        let adapter =
            MomentumStrategyAdapter::new("test_momentum".into(), config, exit_config, true);

        assert_eq!(adapter.id(), "test_momentum");
        assert_eq!(adapter.name(), "Momentum Strategy");
    }

    #[test]
    fn test_from_toml() {
        let toml = r#"
[strategy]
name = "momentum"
mode = "predictive"

[entry]
symbols = ["BTCUSDT", "ETHUSDT"]
min_move = 0.5
max_entry = 45

[exit]
exit_edge_floor_pct = 20
exit_price_band_pct = 12

[timing]
min_time_remaining = 300
max_time_remaining = 900

[risk]
shares = 100
max_positions = 5
"#;

        let adapter = MomentumStrategyAdapter::from_toml("test".into(), toml, true).unwrap();

        assert_eq!(adapter.config.symbols.len(), 2);
        assert!(!adapter.config.hold_to_resolution);
        assert_eq!(adapter.config.shares_per_trade, 100);
        assert_eq!(adapter.config.max_positions, 5);
        assert_eq!(adapter.config.min_time_remaining_secs, 300);
        assert_eq!(adapter.config.max_time_remaining_secs, 900);
    }

    #[test]
    fn test_from_toml_directional_entry_threshold() {
        let toml_pct = r#"
[strategy]
name = "momentum"
mode = "predictive"

[entry]
symbols = ["BTCUSDT"]
directional_mode = true
directional_entry_threshold = 8
"#;
        let adapter_pct =
            MomentumStrategyAdapter::from_toml("test".into(), toml_pct, true).unwrap();
        assert!((adapter_pct.directional_entry_threshold - 0.08).abs() < f64::EPSILON);

        let toml_decimal = r#"
[strategy]
name = "momentum"
mode = "predictive"

[entry]
symbols = ["BTCUSDT"]
directional_mode = true
directional_entry_threshold = 0.11
"#;
        let adapter_decimal =
            MomentumStrategyAdapter::from_toml("test".into(), toml_decimal, true).unwrap();
        assert!((adapter_decimal.directional_entry_threshold - 0.11).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_generate_entry_respects_timing_window() {
        let mut config = MomentumConfig::default();
        config.min_time_remaining_secs = 300;
        config.max_time_remaining_secs = 900;

        let adapter = MomentumStrategyAdapter::new(
            "test_momentum".into(),
            config,
            ExitConfig::default(),
            true,
        );
        let now = Utc::now();
        let up_token = "up_token".to_string();
        let down_token = "down_token".to_string();

        {
            let mut events = adapter.events.write().await;
            events.insert(
                "BTCUSDT".to_string(),
                vec![EventState {
                    event_id: "evt_outside".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    up_token_id: up_token.clone(),
                    down_token_id: down_token.clone(),
                    end_time: now + chrono::Duration::seconds(120),
                    open_price: None,
                    window_secs: 300,
                }],
            );
        }

        {
            let mut quotes = adapter.pm_quotes.write().await;
            quotes.insert(
                up_token.clone(),
                PmQuoteState {
                    token_id: up_token.clone(),
                    best_bid: Some(dec!(0.40)),
                    best_ask: Some(dec!(0.42)),
                    timestamp: now,
                },
            );
        }

        assert!(adapter
            .get_entry_price("BTCUSDT", Direction::Up)
            .await
            .is_none());
        assert!(adapter
            .generate_entry("BTCUSDT", Direction::Up, dec!(0.42))
            .await
            .is_none());
    }

    #[test]
    fn test_momentum_required_feeds_include_xrp_5m() {
        let adapter = MomentumStrategyAdapter::new(
            "test_momentum".into(),
            MomentumConfig::default(),
            ExitConfig::default(),
            true,
        );

        let feeds = adapter.required_feeds();
        let series_ids = feeds
            .iter()
            .find_map(|feed| match feed {
                DataFeed::PolymarketEvents { series_ids } => Some(series_ids.clone()),
                _ => None,
            })
            .expect("expected polymarket events feed");

        assert!(series_ids.contains(&"10685".to_string()));
    }

    #[test]
    fn test_momentum_from_toml_rejects_deprecated_exit_keys() {
        let toml = r#"
[strategy]
name = "momentum"
mode = "predictive"

[entry]
symbols = ["BTCUSDT"]
min_move = 0.5
max_entry = 45

[exit]
take_profit = 20
stop_loss = 12
"#;

        let result = MomentumStrategyAdapter::from_toml("test".into(), toml, true);
        assert!(result.is_err());
    }

    #[test]
    fn test_split_arb_adapter_creation() {
        let config = CoreSplitArbConfig::default();
        let adapter = SplitArbStrategyAdapter::new("test_split".into(), config, true);

        assert_eq!(adapter.id(), "test_split");
        assert_eq!(adapter.name(), "Split Arbitrage Strategy");
    }

    #[test]
    fn test_split_arb_from_toml() {
        let toml = r#"
[strategy]
name = "split_arb"

[entry]
max_entry = 0.35
target_sum = 0.70
min_profit = 0.05

[risk]
shares = 100
max_hedge_wait = 30
max_unhedged = 3
unhedged_stop = 10

[markets]
series_ids = ["10684", "10192", "10684"]
"#;

        let adapter = SplitArbStrategyAdapter::from_toml("test".into(), toml, true).unwrap();

        assert_eq!(adapter.config.max_entry_price, dec!(0.35));
        assert_eq!(adapter.config.target_total_cost, dec!(0.70));
        assert_eq!(adapter.config.shares_per_trade, 100);
        let feeds = adapter.required_feeds();
        match &feeds[0] {
            DataFeed::PolymarketEvents { series_ids } => {
                assert_eq!(series_ids, &vec!["10192".to_string(), "10684".to_string()]);
            }
            _ => panic!("expected PolymarketEvents feed"),
        }
    }

    #[test]
    fn test_split_arb_from_toml_rejects_deprecated_keys() {
        let toml = r#"
[strategy]
name = "split_arb"

[entry]
max_combined_price = 98
min_spread = 2

[position]
shares_per_side = 50
max_positions = 10
"#;

        let result = SplitArbStrategyAdapter::from_toml("test".into(), toml, true);
        assert!(result.is_err());
    }
}
