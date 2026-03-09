use super::*;

use crate::strategy::core::SplitArbConfig as CoreSplitArbConfig;

#[path = "split_arb_adapter/config_support.rs"]
mod config_support;
#[path = "split_arb_adapter/runtime_support.rs"]
mod runtime_support;

/// Adapter that wraps split arbitrage strategy logic to implement the Strategy trait.
///
/// Split arbitrage profits when YES + NO tokens can be purchased for less than $1,
/// guaranteeing profit regardless of outcome.
pub struct SplitArbStrategyAdapter {
    /// Strategy ID
    id: String,
    /// Configuration
    config: CoreSplitArbConfig,
    /// Polymarket series IDs to monitor for event discovery.
    series_ids: Vec<String>,
    /// Whether in dry-run mode
    dry_run: bool,
    /// Markets being monitored (market_id -> market)
    markets: Arc<RwLock<HashMap<String, MonitoredMarket>>>,
    /// Partial positions awaiting hedge (market_id -> position)
    partial_positions: Arc<RwLock<HashMap<String, SplitPosition>>>,
    /// Completed hedged positions
    hedged_positions: Arc<RwLock<Vec<HedgedSplitPosition>>>,
    /// Price cache (token_id -> bid/ask)
    prices: Arc<RwLock<HashMap<String, (Option<Decimal>, Option<Decimal>)>>>,
    /// Order-to-market mapping (order_id -> (market_id, side))
    order_market_map: Arc<RwLock<HashMap<String, (String, Side)>>>,
    /// Markets with in-flight Leg1 orders (prevents duplicate entries)
    pending_leg1_markets: Arc<RwLock<HashSet<String>>>,
    /// Fixed USD amount per trade (overrides shares_per_trade when set)
    fixed_amount_usd: Option<f64>,
    /// Stats
    stats: Arc<RwLock<SplitStats>>,
    /// Enabled flag
    enabled: bool,
}

/// A monitored binary market
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct MonitoredMarket {
    market_id: String,
    yes_token_id: String,
    no_token_id: String,
    description: String,
    end_time: DateTime<Utc>,
    /// CTF condition_id for merge/redeem operations
    condition_id: Option<String>,
}

/// A partial (unhedged) position
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct SplitPosition {
    market_id: String,
    first_side: Side,
    first_token_id: String,
    shares: u64,
    entry_price: Decimal,
    opened_at: DateTime<Utc>,
    order_id: Option<String>,
    hedge_retries: u32,
}

/// A fully hedged position
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct HedgedSplitPosition {
    market_id: String,
    yes_token_id: String,
    no_token_id: String,
    shares: u64,
    yes_price: Decimal,
    no_price: Decimal,
    total_cost: Decimal,
    profit_locked: Decimal,
    opened_at: DateTime<Utc>,
}

/// Statistics for split arb
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
struct SplitStats {
    signals_detected: u64,
    first_leg_entries: u64,
    hedges_completed: u64,
    unhedged_exits: u64,
    total_profit: Decimal,
    total_loss: Decimal,
}

fn default_split_arb_series_ids() -> Vec<String> {
    all_updown_series_ids()
}

impl SplitArbStrategyAdapter {
    pub fn new(id: String, config: CoreSplitArbConfig, dry_run: bool) -> Self {
        Self {
            id,
            config,
            series_ids: default_split_arb_series_ids(),
            dry_run,
            markets: Arc::new(RwLock::new(HashMap::new())),
            partial_positions: Arc::new(RwLock::new(HashMap::new())),
            hedged_positions: Arc::new(RwLock::new(Vec::new())),
            prices: Arc::new(RwLock::new(HashMap::new())),
            order_market_map: Arc::new(RwLock::new(HashMap::new())),
            pending_leg1_markets: Arc::new(RwLock::new(HashSet::new())),
            fixed_amount_usd: None,
            stats: Arc::new(RwLock::new(SplitStats::default())),
            enabled: true,
        }
    }

}

