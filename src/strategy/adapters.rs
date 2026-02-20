//! Strategy Adapters
//!
//! Adapters that wrap legacy strategy implementations to implement the Strategy trait.
//! This enables using existing engines (MomentumEngine, SplitArbEngine) with the new
//! StrategyManager infrastructure.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::momentum::{Direction, ExitConfig, MomentumConfig};
use super::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyEvent, StrategyEventType, StrategyStateInfo,
};
use crate::domain::{OrderRequest, Side};
use crate::error::Result;

// ============================================================================
// Momentum Strategy Adapter
// ============================================================================

/// Adapter that wraps momentum strategy logic to implement the Strategy trait.
///
/// This provides a clean interface for the StrategyManager while reusing
/// the proven momentum detection and execution logic.
pub struct MomentumStrategyAdapter {
    /// Strategy ID
    id: String,
    /// Configuration
    config: MomentumConfig,
    /// Exit configuration
    exit_config: ExitConfig,
    /// Whether in dry-run mode
    dry_run: bool,
    /// Current positions (token_id -> position info)
    positions: Arc<RwLock<HashMap<String, MomentumPosition>>>,
    /// Last CEX prices for momentum detection
    cex_prices: Arc<RwLock<HashMap<String, CexPriceState>>>,
    /// Polymarket quotes (token_id -> quote)
    pm_quotes: Arc<RwLock<HashMap<String, PmQuoteState>>>,
    /// Event mappings (symbol -> event info)
    events: Arc<RwLock<HashMap<String, EventState>>>,
    /// Trade cooldowns (symbol -> last trade time)
    cooldowns: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
    /// Daily trade counter
    daily_trades: Arc<RwLock<u32>>,
    /// Last reset date for daily counter
    last_reset: Arc<RwLock<DateTime<Utc>>>,
    /// Strategy enabled flag
    enabled: bool,
}

/// Price history entry for momentum calculation
#[derive(Debug, Clone)]
struct PriceEntry {
    price: Decimal,
    timestamp: DateTime<Utc>,
}

/// CEX price state with history for momentum detection
#[derive(Debug, Clone)]
struct CexPriceState {
    symbol: String,
    price: Decimal,
    /// Price history for lookback window (stores last N seconds of prices)
    history: Vec<PriceEntry>,
    timestamp: DateTime<Utc>,
}

impl CexPriceState {
    fn new(symbol: String, price: Decimal, timestamp: DateTime<Utc>) -> Self {
        Self {
            symbol,
            price,
            history: vec![PriceEntry { price, timestamp }],
            timestamp,
        }
    }

    /// Add a new price and maintain lookback window
    fn update(&mut self, price: Decimal, timestamp: DateTime<Utc>, lookback_secs: u64) {
        self.price = price;
        self.timestamp = timestamp;
        self.history.push(PriceEntry { price, timestamp });

        // Keep only prices within lookback window + buffer
        let cutoff = timestamp - chrono::Duration::seconds((lookback_secs + 2) as i64);
        self.history.retain(|e| e.timestamp >= cutoff);
    }

    /// Get price from N seconds ago
    fn get_price_at(&self, seconds_ago: u64) -> Option<Decimal> {
        let target_time = self.timestamp - chrono::Duration::seconds(seconds_ago as i64);
        // Find the closest price at or before target_time
        self.history
            .iter()
            .filter(|e| e.timestamp <= target_time)
            .last()
            .map(|e| e.price)
    }
}

/// Polymarket quote state
#[derive(Debug, Clone)]
struct PmQuoteState {
    token_id: String,
    best_bid: Option<Decimal>,
    best_ask: Option<Decimal>,
    timestamp: DateTime<Utc>,
}

/// Event state for tracking active markets
#[derive(Debug, Clone)]
struct EventState {
    event_id: String,
    symbol: String,
    up_token_id: String,
    down_token_id: String,
    end_time: DateTime<Utc>,
}

/// Position in a momentum trade
#[derive(Debug, Clone)]
struct MomentumPosition {
    token_id: String,
    symbol: String,
    direction: Direction,
    side: Side,
    shares: u64,
    entry_price: Decimal,
    current_price: Option<Decimal>,
    opened_at: DateTime<Utc>,
    order_id: Option<String>,
}

