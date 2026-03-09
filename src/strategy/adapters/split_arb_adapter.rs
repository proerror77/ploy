use super::*;

use crate::strategy::core::SplitArbConfig as CoreSplitArbConfig;

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

/// Spawn a CTF merge transaction to convert YES+NO token pair back to USDC.
///
/// This is a best-effort operation. If it fails, the claimer daemon will
/// pick up the unmerged positions later during its periodic scan.
#[cfg(feature = "pm_ctf")]
async fn spawn_ctf_merge(condition_id: &str, shares: u64) -> std::result::Result<String, String> {
    use alloy::primitives::{B256, U256};
    use polymarket_client_sdk::ctf::types::MergePositionsRequest;
    use std::str::FromStr;

    let usdc: alloy::primitives::Address = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174"
        .parse()
        .map_err(|e| format!("{e}"))?;
    let cid = B256::from_str(condition_id).map_err(|e| format!("invalid condition_id: {e}"))?;
    let amount = U256::from(shares) * U256::from(1_000_000u64);

    let pk = std::env::var("POLYMARKET_PRIVATE_KEY")
        .map_err(|_| "POLYMARKET_PRIVATE_KEY not set".to_string())?;

    let signer: alloy::signers::local::PrivateKeySigner = pk
        .parse()
        .map_err(|e| format!("invalid private key: {e}"))?;
    let wallet = alloy::network::EthereumWallet::from(signer);

    let rpc_url =
        std::env::var("POLYGON_RPC_URL").unwrap_or_else(|_| "https://polygon-rpc.com".to_string());

    let provider = alloy::providers::ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url.parse().map_err(|e| format!("bad RPC URL: {e}"))?);

    let client = polymarket_client_sdk::ctf::Client::new(provider, polymarket_client_sdk::POLYGON)
        .map_err(|e| format!("CTF client init: {e}"))?;

    let request = MergePositionsRequest::for_binary_market(usdc, cid, amount);
    let resp = client
        .merge_positions(&request)
        .await
        .map_err(|e| format!("merge tx failed: {e}"))?;

    Ok(format!("{:?}", resp.transaction_hash))
}

