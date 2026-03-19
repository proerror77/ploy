use async_trait::async_trait;
use chrono::{DateTime, Utc};
use toml::Value;

use crate::error::{PloyError, Result};
use crate::plugins::composable_crypto::schema::{
    parse_composable_crypto_spec_toml, ComposableCryptoSchema,
};

use super::adapters::MomentumStrategyAdapter;
use super::traits::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction, StrategyStateInfo,
};

pub struct ComposableCryptoStrategy {
    plugin_id: String,
    schema: ComposableCryptoSchema,
    inner: Box<dyn Strategy>,
}

impl ComposableCryptoStrategy {
    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow::anyhow!("Invalid TOML: {}", e))?;
        let strategy = config
            .get("strategy")
            .and_then(Value::as_table)
            .ok_or_else(|| PloyError::Validation("Missing [strategy] section".to_string()))?;

        let plugin_id = strategy
            .get("plugin_id")
            .and_then(Value::as_str)
            .unwrap_or("crypto.composable.v1")
            .to_string();

        let schema = parse_composable_section(&config)?;
        let inner = build_inner_strategy(&id, config, &schema, dry_run)?;

        Ok(Self {
            plugin_id,
            schema,
            inner,
        })
    }
}

fn parse_composable_section(config: &Value) -> Result<ComposableCryptoSchema> {
    let section = config
        .get("composable_crypto")
        .ok_or_else(|| PloyError::Validation("Missing [composable_crypto] section".to_string()))?;

    parse_composable_crypto_spec_toml(&toml::to_string(section).map_err(|err| {
        PloyError::Validation(format!("invalid composable_crypto section: {err}"))
    })?)
}

fn build_inner_strategy(
    id: &str,
    mut config: Value,
    schema: &ComposableCryptoSchema,
    dry_run: bool,
) -> Result<Box<dyn Strategy>> {
    if schema.signal_blocks.len() != 1 {
        return Err(PloyError::Validation(
            "Composable crypto runtime currently supports exactly one signal block".to_string(),
        ));
    }

    match schema.signal_blocks[0].as_str() {
        "momentum" => {
            let strategy = config
                .get_mut("strategy")
                .and_then(Value::as_table_mut)
                .ok_or_else(|| PloyError::Validation("Missing [strategy] section".to_string()))?;
            strategy.insert("name".to_string(), Value::String("momentum".to_string()));

            let delegate_toml = toml::to_string(&config).map_err(|err| {
                PloyError::Validation(format!(
                    "failed to serialize momentum delegate config: {err}"
                ))
            })?;
            let delegate =
                MomentumStrategyAdapter::from_toml(id.to_string(), &delegate_toml, dry_run)?;
            Ok(Box::new(delegate))
        }
        other => Err(PloyError::Validation(format!(
            "Composable crypto runtime does not support signal block {other} yet"
        ))),
    }
}

#[async_trait]
impl Strategy for ComposableCryptoStrategy {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn name(&self) -> &str {
        "Composable Crypto Strategy"
    }

    fn description(&self) -> &str {
        "Plugin-composed crypto strategy runtime"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        self.inner.required_feeds()
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        self.inner.on_market_update(update).await
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        self.inner.on_order_update(update).await
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        self.inner.on_tick(now).await
    }

    fn state(&self) -> StrategyStateInfo {
        let mut state = self.inner.state();
        state
            .metrics
            .insert("plugin_id".to_string(), self.plugin_id.clone());
        state.metrics.insert(
            "signal_blocks".to_string(),
            self.schema.signal_blocks.join(","),
        );
        state
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.inner.positions()
    }

    fn is_active(&self) -> bool {
        self.inner.is_active()
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.inner.shutdown().await
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::ComposableCryptoStrategy;
    use crate::domain::{OrderStatus, Quote, Side};
    use crate::strategy::{DataFeed, MarketUpdate, Strategy, StrategyAction};
    use chrono::{Duration, Utc};
    use rust_decimal_macros::dec;

    fn composable_momentum_toml() -> &'static str {
        r#"
[strategy]
name = "composable_crypto"
plugin_id = "crypto.momentum.v1"
mode = "predictive"

[entry]
symbols = ["BTCUSDT"]
min_move = 0.5
max_entry = 45.0
min_edge = 5.0
directional_mode = false

[timing]
min_time_remaining = 60
max_time_remaining = 300
cooldown_secs = 0

[risk]
shares = 100
max_positions = 1

[exit]
exit_edge_floor_pct = 2.0
exit_price_band_pct = 5.0

[composable_crypto]
signal_blocks = ["momentum"]

[[composable_crypto.filters]]
type = "volatility_gate"

[[composable_crypto.entry]]
type = "marketable_limit"

[[composable_crypto.exit]]
type = "trailing_stop"

[[composable_crypto.sizing]]
type = "fixed_shares"
"#
    }