impl MomentumStrategyAdapter {
    /// Create a new momentum strategy adapter
    pub fn new(id: String, config: MomentumConfig, exit_config: ExitConfig, dry_run: bool) -> Self {
        Self {
            id,
            config,
            exit_config,
            dry_run,
            positions: Arc::new(RwLock::new(HashMap::new())),
            cex_prices: Arc::new(RwLock::new(HashMap::new())),
            pm_quotes: Arc::new(RwLock::new(HashMap::new())),
            events: Arc::new(RwLock::new(HashMap::new())),
            cooldowns: Arc::new(RwLock::new(HashMap::new())),
            daily_trades: Arc::new(RwLock::new(0)),
            last_reset: Arc::new(RwLock::new(Utc::now())),
            enabled: true,
        }
    }

    /// Create from TOML configuration
    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        use toml::Value;

        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow::anyhow!("Invalid TOML: {}", e))?;

        let empty_table = Value::Table(Default::default());
        let entry = config.get("entry").unwrap_or(&empty_table);
        let exit = config.get("exit").unwrap_or(&empty_table);
        let timing = config.get("timing").unwrap_or(&empty_table);
        let risk = config.get("risk").unwrap_or(&empty_table);
        let strategy = config.get("strategy").unwrap_or(&empty_table);

        let mode = strategy
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("predictive");

        let hold_to_resolution = mode == "confirmatory";

        let symbols: Vec<String> = entry
            .get("symbols")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    "BTCUSDT".into(),
                    "ETHUSDT".into(),
                    "SOLUSDT".into(),
                    "XRPUSDT".into(),
                ]
            });

        // Build baseline volatility map
        let mut baseline_volatility = std::collections::HashMap::new();
        baseline_volatility.insert("BTCUSDT".into(), dec!(0.0005)); // 0.05%
        baseline_volatility.insert("ETHUSDT".into(), dec!(0.0008)); // 0.08%
        baseline_volatility.insert("SOLUSDT".into(), dec!(0.0015)); // 0.15%
        baseline_volatility.insert("XRPUSDT".into(), dec!(0.0012)); // 0.12%

        let momentum_config = MomentumConfig {
            min_move_pct: Decimal::try_from(
                entry
                    .get("min_move")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.05)
                    / 100.0,
            )
            .unwrap_or(dec!(0.0005)),
            max_entry_price: Decimal::try_from(
                entry
                    .get("max_entry")
                    .and_then(|v| v.as_float())
                    .unwrap_or(45.0)
                    / 100.0,
            )
            .unwrap_or(dec!(0.45)),
            min_edge: Decimal::try_from(
                entry
                    .get("min_edge")
                    .and_then(|v| v.as_float())
                    .unwrap_or(5.0)
                    / 100.0,
            )
            .unwrap_or(dec!(0.05)),
            lookback_secs: 5,
            // Multi-timeframe momentum (always enabled) with volatility adjustment
            use_volatility_adjustment: entry
                .get("use_volatility_adjustment")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            baseline_volatility,
            volatility_lookback_secs: entry
                .get("volatility_lookback")
                .and_then(|v| v.as_integer())
                .unwrap_or(60) as u64,
            shares_per_trade: risk
                .get("shares")
                .and_then(|v| v.as_integer())
                .unwrap_or(100) as u64,
            max_positions: risk
                .get("max_positions")
                .and_then(|v| v.as_integer())
                .unwrap_or(5) as usize,
            cooldown_secs: 60,
            max_daily_trades: 50,
            symbols,
            hold_to_resolution,
            min_time_remaining_secs: timing
                .get("min_time_remaining")
                .and_then(|v| v.as_integer())
                .unwrap_or(300) as u64,
            max_time_remaining_secs: timing
                .get("max_time_remaining")
                .and_then(|v| v.as_integer())
                .unwrap_or(900) as u64,
            // Cross-symbol risk control
            max_window_exposure_usd: Decimal::try_from(
                risk.get("max_window_exposure")
                    .and_then(|v| v.as_float())
                    .unwrap_or(25.0),
            )
            .unwrap_or(dec!(25)),
            best_edge_only: risk
                .get("best_edge_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            signal_collection_delay_ms: risk
                .get("signal_delay_ms")
                .and_then(|v| v.as_integer())
                .unwrap_or(2000) as u64,
            // === ENHANCED MOMENTUM DETECTION ===
            require_mtf_agreement: entry
                .get("require_mtf_agreement")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            min_obi_confirmation: Decimal::try_from(
                entry
                    .get("min_obi_confirmation")
                    .and_then(|v| v.as_float())
                    .unwrap_or(5.0)
                    / 100.0,
            )
            .unwrap_or(dec!(0.05)),
            use_kline_volatility: entry
                .get("use_kline_volatility")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            time_decay_factor: Decimal::try_from(
                entry
                    .get("time_decay_factor")
                    .and_then(|v| v.as_float())
                    .unwrap_or(30.0)
                    / 100.0,
            )
            .unwrap_or(dec!(0.30)),
            use_price_to_beat: entry
                .get("use_price_to_beat")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            dynamic_position_sizing: risk
                .get("dynamic_position_sizing")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            min_confidence: entry
                .get("min_confidence")
                .and_then(|v| v.as_float())
                .unwrap_or(0.5),
            use_kelly_sizing: risk
                .get("use_kelly_sizing")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            kelly_fraction_cap: Decimal::try_from(
                risk.get("kelly_fraction_cap")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.25),
            )
            .unwrap_or(dec!(0.25)),

            // VWAP confirmation (legacy momentum config)
            require_vwap_confirmation: entry
                .get("require_vwap_confirmation")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            vwap_lookback_secs: entry
                .get("vwap_lookback_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(60) as u64,
            min_vwap_deviation: Decimal::try_from(
                entry
                    .get("min_vwap_deviation")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.0)
                    / 100.0,
            )
            .unwrap_or(dec!(0)),
        };

        let exit_config = ExitConfig {
            // Binary options semantics:
            // - exit_edge_floor_pct: minimum modeled edge before forced exit
            // - exit_price_band_pct: adverse price-band threshold
            // Keep legacy take_profit/stop_loss keys as backward-compatible aliases.
            take_profit_pct: Decimal::try_from(
                exit.get("exit_edge_floor_pct")
                    .or_else(|| exit.get("take_profit"))
                    .and_then(|v| v.as_float())
                    .unwrap_or(20.0)
                    / 100.0,
            )
            .unwrap_or(dec!(0.20)),
            stop_loss_pct: Decimal::try_from(
                exit.get("exit_price_band_pct")
                    .or_else(|| exit.get("stop_loss"))
                    .and_then(|v| v.as_float())
                    .unwrap_or(12.0)
                    / 100.0,
            )
            .unwrap_or(dec!(0.12)),
            trailing_stop_pct: Decimal::try_from(
                exit.get("trailing_stop")
                    .and_then(|v| v.as_float())
                    .unwrap_or(10.0)
                    / 100.0,
            )
            .unwrap_or(dec!(0.10)),
            exit_before_resolution_secs: exit
                .get("exit_before_resolution")
                .and_then(|v| v.as_integer())
                .unwrap_or(30) as u64,
        };

        Ok(Self::new(id, momentum_config, exit_config, dry_run))
    }

    /// Check if daily trade limit is reached
    async fn daily_limit_reached(&self) -> bool {
        if self.config.max_daily_trades == 0 {
            return false;
        }

        let mut last_reset = self.last_reset.write().await;
        let mut trades = self.daily_trades.write().await;

        // Reset counter on new day
        let now = Utc::now();
        if now.date_naive() != last_reset.date_naive() {
            *trades = 0;
            *last_reset = now;
        }

        *trades >= self.config.max_daily_trades
    }

    /// Check cooldown for a symbol
    async fn in_cooldown(&self, symbol: &str) -> bool {
        let cooldowns = self.cooldowns.read().await;
        if let Some(last_trade) = cooldowns.get(symbol) {
            let elapsed = (Utc::now() - *last_trade).num_seconds();
            elapsed < self.config.cooldown_secs as i64
        } else {
            false
        }
    }

    /// Check for momentum signal based on CEX price move over lookback window
    async fn check_momentum(&self, symbol: &str) -> Option<(Direction, Decimal)> {
        let prices = self.cex_prices.read().await;
        let state = prices.get(symbol)?;

        // Get price from lookback_secs ago
        let base_price = state.get_price_at(self.config.lookback_secs)?;

        if base_price.is_zero() {
            return None;
        }

        let move_pct = (state.price - base_price) / base_price;

        // Log momentum check periodically (every ~100 updates to avoid spam)
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count % 100 == 0 {
            debug!(
                "[{}] {} momentum: {:.4}% (base: ${:.2}, now: ${:.2}, lookback: {}s)",
                self.id,
                symbol,
                move_pct * dec!(100),
                base_price,
                state.price,
                self.config.lookback_secs
            );
        }

        if move_pct.abs() >= self.config.min_move_pct {
            let direction = if move_pct > Decimal::ZERO {
                Direction::Up
            } else {
                Direction::Down
            };
            info!(
                "[{}] 🚀 MOMENTUM SIGNAL: {} {} {:.2}% (${:.2} → ${:.2})",
                self.id,
                symbol,
                direction,
                move_pct.abs() * dec!(100),
                base_price,
                state.price
            );
            Some((direction, move_pct.abs()))
        } else {
            None
        }
    }

    /// Get the best ask price for a direction
    async fn get_entry_price(&self, symbol: &str, direction: Direction) -> Option<Decimal> {
        let events = self.events.read().await;
        let event = events.get(symbol)?;

        let token_id = match direction {
            Direction::Up => &event.up_token_id,
            Direction::Down => &event.down_token_id,
        };

        let quotes = self.pm_quotes.read().await;
        let quote = quotes.get(token_id)?;
        quote.best_ask
    }

    /// Generate entry order action
    async fn generate_entry(
        &self,
        symbol: &str,
        direction: Direction,
        entry_price: Decimal,
    ) -> Option<StrategyAction> {
        let events = self.events.read().await;
        let event = events.get(symbol)?;

        let token_id = match direction {
            Direction::Up => event.up_token_id.clone(),
            Direction::Down => event.down_token_id.clone(),
        };

        let client_order_id = format!(
            "{}_{}_{}_{}",
            self.id,
            symbol,
            direction.to_string().to_lowercase(),
            Utc::now().timestamp_millis()
        );

        // Determine market side based on direction
        let market_side = match direction {
            Direction::Up => Side::Up,
            Direction::Down => Side::Down,
        };

        let order = OrderRequest::buy_limit(
            token_id.clone(),
            market_side,
            self.config.shares_per_trade,
            entry_price,
        );

        info!(
            "[{}] Entry signal: {} {} @ {:.2}¢ ({} shares)",
            self.id,
            direction,
            symbol,
            entry_price * dec!(100),
            self.config.shares_per_trade
        );

        Some(StrategyAction::SubmitOrder {
            client_order_id,
            order,
            priority: 5,
        })
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

                // Check for momentum signal
                if let Some((direction, move_pct)) = self.check_momentum(symbol).await {
                    // Get entry price
                    match self.get_entry_price(symbol, direction).await {
                        Some(entry_price) => {
                            // Check entry conditions
                            if entry_price <= self.config.max_entry_price {
                                if let Some(action) =
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
                                                "{} {} signal: {:.2}% move, entry {:.0}¢",
                                                symbol,
                                                direction,
                                                move_pct * dec!(100),
                                                entry_price * dec!(100)
                                            ),
                                        ),
                                    });

                                    actions.push(action);
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
                            if let Some(event) = events.get(symbol) {
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
                                debug!("[{}] No event mapped for symbol {}", self.id, symbol);
                            }
                        }
                    }
                }
            }

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
                    let positions = self.positions.read().await;
                    for pos in positions.values() {
                        if &pos.token_id == token_id {
                            // Check take profit / stop loss
                            if let Some(current) = pos.current_price {
                                let pnl_pct = (current - pos.entry_price) / pos.entry_price;

                                if pnl_pct >= self.exit_config.take_profit_pct {
                                    info!(
                                        "[{}] Take profit triggered: {:.1}%",
                                        self.id,
                                        pnl_pct * dec!(100)
                                    );
                                    // Would generate exit order
                                } else if pnl_pct <= -self.exit_config.stop_loss_pct {
                                    warn!(
                                        "[{}] Stop loss triggered: {:.1}%",
                                        self.id,
                                        pnl_pct * dec!(100)
                                    );
                                    // Would generate exit order
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
            } => {
                // Map series to symbol
                let symbol = match series_id.as_str() {
                    "10192" => "BTCUSDT",
                    "10191" => "ETHUSDT",
                    "10423" => "SOLUSDT",
                    "10422" => "XRPUSDT",
                    _ => return Ok(actions),
                };

                let mut events = self.events.write().await;
                events.insert(
                    symbol.to_string(),
                    EventState {
                        event_id: event_id.clone(),
                        symbol: symbol.to_string(),
                        up_token_id: up_token.clone(),
                        down_token_id: down_token.clone(),
                        end_time: *end_time,
                    },
                );

                debug!(
                    "[{}] Event discovered: {} for {}",
                    self.id, event_id, symbol
                );
            }

            MarketUpdate::EventExpired { event_id } => {
                let mut events = self.events.write().await;
                events.retain(|_, e| &e.event_id != event_id);
            }

            MarketUpdate::BinanceKline { .. } => {}
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

                // Increment daily trade counter
                let mut trades = self.daily_trades.write().await;
                *trades += 1;

                actions.push(StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::OrderFilled,
                        format!("Order {} filled", update.order_id),
                    ),
                });
            }
            crate::domain::OrderStatus::Cancelled => {
                warn!("[{}] Order cancelled: {}", self.id, update.order_id);
            }
            crate::domain::OrderStatus::Failed => {
                warn!(
                    "[{}] Order failed: {} - {:?}",
                    self.id, update.order_id, update.error
                );

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
        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: if self.enabled { "running" } else { "paused" }.to_string(),
            enabled: self.enabled,
            active: true,
            position_count: 0, // Would need async access
            pending_order_count: 0,
            total_exposure: Decimal::ZERO,
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
        // Would need to synchronously get positions - return empty for now
        // In practice, would use try_read or cache the positions
        vec![]
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
        // Clear state for new session
        // Would need blocking access or restructure
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
    /// Stats
    stats: Arc<RwLock<SplitStats>>,
    /// Enabled flag
    enabled: bool,
}

