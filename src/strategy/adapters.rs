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

#[async_trait]
impl Strategy for MomentumStrategyAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Momentum Strategy"
    }

    fn description(&self) -> &str {
        "CEX momentum → Polymarket arbitrage (gabagool22 style)"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![
            DataFeed::BinanceSpot {
                symbols: self.config.symbols.clone(),
            },
            DataFeed::PolymarketEvents {
                series_ids: vec![
                    // 5m windows
                    "10684".into(), // BTC 5m
                    "10683".into(), // ETH 5m
                    "10686".into(), // SOL 5m
                    "10685".into(), // XRP 5m
                    // 15m windows
                    "10192".into(), // BTC 15m
                    "10191".into(), // ETH 15m
                    "10423".into(), // SOL 15m
                    "10422".into(), // XRP 15m
                ],
            },
            DataFeed::Tick { interval_ms: 1000 },
        ]
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        match update {
            MarketUpdate::BinancePrice {
                symbol,
                price,
                timestamp,
            } => {
                // Update CEX price state with history
                let mut prices = self.cex_prices.write().await;
                if let Some(state) = prices.get_mut(symbol) {
                    state.update(*price, *timestamp, self.config.lookback_secs);
                } else {
                    prices.insert(
                        symbol.clone(),
                        CexPriceState::new(symbol.clone(), *price, *timestamp),
                    );
                }
                drop(prices);

                // Check for momentum signal
                if !self.enabled {
                    return Ok(actions);
                }

                // Check limits
                if self.daily_limit_reached().await {
                    return Ok(actions);
                }

                if self.in_cooldown(symbol).await {
                    return Ok(actions);
                }

                // Check position limit
                let positions = self.positions.read().await;
                if positions.len() >= self.config.max_positions {
                    return Ok(actions);
                }

                // Already have position in this symbol?
                if positions.values().any(|p| &p.symbol == symbol) {
                    return Ok(actions);
                }
                drop(positions);

                // === DIRECTIONAL MODE: probability model entry ===
                if self.config.directional_mode {
                    if let Some(action) = self
                        .check_directional_entry(symbol, price, *timestamp)
                        .await
                    {
                        let mut cooldowns = self.cooldowns.write().await;
                        cooldowns.insert(symbol.clone(), Utc::now());
                        actions.push(action);
                    }
                    return Ok(actions);
                }

                // Check for momentum signal
                if let Some((direction, move_pct)) = self.check_momentum(symbol).await {
                    // Get entry price
                    match self.get_entry_price(symbol, direction).await {
                        Some(entry_price) => {
                            // Check entry conditions
                            if entry_price <= self.config.max_entry_price {
                                let fair_value = self.estimate_non_directional_fair_value(move_pct);
                                let edge = fair_value - entry_price;

                                if edge < self.config.min_edge {
                                    debug!(
                                        "[{}] {} {} edge {:.1}% < min {:.1}%, skip",
                                        self.id,
                                        symbol,
                                        direction,
                                        edge * dec!(100),
                                        self.config.min_edge * dec!(100)
                                    );
                                } else {
                                    let ev_net = self
                                        .non_directional_ev_after_costs(fair_value, entry_price)
                                        .unwrap_or(Decimal::ZERO);
                                    if ev_net <= Decimal::ZERO {
                                        debug!(
                                            "[{}] {} {} ev_net {:.2}% <= 0 after fees/slippage, skip",
                                            self.id,
                                            symbol,
                                            direction,
                                            ev_net * dec!(100)
                                        );
                                    } else if let Some(action) =
                                        self.generate_entry(symbol, direction, entry_price).await
                                    {
                                        // Update cooldown
                                        let mut cooldowns = self.cooldowns.write().await;
                                        cooldowns.insert(symbol.clone(), Utc::now());

                                        // Log event
                                        actions.push(StrategyAction::LogEvent {
                                            event: StrategyEvent::new(
                                                StrategyEventType::SignalDetected,
                                                format!(
                                                    "{} {} signal: {:.2}% move, entry {:.0}¢ edge {:.1}% ev {:.1}%",
                                                    symbol,
                                                    direction,
                                                    move_pct * dec!(100),
                                                    entry_price * dec!(100),
                                                    edge * dec!(100),
                                                    ev_net * dec!(100),
                                                ),
                                            ),
                                        });

                                        actions.push(action);
                                    }
                                }
                            } else {
                                debug!(
                                    "[{}] Entry price {:.0}¢ > max {:.0}¢ for {}",
                                    self.id,
                                    entry_price * dec!(100),
                                    self.config.max_entry_price * dec!(100),
                                    symbol
                                );
                            }
                        }
                        None => {
                            // Log why we can't get entry price
                            let events = self.events.read().await;
                            let quotes = self.pm_quotes.read().await;
                            let now = Utc::now();
                            if let Some(event_list) = events.get(symbol) {
                                if let Some(event) =
                                    self.pick_entry_event_in_window(event_list, now)
                                {
                                    let token_id = match direction {
                                        Direction::Up => &event.up_token_id,
                                        Direction::Down => &event.down_token_id,
                                    };
                                    if let Some(q) = quotes.get(token_id) {
                                        debug!(
                                            "[{}] Quote has no best_ask for {} (bid={:?})",
                                            self.id, direction, q.best_bid
                                        );
                                    } else {
                                        debug!(
                                            "[{}] No quote for token {} ({})",
                                            self.id,
                                            &token_id[..8],
                                            direction
                                        );
                                    }
                                } else {
                                    let nearest = event_list
                                        .iter()
                                        .filter(|e| e.end_time > now)
                                        .min_by_key(|e| e.end_time)
                                        .map(|e| (e.end_time - now).num_seconds())
                                        .unwrap_or(-1);
                                    debug!(
                                        "[{}] No event in timing window for {} ({}..{}s, nearest={}s)",
                                        self.id,
                                        symbol,
                                        self.config.min_time_remaining_secs,
                                        self.config.max_time_remaining_secs,
                                        nearest
                                    );
                                }
                            } else {
                                debug!("[{}] No event mapped for symbol {}", self.id, symbol);
                            }
                        }
                    }
                }
            }

            MarketUpdate::BinanceL2 { .. } => {}

            MarketUpdate::PolymarketQuote {
                token_id,
                quote,
                timestamp,
                ..
            } => {
                // Update quote state
                let mut quotes = self.pm_quotes.write().await;
                let is_new = !quotes.contains_key(token_id);
                quotes.insert(
                    token_id.clone(),
                    PmQuoteState {
                        token_id: token_id.clone(),
                        best_bid: quote.best_bid,
                        best_ask: quote.best_ask,
                        timestamp: timestamp.clone(),
                    },
                );
                drop(quotes);

                {
                    let mut positions = self.positions.write().await;
                    if let Some(pos) = positions.get_mut(token_id) {
                        // Mark-to-market with executable side first.
                        pos.current_price = quote.best_bid.or(quote.best_ask);
                    }
                }

                // Log LOB updates (first update or significant changes)
                if is_new {
                    info!(
                        "[{}] 📊 LOB: token {} bid: {}¢ ask: {}¢",
                        self.id,
                        &token_id[..8],
                        quote
                            .best_bid
                            .map(|b| (b * dec!(100)).to_string())
                            .unwrap_or("-".into()),
                        quote
                            .best_ask
                            .map(|a| (a * dec!(100)).to_string())
                            .unwrap_or("-".into())
                    );
                }

                // Check exit conditions for positions
                if !self.config.hold_to_resolution {
                    let trigger = {
                        let positions = self.positions.read().await;
                        positions.get(token_id).and_then(|pos| {
                            let current = pos.current_price?;
                            if pos.entry_price.is_zero() {
                                return None;
                            }
                            let pnl_pct = (current - pos.entry_price) / pos.entry_price;
                            if pnl_pct >= self.exit_config.take_profit_pct {
                                Some((pos.clone(), "take_profit", pnl_pct))
                            } else if pnl_pct <= -self.exit_config.stop_loss_pct {
                                Some((pos.clone(), "stop_loss", pnl_pct))
                            } else {
                                None
                            }
                        })
                    };

                    if let Some((pos, reason, pnl_pct)) = trigger {
                        if self.has_pending_exit_for_token(&pos.token_id).await {
                            return Ok(actions);
                        }

                        let exit_price = match quote.best_bid {
                            Some(p) if p > Decimal::ZERO => p,
                            _ => return Ok(actions),
                        };
                        let client_order_id = format!(
                            "{}_exit_{}_{}",
                            self.id,
                            pos.symbol,
                            Utc::now().timestamp_millis()
                        );
                        let order = OrderRequest::sell_limit(
                            pos.token_id.clone(),
                            pos.side,
                            pos.shares,
                            exit_price,
                        );

                        {
                            let mut pending = self.pending_orders.write().await;
                            pending.insert(
                                client_order_id.clone(),
                                MomentumOrderTrack {
                                    kind: MomentumOrderKind::Exit,
                                    symbol: pos.symbol.clone(),
                                    token_id: pos.token_id.clone(),
                                    side: pos.side,
                                    direction: pos.direction,
                                    shares: pos.shares,
                                    price: exit_price,
                                },
                            );
                        }

                        if reason == "take_profit" {
                            info!(
                                "[{}] Take profit triggered: {} {:.1}% @ {:.2}¢",
                                self.id,
                                pos.symbol,
                                pnl_pct * dec!(100),
                                exit_price * dec!(100)
                            );
                        } else {
                            warn!(
                                "[{}] Stop loss triggered: {} {:.1}% @ {:.2}¢",
                                self.id,
                                pos.symbol,
                                pnl_pct * dec!(100),
                                exit_price * dec!(100)
                            );
                        }

                        actions.push(StrategyAction::SubmitOrder {
                            client_order_id,
                            purpose: crate::strategy::OrderPurpose::from_order_request(&order),
                            order,
                            priority: 8,
                        });
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
                condition_id: _,
            } => {
                // Map series to symbol
                let (symbol, window_secs) = match series_id.as_str() {
                    // 5m windows
                    "10684" => ("BTCUSDT", 300u64),
                    "10683" => ("ETHUSDT", 300),
                    "10686" => ("SOLUSDT", 300),
                    "10685" => ("XRPUSDT", 300),
                    // 15m windows
                    "10192" => ("BTCUSDT", 900),
                    "10191" => ("ETHUSDT", 900),
                    "10423" => ("SOLUSDT", 900),
                    "10422" => ("XRPUSDT", 900),
                    _ => return Ok(actions),
                };

                let mut events = self.events.write().await;
                let event_vec = events.entry(symbol.to_string()).or_default();

                // Prune expired events
                let now = chrono::Utc::now();
                event_vec.retain(|e| e.end_time > now);

                // Dedup by event_id
                if event_vec.iter().any(|e| e.event_id == *event_id) {
                    return Ok(actions);
                }

                // Get current CEX price as S0 for directional mode
                let open_price = if self.config.directional_mode {
                    let prices = self.cex_prices.read().await;
                    prices.get(symbol).map(|s| s.price)
                } else {
                    None
                };

                event_vec.push(EventState {
                    event_id: event_id.clone(),
                    symbol: symbol.to_string(),
                    up_token_id: up_token.clone(),
                    down_token_id: down_token.clone(),
                    end_time: *end_time,
                    open_price,
                    window_secs,
                });

                debug!(
                    "[{}] Event discovered: {} for {} ({}m window, ends {})",
                    self.id,
                    event_id,
                    symbol,
                    window_secs / 60,
                    end_time
                );
            }

            MarketUpdate::EventExpired { event_id } => {
                let mut events = self.events.write().await;
                for list in events.values_mut() {
                    list.retain(|e| &e.event_id != event_id);
                }
            }

            MarketUpdate::BinanceKline { .. } => {}
        }

        Ok(actions)
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        let order_key = update
            .client_order_id
            .clone()
            .unwrap_or_else(|| update.order_id.clone());
        let track = {
            let pending = self.pending_orders.read().await;
            pending.get(&order_key).cloned()
        };

        match update.status {
            crate::domain::OrderStatus::Filled => {
                info!(
                    "[{}] Order filled: {} @ {:?}",
                    self.id, update.order_id, update.avg_fill_price
                );

                if let Some(track) = track {
                    match track.kind {
                        MomentumOrderKind::Entry => {
                            let fill_price = update.avg_fill_price.unwrap_or(track.price);
                            let filled_shares = if update.filled_qty > 0 {
                                update.filled_qty
                            } else {
                                track.shares
                            };

                            {
                                let mut positions = self.positions.write().await;
                                positions.insert(
                                    track.token_id.clone(),
                                    MomentumPosition {
                                        token_id: track.token_id.clone(),
                                        symbol: track.symbol.clone(),
                                        direction: track.direction,
                                        side: track.side,
                                        shares: filled_shares,
                                        entry_price: fill_price,
                                        current_price: Some(fill_price),
                                        opened_at: update.timestamp,
                                        order_id: Some(update.order_id.clone()),
                                    },
                                );
                            }

                            let mut trades = self.daily_trades.write().await;
                            *trades += 1;
                        }
                        MomentumOrderKind::Exit => {
                            let mut positions = self.positions.write().await;
                            positions.remove(&track.token_id);
                        }
                    }

                    self.pending_orders.write().await.remove(&order_key);
                }

                actions.push(StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::OrderFilled,
                        format!("Order {} filled", update.order_id),
                    ),
                });
            }
            crate::domain::OrderStatus::Cancelled => {
                warn!("[{}] Order cancelled: {}", self.id, update.order_id);
                self.pending_orders.write().await.remove(&order_key);
            }
            crate::domain::OrderStatus::Failed => {
                warn!(
                    "[{}] Order failed: {} - {:?}",
                    self.id, update.order_id, update.error
                );

                self.pending_orders.write().await.remove(&order_key);

                actions.push(StrategyAction::Alert {
                    level: AlertLevel::Warning,
                    message: format!("Order failed: {:?}", update.error),
                });
            }
            _ => {}
        }

        Ok(actions)
    }

    async fn on_tick(&mut self, _now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        // Periodic health check / position monitoring
        Ok(vec![])
    }

    fn state(&self) -> StrategyStateInfo {
        let position_count = self.positions.try_read().map(|p| p.len()).unwrap_or(0);
        let pending_count = self.pending_orders.try_read().map(|p| p.len()).unwrap_or(0);
        let total_exposure = self
            .positions
            .try_read()
            .map(|p| {
                p.values()
                    .map(|pos| pos.entry_price * Decimal::from(pos.shares))
                    .sum::<Decimal>()
            })
            .unwrap_or(Decimal::ZERO);

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: if self.enabled { "running" } else { "paused" }.to_string(),
            enabled: self.enabled,
            active: self.enabled,
            position_count,
            pending_order_count: pending_count,
            total_exposure,
            unrealized_pnl: Decimal::ZERO,
            realized_pnl_today: Decimal::ZERO,
            last_update: Utc::now(),
            metrics: {
                let mut m = HashMap::new();
                m.insert(
                    "mode".into(),
                    if self.config.hold_to_resolution {
                        "confirmatory"
                    } else {
                        "predictive"
                    }
                    .into(),
                );
                m.insert("dry_run".into(), self.dry_run.to_string());
                m
            },
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        let positions = match self.positions.try_read() {
            Ok(p) => p,
            Err(_) => return vec![],
        };

        positions
            .values()
            .map(|p| {
                PositionInfo::new(
                    p.token_id.clone(),
                    p.side,
                    p.shares,
                    p.entry_price,
                    self.id.clone(),
                )
            })
            .collect()
    }

    fn is_active(&self) -> bool {
        self.enabled
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        info!("[{}] Shutting down momentum strategy", self.id);
        self.enabled = false;

        let mut actions = Vec::new();

        // Close all positions if not holding to resolution
        if !self.config.hold_to_resolution {
            let positions = self.positions.read().await;
            for pos in positions.values() {
                info!(
                    "[{}] Closing position: {} {} shares @ {:?}",
                    self.id, pos.token_id, pos.shares, pos.current_price
                );

                // Would generate sell order here
            }
        }

        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::StateChanged,
                "Strategy shutdown initiated",
            ),
        });

        Ok(actions)
    }

    fn reset(&mut self) {
        self.positions = Arc::new(RwLock::new(HashMap::new()));
        self.cex_prices = Arc::new(RwLock::new(HashMap::new()));
        self.pm_quotes = Arc::new(RwLock::new(HashMap::new()));
        self.events = Arc::new(RwLock::new(HashMap::new()));
        self.cooldowns = Arc::new(RwLock::new(HashMap::new()));
        self.daily_trades = Arc::new(RwLock::new(0));
        self.last_reset = Arc::new(RwLock::new(Utc::now()));
        self.pending_orders = Arc::new(RwLock::new(HashMap::new()));
    }
}