    #[test]
    fn momentum_composable_crypto_spec_boots_strategy_runtime() {
        let strategy = ComposableCryptoStrategy::from_toml(
            "test-composable".to_string(),
            composable_momentum_toml(),
            true,
        )
        .expect("composable momentum strategy");

        assert_eq!(strategy.name(), "Composable Crypto Strategy");
        assert!(
            strategy
                .required_feeds()
                .iter()
                .any(|feed| matches!(feed, DataFeed::BinanceSpot { .. })),
            "momentum composable path should inherit Binance spot feeds"
        );
    }

    #[tokio::test]
    async fn momentum_spec_emits_entry_then_exit_intents() {
        let mut strategy = ComposableCryptoStrategy::from_toml(
            "test-composable".to_string(),
            composable_momentum_toml(),
            true,
        )
        .expect("composable momentum strategy");

        let now = Utc::now();
        let up_token = "up-token".to_string();
        let down_token = "down-token".to_string();

        strategy
            .on_market_update(&MarketUpdate::EventDiscovered {
                event_id: "evt-btc-5m".to_string(),
                series_id: "10684".to_string(),
                up_token: up_token.clone(),
                down_token: down_token.clone(),
                end_time: now + Duration::seconds(120),
                price_to_beat: None,
                title: Some("BTC 5m".to_string()),
                condition_id: None,
            })
            .await
            .expect("discover event");

        strategy
            .on_market_update(&MarketUpdate::PolymarketQuote {
                token_id: up_token.clone(),
                side: Side::Up,
                quote: Quote {
                    side: Side::Up,
                    best_bid: Some(dec!(0.39)),
                    best_ask: Some(dec!(0.40)),
                    bid_size: Some(dec!(100)),
                    ask_size: Some(dec!(100)),
                    timestamp: now,
                },
                timestamp: now,
            })
            .await
            .expect("seed entry quote");

        strategy
            .on_market_update(&MarketUpdate::BinancePrice {
                symbol: "BTCUSDT".to_string(),
                price: dec!(100),
                timestamp: now - Duration::seconds(6),
            })
            .await
            .expect("seed price history");

        let actions = strategy
            .on_market_update(&MarketUpdate::BinancePrice {
                symbol: "BTCUSDT".to_string(),
                price: dec!(101),
                timestamp: now,
            })
            .await
            .expect("momentum entry");

        let entry_order = actions
            .into_iter()
            .find_map(|action| match action {
                StrategyAction::SubmitIntent { intent } => Some(intent),
                _ => None,
            })
            .expect("entry order");
        let entry_request = crate::domain::order_request_from_strategy_intent(&entry_order);
        assert!(entry_order.is_buy, "entry intent should be a buy");

        strategy
            .on_order_update(&crate::strategy::OrderUpdate {
                order_id: "entry-order".to_string(),
                client_order_id: Some(entry_order.client_order_id.clone()),
                status: OrderStatus::Filled,
                filled_qty: entry_request.shares,
                avg_fill_price: Some(dec!(0.40)),
                timestamp: now + Duration::seconds(1),
                error: None,
            })
            .await
            .expect("mark entry filled");

        let exit_actions = strategy
            .on_market_update(&MarketUpdate::PolymarketQuote {
                token_id: up_token,
                side: Side::Up,
                quote: Quote {
                    side: Side::Up,
                    best_bid: Some(dec!(0.50)),
                    best_ask: Some(dec!(0.51)),
                    bid_size: Some(dec!(100)),
                    ask_size: Some(dec!(100)),
                    timestamp: now + Duration::seconds(2),
                },
                timestamp: now + Duration::seconds(2),
            })
            .await
            .expect("trigger exit");

        let exit_order = exit_actions.into_iter().find_map(|action| match action {
            StrategyAction::SubmitIntent { intent } => Some(intent),
            _ => None,
        });
        assert_eq!(exit_order.as_ref().map(|intent| intent.is_buy), Some(false));
    }
}