#[cfg(not(feature = "pm_ctf"))]
async fn spawn_ctf_merge(
    _condition_id: &str,
    _shares: u64,
) -> std::result::Result<String, String> {
    Err("CTF merge not available (pm_ctf feature disabled)".to_string())
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

    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        use toml::Value;

        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow::anyhow!("Invalid TOML: {}", e))?;

        let empty_table = Value::Table(Default::default());
        let _strategy = config.get("strategy").unwrap_or(&empty_table);
        let entry = config.get("entry").unwrap_or(&empty_table);
        let risk = config.get("risk").unwrap_or(&empty_table);
        let position = config.get("position").unwrap_or(&empty_table);
        let markets = config.get("markets").unwrap_or(&empty_table);

        if entry.get("max_combined_price").is_some() {
            return Err(crate::error::PloyError::Validation(
                "deprecated key `entry.max_combined_price` is no longer supported; use `entry.target_sum`"
                    .to_string(),
            ));
        }
        if entry.get("min_spread").is_some() {
            return Err(crate::error::PloyError::Validation(
                "deprecated key `entry.min_spread` is no longer supported; use `entry.min_profit`"
                    .to_string(),
            ));
        }
        if position.get("shares_per_side").is_some() {
            return Err(crate::error::PloyError::Validation(
                "deprecated key `position.shares_per_side` is no longer supported; use `risk.shares`"
                    .to_string(),
            ));
        }
        if position.get("max_positions").is_some() {
            return Err(crate::error::PloyError::Validation(
                "deprecated key `position.max_positions` is no longer supported; use `risk.max_unhedged`"
                    .to_string(),
            ));
        }

        let target_sum = entry
            .get("target_sum")
            .and_then(|v| v.as_float())
            .map(|v| if v > 1.0 { v / 100.0 } else { v })
            .unwrap_or(0.98);

        let max_entry = entry
            .get("max_entry")
            .and_then(|v| v.as_float())
            .map(|v| if v > 1.0 { v / 100.0 } else { v })
            .unwrap_or(target_sum / 2.0);

        let min_profit = entry
            .get("min_profit")
            .and_then(|v| v.as_float())
            .map(|v| if v > 1.0 { v / 100.0 } else { v })
            .unwrap_or(0.02);

        let shares = risk
            .get("shares")
            .and_then(|v| v.as_integer())
            .unwrap_or(50) as u64;

        let split_config = CoreSplitArbConfig {
            max_entry_price: Decimal::try_from(max_entry).unwrap_or(dec!(0.49)),
            target_total_cost: Decimal::try_from(target_sum).unwrap_or(dec!(0.98)),
            min_profit_margin: Decimal::try_from(min_profit).unwrap_or(dec!(0.02)),
            max_hedge_wait_secs: risk
                .get("max_hedge_wait")
                .and_then(|v| v.as_integer())
                .unwrap_or(30) as u64,
            shares_per_trade: shares,
            max_unhedged_positions: risk
                .get("max_unhedged")
                .and_then(|v| v.as_integer())
                .unwrap_or(3) as usize,
            unhedged_stop_loss: Decimal::try_from(
                risk.get("unhedged_stop")
                    .and_then(|v| v.as_float())
                    .unwrap_or(10.0)
                    / 100.0,
            )
            .unwrap_or(dec!(0.10)),
            fee_rate: Decimal::try_from(
                risk.get("fee_rate")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.02),
            )
            .unwrap_or(dec!(0.02)),
        };
        let mut series_ids: Vec<String> = markets
            .get("series_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if series_ids.is_empty() {
            series_ids = default_split_arb_series_ids();
        } else {
            series_ids.sort();
            series_ids.dedup();
        }

        let mut adapter = Self::new(id, split_config, dry_run);
        adapter.series_ids = series_ids;
        adapter.fixed_amount_usd = risk.get("fixed_amount_usd").and_then(|v| v.as_float());
        Ok(adapter)
    }

    async fn check_opportunity(&self, market_id: &str) -> Option<(Side, Decimal)> {
        let markets = self.markets.read().await;
        let market = markets.get(market_id)?;

        let prices = self.prices.read().await;
        let (_yes_bid, yes_ask) = prices.get(&market.yes_token_id)?;
        let (_no_bid, no_ask) = prices.get(&market.no_token_id)?;

        let yes_ask = (*yes_ask)?;
        let no_ask = (*no_ask)?;

        let total_cost = yes_ask + no_ask;
        let fee_cost = total_cost * self.config.fee_rate;
        if total_cost + fee_cost < dec!(1.0)
            && (dec!(1.0) - total_cost - fee_cost) >= self.config.min_profit_margin
        {
            if yes_ask <= no_ask && yes_ask <= self.config.max_entry_price {
                return Some((Side::Up, yes_ask));
            } else if no_ask <= self.config.max_entry_price {
                return Some((Side::Down, no_ask));
            }
        }

        None
    }

    async fn generate_first_leg(
        &self,
        market_id: &str,
        side: Side,
        price: Decimal,
    ) -> Option<StrategyAction> {
        let markets = self.markets.read().await;
        let market = markets.get(market_id)?;

        let token_id = match side {
            Side::Up => market.yes_token_id.clone(),
            Side::Down => market.no_token_id.clone(),
        };

        let client_order_id = format!(
            "{}_leg1_{}_{}",
            self.id,
            market_id,
            Utc::now().timestamp_millis()
        );

        let shares = if let Some(amount_usd) = self.fixed_amount_usd {
            let price_f64 = price.to_string().parse::<f64>().unwrap_or(0.5);
            if price_f64 > 0.0 {
                (amount_usd / price_f64).floor() as u64
            } else {
                self.config.shares_per_trade
            }
        } else {
            self.config.shares_per_trade
        };

        info!(
            "[{}] First leg entry: {} @ {:.2}¢ ({} shares, ${:.2})",
            self.id,
            if side == Side::Up { "YES" } else { "NO" },
            price * dec!(100),
            shares,
            price.to_string().parse::<f64>().unwrap_or(0.0) * shares as f64,
        );

        {
            let mut map = self.order_market_map.write().await;
            map.insert(client_order_id.clone(), (market_id.to_string(), side));
        }

        Some(super::crypto_submit_intent(
            client_order_id,
            market_id.to_string(),
            token_id,
            side,
            true,
            shares,
            price,
            10,
        ))
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
        let mut actions = Vec::new();

        match update.status {
            crate::domain::OrderStatus::Filled => {
                info!(
                    "[{}] Order filled: {} @ {:?}",
                    self.id, update.order_id, update.avg_fill_price
                );

                let order_key = update
                    .client_order_id
                    .clone()
                    .unwrap_or_else(|| update.order_id.clone());
                let mapping = {
                    let map = self.order_market_map.read().await;
                    map.get(&order_key).cloned()
                };

                if let Some((market_id, side)) = mapping {
                    let fill_price = update.avg_fill_price.unwrap_or(Decimal::ZERO);
                    let has_partial = {
                        let partials = self.partial_positions.read().await;
                        partials.contains_key(&market_id)
                    };

                    if !has_partial {
                        let markets = self.markets.read().await;
                        let token_id = markets
                            .get(&market_id)
                            .map(|m| match side {
                                Side::Up => m.yes_token_id.clone(),
                                Side::Down => m.no_token_id.clone(),
                            })
                            .unwrap_or_default();
                        drop(markets);

                        let partial = SplitPosition {
                            market_id: market_id.clone(),
                            first_side: side,
                            first_token_id: token_id,
                            shares: update.filled_qty,
                            entry_price: fill_price,
                            opened_at: Utc::now(),
                            order_id: Some(order_key.clone()),
                            hedge_retries: 0,
                        };

                        let mut partials = self.partial_positions.write().await;
                        partials.insert(market_id.clone(), partial);

                        self.pending_leg1_markets.write().await.remove(&market_id);

                        let mut stats = self.stats.write().await;
                        stats.first_leg_entries += 1;

                        info!(
                            "[{}] First leg tracked: {} {} @ {:.2}c",
                            self.id,
                            market_id,
                            if side == Side::Up { "YES" } else { "NO" },
                            fill_price * dec!(100)
                        );
                    } else {
                        let mut partials = self.partial_positions.write().await;
                        if let Some(partial) = partials.remove(&market_id) {
                            let total_cost = partial.entry_price + fill_price;
                            let fee_cost = total_cost * self.config.fee_rate;
                            let profit = dec!(1.0) - total_cost - fee_cost;

                            let markets = self.markets.read().await;
                            let (yes_token, no_token, yes_price, no_price) =
                                if let Some(m) = markets.get(&market_id) {
                                    match partial.first_side {
                                        Side::Up => (
                                            m.yes_token_id.clone(),
                                            m.no_token_id.clone(),
                                            partial.entry_price,
                                            fill_price,
                                        ),
                                        Side::Down => (
                                            m.yes_token_id.clone(),
                                            m.no_token_id.clone(),
                                            fill_price,
                                            partial.entry_price,
                                        ),
                                    }
                                } else {
                                    (
                                        String::new(),
                                        String::new(),
                                        partial.entry_price,
                                        fill_price,
                                    )
                                };
                            drop(markets);

                            let hedged = HedgedSplitPosition {
                                market_id: market_id.clone(),
                                yes_token_id: yes_token,
                                no_token_id: no_token,
                                shares: partial.shares,
                                yes_price,
                                no_price,
                                total_cost,
                                profit_locked: profit,
                                opened_at: partial.opened_at,
                            };

                            let mut hedged_positions = self.hedged_positions.write().await;
                            hedged_positions.push(hedged);

                            let mut stats = self.stats.write().await;
                            stats.hedges_completed += 1;
                            stats.total_profit += profit * Decimal::from(partial.shares);

                            info!(
                                "[{}] Hedge complete: {} cost={:.2}c profit={:.2}c/share ({} shares)",
                                self.id, market_id,
                                total_cost * dec!(100),
                                profit * dec!(100),
                                partial.shares,
                            );

                            let markets = self.markets.read().await;
                            let merge_condition_id =
                                markets.get(&market_id).and_then(|m| m.condition_id.clone());
                            drop(markets);

                            if let Some(cid) = merge_condition_id {
                                let merge_shares = partial.shares;
                                let merge_market_id = market_id.clone();
                                let strategy_id = self.id.clone();
                                let dry_run = self.dry_run;
                                tokio::spawn(async move {
                                    if dry_run {
                                        info!(
                                            "[{}] CTF merge skipped (dry-run): {} shares for {}",
                                            strategy_id, merge_shares, merge_market_id
                                        );
                                        return;
                                    }
                                    match spawn_ctf_merge(&cid, merge_shares).await {
                                        Ok(tx_hash) => {
                                            info!(
                                                "[{}] CTF merge successful: {} shares for {} (tx: {})",
                                                strategy_id, merge_shares, merge_market_id, tx_hash
                                            );
                                        }
                                        Err(e) => {
                                            warn!(
                                                "[{}] CTF merge failed for {} ({} shares): {} — claimer will pick up later",
                                                strategy_id, merge_market_id, merge_shares, e
                                            );
                                        }
                                    }
                                });
                            } else {
                                warn!(
                                    "[{}] No condition_id for market {} — skipping auto-merge, claimer will handle",
                                    self.id, market_id
                                );
                            }
                        }
                    }

                    let mut map = self.order_market_map.write().await;
                    map.remove(&order_key);
                }

                actions.push(StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::OrderFilled,
                        format!("Split arb leg filled: {}", update.order_id),
                    ),
                });
            }
            crate::domain::OrderStatus::Cancelled | crate::domain::OrderStatus::Failed => {
                warn!(
                    "[{}] Order {} - {:?}",
                    self.id, update.order_id, update.error
                );
                let order_key = update
                    .client_order_id
                    .clone()
                    .unwrap_or_else(|| update.order_id.clone());
                let mapping = self.order_market_map.read().await.get(&order_key).cloned();

                if let Some((market_id, _side)) = mapping {
                    let is_hedge_failure = {
                        let partials = self.partial_positions.read().await;
                        partials.contains_key(&market_id)
                    };

                    if is_hedge_failure {
                        const MAX_HEDGE_RETRIES: u32 = 3;
                        let mut partials = self.partial_positions.write().await;
                        let should_exit = if let Some(pos) = partials.get_mut(&market_id) {
                            pos.hedge_retries += 1;
                            warn!(
                                "[{}] Hedge retry {}/{} for {}",
                                self.id, pos.hedge_retries, MAX_HEDGE_RETRIES, market_id
                            );
                            pos.hedge_retries >= MAX_HEDGE_RETRIES
                        } else {
                            false
                        };

                        if should_exit {
                            if let Some(pos) = partials.remove(&market_id) {
                                warn!(
                                    "[{}] Removing orphaned partial for {} after {} hedge failures",
                                    self.id, market_id, MAX_HEDGE_RETRIES
                                );

                                let urgency_buffer = dec!(0.01);
                                let exit_price = pos.entry_price - urgency_buffer;
                                let exit_price = if exit_price < dec!(0.01) {
                                    dec!(0.01)
                                } else {
                                    exit_price
                                };

                                let client_order_id = format!(
                                    "{}_orphan_exit_{}_{}",
                                    self.id,
                                    market_id,
                                    Utc::now().timestamp_millis()
                                );

                                actions.push(super::crypto_submit_intent(
                                    client_order_id,
                                    market_id.clone(),
                                    pos.first_token_id.clone(),
                                    pos.first_side,
                                    false,
                                    pos.shares,
                                    exit_price,
                                    15,
                                ));

                                let mut stats = self.stats.write().await;
                                stats.unhedged_exits += 1;
                            }
                        }
                    } else {
                        self.pending_leg1_markets.write().await.remove(&market_id);
                    }
                }

                self.order_market_map.write().await.remove(&order_key);
            }
            _ => {}
        }

        Ok(actions)
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();
        let mut timed_out = Vec::new();
        {
            let partials = self.partial_positions.read().await;
            for (market_id, pos) in partials.iter() {
                let elapsed = (now - pos.opened_at).num_seconds() as u64;
                if elapsed > self.config.max_hedge_wait_secs {
                    timed_out.push(market_id.clone());
                }
            }
        }

        for market_id in timed_out {
            warn!(
                "[{}] Hedge timeout for {}, exiting unhedged",
                self.id, market_id
            );

            let mut partials = self.partial_positions.write().await;
            if let Some(pos) = partials.remove(&market_id) {
                let mut stats = self.stats.write().await;
                stats.unhedged_exits += 1;

                let urgency_buffer = dec!(0.01);
                let exit_price = pos.entry_price - urgency_buffer;
                let exit_price = if exit_price < dec!(0.01) {
                    dec!(0.01)
                } else {
                    exit_price
                };

                let client_order_id =
                    format!("{}_exit_{}_{}", self.id, market_id, now.timestamp_millis());

                info!(
                    "[{}] Unhedged exit: {} {} @ {:.2}c ({} shares)",
                    self.id,
                    market_id,
                    if pos.first_side == Side::Up {
                        "YES"
                    } else {
                        "NO"
                    },
                    exit_price * dec!(100),
                    pos.shares,
                );

                actions.push(super::crypto_submit_intent(
                    client_order_id,
                    market_id.clone(),
                    pos.first_token_id.clone(),
                    pos.first_side,
                    false,
                    pos.shares,
                    exit_price,
                    8,
                ));

                actions.push(StrategyAction::Alert {
                    level: AlertLevel::Warning,
                    message: format!("Unhedged exit: {}", market_id),
                });
            }
        }

        Ok(actions)
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