// ============================================================================
// Split Arbitrage Strategy Adapter
// ============================================================================

use super::core::SplitArbConfig as CoreSplitArbConfig;

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
/// This is a best-effort operation — if it fails, the claimer daemon will
/// pick up the unmerged positions later during its periodic scan.
#[cfg(feature = "pm_ctf")]
async fn spawn_ctf_merge(condition_id: &str, shares: u64) -> std::result::Result<String, String> {
    use alloy::primitives::{B256, U256};
    use polymarket_client_sdk::ctf::types::MergePositionsRequest;
    use std::str::FromStr;

    // USDC on Polygon
    let usdc: alloy::primitives::Address = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174"
        .parse()
        .map_err(|e| format!("{e}"))?;
    let cid = B256::from_str(condition_id).map_err(|e| format!("invalid condition_id: {e}"))?;
    // Polymarket shares are 10^6 (USDC decimals)
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

/// Fallback when CTF feature is not enabled — always returns an error.
#[cfg(not(feature = "pm_ctf"))]
async fn spawn_ctf_merge(_condition_id: &str, _shares: u64) -> std::result::Result<String, String> {
    Err("CTF merge not available (pm_ctf feature disabled)".to_string())
}

fn default_split_arb_series_ids() -> Vec<String> {
    vec![
        "10684".to_string(), // BTC 5m
        "10683".to_string(), // ETH 5m
        "10686".to_string(), // SOL 5m
        "10685".to_string(), // XRP 5m
        "10192".to_string(), // BTC 15m
        "10191".to_string(), // ETH 15m
        "10423".to_string(), // SOL 15m
        "10422".to_string(), // XRP 15m
    ]
}

impl SplitArbStrategyAdapter {
    /// Create a new split arbitrage strategy adapter
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

    /// Create from TOML configuration
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

        // Get target total cost (YES + NO combined)
        let target_sum = entry
            .get("target_sum")
            .and_then(|v| v.as_float())
            .map(|v| if v > 1.0 { v / 100.0 } else { v }) // Handle both cents and decimal
            .unwrap_or(0.98);

        // Max entry for single side (default to half of target_sum)
        let max_entry = entry
            .get("max_entry")
            .and_then(|v| v.as_float())
            .map(|v| if v > 1.0 { v / 100.0 } else { v })
            .unwrap_or(target_sum / 2.0);

        // min_profit threshold
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

    /// Check if a market has an arbitrage opportunity
    async fn check_opportunity(&self, market_id: &str) -> Option<(Side, Decimal)> {
        let markets = self.markets.read().await;
        let market = markets.get(market_id)?;

        let prices = self.prices.read().await;
        let (_yes_bid, yes_ask) = prices.get(&market.yes_token_id)?;
        let (_no_bid, no_ask) = prices.get(&market.no_token_id)?;

        let yes_ask = (*yes_ask)?;
        let no_ask = (*no_ask)?;

        // Check if sum of asks is below target (profit opportunity after fees)
        let total_cost = yes_ask + no_ask;
        let fee_cost = total_cost * self.config.fee_rate;
        if total_cost + fee_cost < dec!(1.0)
            && (dec!(1.0) - total_cost - fee_cost) >= self.config.min_profit_margin
        {
            // Determine which side to enter first (cheaper side)
            if yes_ask <= no_ask && yes_ask <= self.config.max_entry_price {
                return Some((Side::Up, yes_ask));
            } else if no_ask <= self.config.max_entry_price {
                return Some((Side::Down, no_ask));
            }
        }

        None
    }

    /// Generate entry order for first leg
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

        // Calculate shares: use fixed_amount_usd if set, otherwise fall back to config
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

        let order = OrderRequest::buy_limit(token_id.clone(), side, shares, price);

        info!(
            "[{}] First leg entry: {} @ {:.2}¢ ({} shares, ${:.2})",
            self.id,
            if side == Side::Up { "YES" } else { "NO" },
            price * dec!(100),
            shares,
            price.to_string().parse::<f64>().unwrap_or(0.0) * shares as f64,
        );

        // Track order -> market mapping so we can associate fills with positions
        {
            let mut map = self.order_market_map.write().await;
            map.insert(client_order_id.clone(), (market_id.to_string(), side));
        }

        Some(StrategyAction::SubmitOrder {
            client_order_id,
            purpose: crate::strategy::OrderPurpose::from_order_request(&order),
            order,
            priority: 10, // Higher priority for arb
        })
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
            DataFeed::Tick {
                interval_ms: 500, // Fast ticks for arb
            },
        ]
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        match update {
            MarketUpdate::PolymarketQuote {
                token_id, quote, ..
            } => {
                // Update price cache
                let mut prices = self.prices.write().await;
                prices.insert(token_id.clone(), (quote.best_bid, quote.best_ask));
                drop(prices);

                if !self.enabled {
                    return Ok(actions);
                }

                // Find which market this token belongs to
                let market_id = {
                    let markets = self.markets.read().await;
                    markets
                        .iter()
                        .find(|(_, m)| &m.yes_token_id == token_id || &m.no_token_id == token_id)
                        .map(|(id, _)| id.clone())
                };

                if let Some(market_id) = market_id {
                    // Check if we already have a partial position
                    let has_partial = {
                        let partials = self.partial_positions.read().await;
                        partials.contains_key(&market_id)
                    };

                    if has_partial {
                        // Check if we can complete the hedge (second leg)
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
                                            return Ok(actions); // Not enough profit after fees
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

                                        let order = OrderRequest::buy_limit(
                                            hedge_token,
                                            hedge_side,
                                            shares,
                                            hedge_price,
                                        );

                                        // Track hedge order -> market mapping
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

                                        actions.push(StrategyAction::SubmitOrder {
                                            client_order_id,
                                            purpose: crate::strategy::OrderPurpose::Hedge,
                                            order,
                                            priority: 10,
                                        });
                                    }
                                }
                            }
                        }
                    } else {
                        // Check for new opportunity
                        let partials = self.partial_positions.read().await;
                        if partials.len() < self.config.max_unhedged_positions {
                            drop(partials);

                            // Skip if we already have an in-flight Leg1 order for this market
                            let pending = self.pending_leg1_markets.read().await;
                            if pending.contains(&market_id) {
                                return Ok(actions);
                            }
                            drop(pending);

                            if let Some((side, price)) = self.check_opportunity(&market_id).await {
                                if let Some(action) =
                                    self.generate_first_leg(&market_id, side, price).await
                                {
                                    // Mark market as having an in-flight order
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

                // Look up which market/side this order belongs to
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
                        // First leg fill -- create a partial position
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

                        // Clear in-flight flag now that we have a tracked position
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
                        // Hedge leg fill -- complete the arb cycle
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

                            // Auto-merge: convert YES+NO token pair → USDC via CTF
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

                    // Clean up the order mapping
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
                    // Check if this was a hedge (leg2) failure — partial position exists
                    let is_hedge_failure = {
                        let partials = self.partial_positions.read().await;
                        partials.contains_key(&market_id)
                    };

                    if is_hedge_failure {
                        // Hedge order failed — increment retry counter
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
                            // Max retries exceeded — remove orphaned partial and exit leg1
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

                                let order = OrderRequest::sell_limit(
                                    pos.first_token_id.clone(),
                                    pos.first_side,
                                    pos.shares,
                                    exit_price,
                                );

                                actions.push(StrategyAction::SubmitOrder {
                                    client_order_id,
                                    purpose: crate::strategy::OrderPurpose::Exit,
                                    order,
                                    priority: 15,
                                });

                                let mut stats = self.stats.write().await;
                                stats.unhedged_exits += 1;
                            }
                        }
                    } else {
                        // Leg1 failure — just clear the in-flight flag
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

        // Check for hedge timeouts
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

        // Exit timed out positions
        for market_id in timed_out {
            warn!(
                "[{}] Hedge timeout for {}, exiting unhedged",
                self.id, market_id
            );

            let mut partials = self.partial_positions.write().await;
            if let Some(pos) = partials.remove(&market_id) {
                let mut stats = self.stats.write().await;
                stats.unhedged_exits += 1;

                // Generate a sell order to exit the unhedged first leg
                let urgency_buffer = dec!(0.01);
                let exit_price = pos.entry_price - urgency_buffer;
                // Floor at 1 cent to avoid nonsensical prices
                let exit_price = if exit_price < dec!(0.01) {
                    dec!(0.01)
                } else {
                    exit_price
                };

                let client_order_id =
                    format!("{}_exit_{}_{}", self.id, market_id, now.timestamp_millis());

                let order = OrderRequest::sell_limit(
                    pos.first_token_id.clone(),
                    pos.first_side,
                    pos.shares,
                    exit_price,
                );

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

                actions.push(StrategyAction::SubmitOrder {
                    client_order_id,
                    purpose: crate::strategy::OrderPurpose::Exit,
                    order,
                    priority: 8,
                });

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
            position_count: 0, // Would need async
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

        // Log any open positions that need attention
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

    fn reset(&mut self) {
        // Would clear positions
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