#[async_trait]
impl Strategy for SplitArbStrategyAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Split Arbitrage Strategy"
    }

    fn description(&self) -> &str {
        "Buy YES + NO for < $1, profit guaranteed at resolution"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![
            DataFeed::PolymarketEvents {
                series_ids: self.series_ids.clone(),
            },
            DataFeed::Tick { interval_ms: 500 },
        ]
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        match update {
            MarketUpdate::PolymarketQuote {
                token_id, quote, ..
            } => {
                let mut prices = self.prices.write().await;
                prices.insert(token_id.clone(), (quote.best_bid, quote.best_ask));
                drop(prices);

                if !self.enabled {
                    return Ok(actions);
                }

                let market_id = {
                    let markets = self.markets.read().await;
                    markets
                        .iter()
                        .find(|(_, m)| &m.yes_token_id == token_id || &m.no_token_id == token_id)
                        .map(|(id, _)| id.clone())
                };

                if let Some(market_id) = market_id {
                    let has_partial = {
                        let partials = self.partial_positions.read().await;
                        partials.contains_key(&market_id)
                    };

                    if has_partial {
                        let partials = self.partial_positions.read().await;
                        if let Some(partial) = partials.get(&market_id) {
                            let hedge_side = partial.first_side.opposite();
                            let markets = self.markets.read().await;
                            if let Some(market) = markets.get(&market_id) {
                                let hedge_token = match hedge_side {
                                    Side::Up => market.yes_token_id.clone(),
                                    Side::Down => market.no_token_id.clone(),
                                };
                                drop(markets);

                                let prices = self.prices.read().await;
                                if let Some((_, Some(opposite_ask))) = prices.get(&hedge_token) {
                                    let combined = partial.entry_price + *opposite_ask;
                                    let fee_cost = combined * self.config.fee_rate;
                                    if combined + fee_cost < dec!(1.0) {
                                        let profit = dec!(1.0) - combined - fee_cost;
                                        if profit < self.config.min_profit_margin {
                                            return Ok(actions);
                                        }
                                        let hedge_price = *opposite_ask;
                                        let shares = partial.shares;
                                        let partial_market_id = partial.market_id.clone();
                                        drop(prices);
                                        drop(partials);

                                        let client_order_id = format!(
                                            "{}_leg2_{}_{}",
                                            self.id,
                                            partial_market_id,
                                            Utc::now().timestamp_millis()
                                        );

                                        {
                                            let mut map = self.order_market_map.write().await;
                                            map.insert(
                                                client_order_id.clone(),
                                                (partial_market_id.clone(), hedge_side),
                                            );
                                        }

                                        info!(
                                            "[{}] Hedge leg: {} @ {:.2}c (combined {:.2}c, profit {:.2}c)",
                                            self.id,
                                            if hedge_side == Side::Up { "YES" } else { "NO" },
                                            hedge_price * dec!(100),
                                            combined * dec!(100),
                                            profit * dec!(100),
                                        );

                                        actions.push(StrategyAction::LogEvent {
                                            event: StrategyEvent::new(
                                                StrategyEventType::EntryTriggered,
                                                format!(
                                                    "Hedge leg for {}: {} @ {:.0}c, locked profit {:.1}c",
                                                    partial_market_id,
                                                    if hedge_side == Side::Up { "YES" } else { "NO" },
                                                    hedge_price * dec!(100),
                                                    profit * dec!(100),
                                                ),
                                            ),
                                        });

                                        actions.push(super::crypto_submit_intent(
                                            client_order_id,
                                            partial_market_id.clone(),
                                            hedge_token,
                                            hedge_side,
                                            true,
                                            shares,
                                            hedge_price,
                                            10,
                                        ));
                                    }
                                }
                            }
                        }
                    } else {
                        let partials = self.partial_positions.read().await;
                        if partials.len() < self.config.max_unhedged_positions {
                            drop(partials);

                            let pending = self.pending_leg1_markets.read().await;
                            if pending.contains(&market_id) {
                                return Ok(actions);
                            }
                            drop(pending);

                            if let Some((side, price)) = self.check_opportunity(&market_id).await {
                                if let Some(action) =
                                    self.generate_first_leg(&market_id, side, price).await
                                {
                                    self.pending_leg1_markets
                                        .write()
                                        .await
                                        .insert(market_id.clone());

                                    let mut stats = self.stats.write().await;
                                    stats.signals_detected += 1;

                                    actions.push(StrategyAction::LogEvent {
                                        event: StrategyEvent::new(
                                            StrategyEventType::SignalDetected,
                                            format!(
                                                "Split arb opportunity: {} @ {:.0}¢",
                                                market_id,
                                                price * dec!(100)
                                            ),
                                        ),
                                    });

                                    actions.push(action);
                                }
                            }
                        }
                    }
                }
            }

            MarketUpdate::EventDiscovered {
                event_id,
                series_id,
                up_token,
                down_token,
                end_time,
                price_to_beat: _,
                title: _,
                condition_id,
            } => {
                let mut markets = self.markets.write().await;
                markets.insert(
                    event_id.clone(),
                    MonitoredMarket {
                        market_id: event_id.clone(),
                        yes_token_id: up_token.clone(),
                        no_token_id: down_token.clone(),
                        description: format!("Series {}", series_id),
                        end_time: *end_time,
                        condition_id: condition_id.clone(),
                    },
                );

                debug!(
                    "[{}] Market added: {} (YES={}, NO={}, condition={:?})",
                    self.id, event_id, up_token, down_token, condition_id
                );
            }

            MarketUpdate::EventExpired { event_id } => {
                let mut markets = self.markets.write().await;
                markets.remove(event_id);
            }

            _ => {}
        }

        Ok(actions)
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        self.handle_order_update(update).await
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        self.handle_tick(now).await
    }

    fn state(&self) -> StrategyStateInfo {
        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: if self.enabled { "running" } else { "paused" }.to_string(),
            enabled: self.enabled,
            active: true,
            position_count: 0,
            pending_order_count: 0,
            total_exposure: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            realized_pnl_today: Decimal::ZERO,
            last_update: Utc::now(),
            metrics: {
                let mut m = HashMap::new();
                m.insert("dry_run".into(), self.dry_run.to_string());
                m.insert(
                    "target_sum".into(),
                    format!("{:.0}¢", self.config.target_total_cost * dec!(100)),
                );
                m
            },
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        vec![]
    }

    fn is_active(&self) -> bool {
        self.enabled
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        info!("[{}] Shutting down split arb strategy", self.id);
        self.enabled = false;

        let mut actions = Vec::new();
        let partials = self.partial_positions.read().await;
        if !partials.is_empty() {
            warn!(
                "[{}] {} unhedged positions at shutdown!",
                self.id,
                partials.len()
            );
            actions.push(StrategyAction::Alert {
                level: AlertLevel::Error,
                message: format!("{} unhedged positions at shutdown", partials.len()),
            });
        }

        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::StateChanged,
                "Split arb strategy shutdown",
            ),
        });

        Ok(actions)
    }

    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

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