/// A monitored binary market
#[derive(Debug, Clone)]
struct MonitoredMarket {
    market_id: String,
    yes_token_id: String,
    no_token_id: String,
    description: String,
    end_time: DateTime<Utc>,
}

/// A partial (unhedged) position
#[derive(Debug, Clone)]
struct SplitPosition {
    market_id: String,
    first_side: Side,
    first_token_id: String,
    shares: u64,
    entry_price: Decimal,
    opened_at: DateTime<Utc>,
    order_id: Option<String>,
}

/// A fully hedged position
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
#[derive(Debug, Clone, Default)]
struct SplitStats {
    signals_detected: u64,
    first_leg_entries: u64,
    hedges_completed: u64,
    unhedged_exits: u64,
    total_profit: Decimal,
    total_loss: Decimal,
}

impl SplitArbStrategyAdapter {
    /// Create a new split arbitrage strategy adapter
    pub fn new(id: String, config: CoreSplitArbConfig, dry_run: bool) -> Self {
        Self {
            id,
            config,
            dry_run,
            markets: Arc::new(RwLock::new(HashMap::new())),
            partial_positions: Arc::new(RwLock::new(HashMap::new())),
            hedged_positions: Arc::new(RwLock::new(Vec::new())),
            prices: Arc::new(RwLock::new(HashMap::new())),
            order_market_map: Arc::new(RwLock::new(HashMap::new())),
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
        let position = config.get("position").unwrap_or(&empty_table);
        let risk = config.get("risk").unwrap_or(&empty_table);

        // Support both config field naming conventions
        // Legacy: max_combined_price = total for YES+NO (e.g., 98 cents)
        // New: target_sum = same meaning, max_entry = single side max

        // Get target total cost (YES + NO combined)
        let target_sum = entry
            .get("target_sum")
            .or_else(|| entry.get("max_combined_price"))
            .and_then(|v| v.as_float())
            .map(|v| if v > 1.0 { v / 100.0 } else { v }) // Handle both cents and decimal
            .unwrap_or(0.98);

        // Max entry for single side (default to half of target_sum)
        let max_entry = entry
            .get("max_entry")
            .and_then(|v| v.as_float())
            .map(|v| if v > 1.0 { v / 100.0 } else { v })
            .unwrap_or(target_sum / 2.0);

        // min_profit (new) or min_spread (legacy)
        let min_profit = entry
            .get("min_profit")
            .or_else(|| entry.get("min_spread"))
            .and_then(|v| v.as_float())
            .map(|v| if v > 1.0 { v / 100.0 } else { v })
            .unwrap_or(0.02);

        // shares: risk.shares (new) or position.shares_per_side (legacy)
        let shares = risk
            .get("shares")
            .or_else(|| position.get("shares_per_side"))
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
                .or_else(|| position.get("max_positions"))
                .and_then(|v| v.as_integer())
                .unwrap_or(3) as usize,
            unhedged_stop_loss: Decimal::try_from(
                risk.get("unhedged_stop")
                    .and_then(|v| v.as_float())
                    .unwrap_or(10.0)
                    / 100.0,
            )
            .unwrap_or(dec!(0.10)),
        };

        Ok(Self::new(id, split_config, dry_run))
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

        // Check if sum of asks is below target (profit opportunity)
        let total_cost = yes_ask + no_ask;
        if total_cost < self.config.target_total_cost {
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

        let order =
            OrderRequest::buy_limit(token_id.clone(), side, self.config.shares_per_trade, price);

        info!(
            "[{}] First leg entry: {} @ {:.2}¢ ({} shares)",
            self.id,
            if side == Side::Up { "YES" } else { "NO" },
            price * dec!(100),
            self.config.shares_per_trade
        );

        // Track order -> market mapping so we can associate fills with positions
        {
            let mut map = self.order_market_map.write().await;
            map.insert(client_order_id.clone(), (market_id.to_string(), side));
        }

        Some(StrategyAction::SubmitOrder {
            client_order_id,
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
                series_ids: vec![], // Will be configured per market type
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
                                    let fee_buffer = dec!(0.02);
                                    let combined = partial.entry_price + *opposite_ask;
                                    if combined < dec!(1.0) - fee_buffer {
                                        let profit = dec!(1.0) - combined - fee_buffer;
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

                            if let Some((side, price)) = self.check_opportunity(&market_id).await {
                                if let Some(action) =
                                    self.generate_first_leg(&market_id, side, price).await
                                {
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
                    },
                );

                debug!(
                    "[{}] Market added: {} (YES={}, NO={})",
                    self.id, event_id, up_token, down_token
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
                        };

                        let mut partials = self.partial_positions.write().await;
                        partials.insert(market_id.clone(), partial);

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
                            let profit = dec!(1.0) - total_cost;

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
"#;

        let adapter = SplitArbStrategyAdapter::from_toml("test".into(), toml, true).unwrap();

        assert_eq!(adapter.config.max_entry_price, dec!(0.35));
        assert_eq!(adapter.config.target_total_cost, dec!(0.70));
        assert_eq!(adapter.config.shares_per_trade, 100);
    }

    #[test]
    fn test_split_arb_from_legacy_toml() {
        // Test with legacy config format (using cents and max_combined_price)
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

        let adapter = SplitArbStrategyAdapter::from_toml("test".into(), toml, true).unwrap();

        // max_combined_price maps to target_total_cost
        assert_eq!(adapter.config.target_total_cost, dec!(0.98));
        // max_entry defaults to half of target_sum
        assert_eq!(adapter.config.max_entry_price, dec!(0.49));
        assert_eq!(adapter.config.shares_per_trade, 50);
        assert_eq!(adapter.config.max_unhedged_positions, 10);
    }
}
