use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;

use crate::adapters::SpotPrice;
use crate::collector::LobSnapshot;
use crate::domain::Quote;
use crate::error::{PloyError, Result};
#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
use crate::strategy::crypto_rl_policy::core;
use crate::strategy::traits::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction, StrategyEvent,
    StrategyEventType, StrategyStateInfo,
};
mod config_loader;
mod runtime_support;
#[path = "signal_flow.rs"]
mod signal_flow;
use self::config_loader::CryptoRlPolicyStrategyConfig;
use self::signal_flow::{RlSignalSummary, RlTrackedEvent};

const STRATEGY_NAME: &str = "crypto_rl_policy";

fn default_policy_output() -> String {
    "continuous".to_string()
}

fn default_observation_version() -> u32 {
    2
}

fn default_max_entry_price() -> Decimal {
    dec!(0.70)
}

pub struct CryptoRlPolicyStrategy {
    id: String,
    enabled: bool,
    cfg: CryptoRlPolicyStrategyConfig,
    symbols: Vec<String>,
    series_ids: Vec<String>,
    spot_prices: HashMap<String, SpotPrice>,
    l2_by_symbol: HashMap<String, LobSnapshot>,
    quotes: HashMap<String, Quote>,
    active_events: HashMap<String, RlTrackedEvent>,
    last_signal: Option<RlSignalSummary>,
    last_reason: Option<String>,
    last_error: Option<String>,
    last_logged_at: HashMap<String, DateTime<Utc>>,

    #[cfg(feature = "onnx")]
    policy_model: Option<OnnxModel>,
}

impl CryptoRlPolicyStrategy {
    pub fn from_toml(id: String, config_str: &str, _dry_run: bool) -> Result<Self> {
        config_loader::build_strategy_from_toml(id, config_str)
    }
}

#[cfg(feature = "onnx")]
fn load_policy_model(
    strategy_id: &str,
    cfg: &mut CryptoRlPolicyStrategyConfig,
) -> Result<Option<OnnxModel>> {
    use tracing::warn;

    let Some(path) = cfg.policy_model_path.as_deref() else {
        return Ok(None);
    };
    if path.trim().is_empty() {
        return Ok(None);
    }

    let primary_version = cfg.observation_version;
    let primary_dim = if primary_version == 1 {
        core::OBS_DIM_V1
    } else {
        core::OBS_DIM_V2
    };

    match OnnxModel::load_for_vec_input(path, primary_dim) {
        Ok(model) => Ok(Some(model)),
        Err(primary_error) => {
            let fallback_version = if primary_version == 1 { 2 } else { 1 };
            let fallback_dim = if fallback_version == 1 {
                core::OBS_DIM_V1
            } else {
                core::OBS_DIM_V2
            };

            match OnnxModel::load_for_vec_input(path, fallback_dim) {
                Ok(model) => {
                    cfg.observation_version = fallback_version;
                    Ok(Some(model))
                }
                Err(fallback_error) => {
                    warn!(
                        strategy = strategy_id,
                        model_path = %path,
                        error = %primary_error,
                        "failed to load crypto RL policy model (primary schema)"
                    );
                    warn!(
                        strategy = strategy_id,
                        model_path = %path,
                        error = %fallback_error,
                        "failed to load crypto RL policy model (fallback schema)"
                    );
                    Ok(None)
                }
            }
        }
    }
}

#[async_trait]
impl Strategy for CryptoRlPolicyStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        STRATEGY_NAME
    }

    fn description(&self) -> &str {
        "Canonical no-submit crypto RL policy wrapper for runtime migration"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![
            DataFeed::BinanceSpot {
                symbols: self.symbols.clone(),
            },
            DataFeed::PolymarketEvents {
                series_ids: self.series_ids.clone(),
            },
            DataFeed::Tick {
                interval_ms: self.cfg.tick_interval_ms,
            },
        ]
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        self.apply_market_update(update);
        Ok(Vec::new())
    }

    async fn on_order_update(&mut self, _update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        Ok(Vec::new())
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        self.run_tick(now)
    }

    fn state(&self) -> StrategyStateInfo {
        self.state_info()
    }

    fn positions(&self) -> Vec<PositionInfo> {
        Vec::new()
    }

    fn is_active(&self) -> bool {
        !self.active_events.is_empty()
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.enabled = false;
        Ok(Vec::new())
    }

    fn reset(&mut self) {
        self.reset_runtime_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Side;

    fn minimal_toml() -> &'static str {
        r#"
[strategy]
name = "crypto_rl_policy"
enabled = true

[crypto_rl_policy]
coins = ["BTC"]
tick_interval_ms = 1000
min_time_remaining_secs = 60
max_time_remaining_secs = 300
max_lob_snapshot_age_secs = 2
max_entry_price = 0.70
"#
    }

    fn event_update(now: DateTime<Utc>) -> MarketUpdate {
        MarketUpdate::EventDiscovered {
            event_id: "evt-btc-5m".to_string(),
            series_id: "10684".to_string(),
            up_token: "up-token".to_string(),
            down_token: "down-token".to_string(),
            end_time: now + chrono::Duration::seconds(120),
            price_to_beat: Some(dec!(102500)),
            title: Some("BTC above 102500".to_string()),
            condition_id: None,
        }
    }

    fn price_update(price: i64, ts: DateTime<Utc>) -> MarketUpdate {
        MarketUpdate::BinancePrice {
            symbol: "BTCUSDT".to_string(),
            price: Decimal::from(price),
            timestamp: ts,
        }
    }

    fn l2_update(ts: DateTime<Utc>) -> MarketUpdate {
        MarketUpdate::BinanceL2 {
            symbol: "BTCUSDT".to_string(),
            obi_1: dec!(0.12),
            obi_2: dec!(0.11),
            obi_3: dec!(0.10),
            obi_5: dec!(0.08),
            obi_10: dec!(0.06),
            obi_20: dec!(0.04),
            bid_volume_5: dec!(1200),
            ask_volume_5: dec!(1150),
            spread_bps: dec!(1.8),
            timestamp: ts,
        }
    }

    fn quote_update(
        token_id: &str,
        side: Side,
        bid: Decimal,
        ask: Decimal,
        ts: DateTime<Utc>,
    ) -> MarketUpdate {
        MarketUpdate::PolymarketQuote {
            token_id: token_id.to_string(),
            side,
            quote: Quote {
                side,
                best_bid: Some(bid),
                best_ask: Some(ask),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                timestamp: ts,
            },
            timestamp: ts,
        }
    }

    #[test]
    fn from_toml_builds_expected_feeds() {
        let strategy =
            CryptoRlPolicyStrategy::from_toml("rl-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        let feeds = strategy.required_feeds();
        assert!(feeds.iter().any(|feed| matches!(
            feed,
            DataFeed::BinanceSpot { symbols } if symbols == &vec!["BTCUSDT".to_string()]
        )));
        assert!(feeds.iter().any(|feed| matches!(
            feed,
            DataFeed::PolymarketEvents { series_ids } if series_ids == &vec!["10192".to_string(), "10684".to_string()]
        )));
    }

    #[tokio::test]
    async fn event_discovered_tracks_event_without_legacy_control_actions() {
        let mut strategy =
            CryptoRlPolicyStrategy::from_toml("rl-test".to_string(), minimal_toml(), true)
                .expect("strategy");

        let actions = strategy
            .on_market_update(&event_update(Utc::now()))
            .await
            .expect("event tracked");

        assert!(actions.is_empty());
        assert_eq!(strategy.active_events.len(), 1);
    }

    #[tokio::test]
    async fn on_tick_emits_buy_up_signal_log_when_rule_based_policy_triggers() {
        let mut strategy =
            CryptoRlPolicyStrategy::from_toml("rl-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        let start = Utc::now();
        let tick = start + chrono::Duration::seconds(5);

        strategy
            .on_market_update(&event_update(start))
            .await
            .expect("event tracked");
        strategy
            .on_market_update(&price_update(100000, start))
            .await
            .expect("initial price");
        strategy
            .on_market_update(&price_update(101000, tick))
            .await
            .expect("latest price");
        strategy
            .on_market_update(&l2_update(tick))
            .await
            .expect("l2 update");
        strategy
            .on_market_update(&quote_update(
                "up-token",
                Side::Up,
                dec!(0.45),
                dec!(0.46),
                tick,
            ))
            .await
            .expect("up quote");
        strategy
            .on_market_update(&quote_update(
                "down-token",
                Side::Down,
                dec!(0.47),
                dec!(0.48),
                tick,
            ))
            .await
            .expect("down quote");

        let actions = strategy.on_tick(tick).await.expect("tick");
        assert!(actions.iter().any(|action| matches!(
            action,
            StrategyAction::LogEvent { event }
                if matches!(&event.event_type, StrategyEventType::Custom(kind) if kind == "crypto_rl_policy_signal")
        )));
        assert_eq!(
            strategy.state().metrics.get("last_action"),
            Some(&"buy_up".to_string())
        );
    }
}
