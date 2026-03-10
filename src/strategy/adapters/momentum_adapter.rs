use super::*;
use rust_decimal::prelude::ToPrimitive;

use crate::strategy::volatility::normal_cdf;

#[path = "momentum_adapter/config_support.rs"]
mod config_support;

fn database_url_from_env() -> Option<String> {
    std::env::var("PLOY_DATABASE__URL")
        .ok()
        .or_else(|| std::env::var("PLOY__DATABASE__URL").ok())
        .or_else(|| std::env::var("PLOY_DATABASE_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn account_id_from_env() -> String {
    std::env::var("PLOY_ACCOUNT__ID")
        .ok()
        .or_else(|| std::env::var("PLOY__ACCOUNT__ID").ok())
        .or_else(|| std::env::var("PLOY_ACCOUNT_ID").ok())
        .unwrap_or_else(|| "default".to_string())
        .trim()
        .to_string()
}

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
    positions: HashMap<String, MomentumPosition>,
    /// Last CEX prices for momentum detection
    cex_prices: HashMap<String, CexPriceState>,
    /// Polymarket quotes (token_id -> quote)
    pm_quotes: HashMap<String, PmQuoteState>,
    /// Event mappings (symbol -> upcoming events, sorted by end_time)
    events: HashMap<String, Vec<EventState>>,
    /// Trade cooldowns (symbol -> last trade time)
    cooldowns: HashMap<String, DateTime<Utc>>,
    /// Daily trade counter
    daily_trades: u32,
    /// Last reset date for daily counter
    last_reset: DateTime<Utc>,
    /// Strategy enabled flag
    enabled: bool,
    /// Optional Postgres pool for recording dry-run signals (signal_history).
    signal_log_pool: OnceCell<Arc<sqlx::PgPool>>,
    signal_log_ready: OnceCell<()>,
    /// Fixed USD amount per trade (overrides shares_per_trade when set)
    fixed_amount_usd: Option<f64>,
    /// Minimum directional EV required after fees/slippage.
    directional_entry_threshold: f64,
    /// In-flight entry/exit orders keyed by client_order_id.
    pending_orders: HashMap<String, MomentumOrderTrack>,
}

/// Price history entry for momentum calculation
#[derive(Debug, Clone)]
struct PriceEntry {
    price: Decimal,
    timestamp: DateTime<Utc>,
}

/// CEX price state with history for momentum detection
#[allow(dead_code)]
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
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct PmQuoteState {
    token_id: String,
    best_bid: Option<Decimal>,
    best_ask: Option<Decimal>,
    timestamp: DateTime<Utc>,
}

/// Event state for tracking active markets
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct EventState {
    event_id: String,
    symbol: String,
    up_token_id: String,
    down_token_id: String,
    end_time: DateTime<Utc>,
    /// CEX price when event was first discovered (used as S0 for directional mode)
    open_price: Option<Decimal>,
    /// Window duration in seconds (300 = 5m, 900 = 15m)
    window_secs: u64,
}

/// Position in a momentum trade
#[allow(dead_code)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MomentumOrderKind {
    Entry,
    Exit,
}

#[derive(Debug, Clone)]
struct MomentumOrderTrack {
    kind: MomentumOrderKind,
    symbol: String,
    token_id: String,
    side: Side,
    direction: Direction,
    shares: u64,
    price: Decimal,
}

impl MomentumStrategyAdapter {
    /// Create a new momentum strategy adapter
    pub fn new(id: String, config: MomentumConfig, exit_config: ExitConfig, dry_run: bool) -> Self {
        Self {
            id,
            config,
            exit_config,
            dry_run,
            positions: HashMap::new(),
            cex_prices: HashMap::new(),
            pm_quotes: HashMap::new(),
            events: HashMap::new(),
            cooldowns: HashMap::new(),
            daily_trades: 0,
            last_reset: Utc::now(),
            enabled: true,
            signal_log_pool: OnceCell::new(),
            signal_log_ready: OnceCell::new(),
            fixed_amount_usd: None,
            directional_entry_threshold: 0.08,
            pending_orders: HashMap::new(),
        }
    }

    async fn get_signal_log_pool(&self) -> Option<Arc<sqlx::PgPool>> {
        let existing = self.signal_log_pool.get();
        if let Some(pool) = existing {
            return Some(pool.clone());
        }

        let db_url = database_url_from_env()?;

        let pool = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&db_url)
            .await
        {
            Ok(p) => Arc::new(p),
            Err(e) => {
                warn!(
                    error = %e,
                    "signal recorder: failed to connect to Postgres (signal logging disabled)"
                );
                return None;
            }
        };

        let _ = self.signal_log_pool.set(pool.clone());
        Some(pool)
    }

    async fn ensure_signal_log_ready(&self, pool: &sqlx::PgPool) {
        if self.signal_log_ready.get().is_some() {
            return;
        }

        if let Err(e) = crate::persistence::ensure_strategy_observability_tables(pool).await {
            warn!(error = %e, "signal recorder: failed to ensure observability tables");
            return;
        }

        let _ = self.signal_log_ready.set(());
    }

    async fn record_directional_signal(
        &self,
        symbol: &str,
        direction: Direction,
        event_id: &str,
        token_id: &str,
        p_hat: f64,
        effective_p: f64,
        ev_net: f64,
        market_ask: Decimal,
        sigma: f64,
        s0: Decimal,
        st: Decimal,
        time_remaining_secs: f64,
        window_secs: u64,
    ) {
        let Some(pool) = self.get_signal_log_pool().await else {
            return;
        };
        self.ensure_signal_log_ready(&pool).await;
        if self.signal_log_ready.get().is_none() {
            return;
        }

        let account_id = account_id_from_env();
        let agent_id = std::env::var("PLOY_AGENT_ID")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.id.clone());

        let strategy_id = self.id.clone();
        let symbol = symbol.to_string();
        let side = match direction {
            Direction::Up => "UP",
            Direction::Down => "DOWN",
        }
        .to_string();

        let context = serde_json::json!({
            "mode": "directional",
            "dry_run": self.dry_run,
            "event_id": event_id,
            "p_hat": p_hat,
            "effective_p": effective_p,
            "ev_net": ev_net,
            "sigma": sigma,
            "s0": s0.to_string(),
            "st": st.to_string(),
            "time_remaining_secs": time_remaining_secs,
            "window_secs": window_secs,
        });

        let token_id = token_id.to_string();
        let market_ask = market_ask;

        tokio::spawn(async move {
            let res = sqlx::query(
                r#"
                INSERT INTO signal_history (
                    account_id, intent_id, agent_id, strategy_id, domain, signal_type,
                    market_slug, token_id, symbol, side, confidence, fair_value, market_price, edge, config_hash, context
                )
                VALUES (
                    $1, NULL, $2, $3, 'crypto', 'directional_entry',
                    NULL, $4, $5, $6, $7, $8, $9, $10, NULL, $11
                )
                "#,
            )
            .bind(account_id)
            .bind(agent_id)
            .bind(strategy_id)
            .bind(token_id)
            .bind(symbol)
            .bind(side)
            .bind(effective_p)
            .bind(p_hat)
            .bind(market_ask)
            .bind(ev_net)
            .bind(sqlx::types::Json(context))
            .execute(&*pool)
            .await;

            if let Err(e) = res {
                warn!(error = %e, "signal recorder: failed to insert directional signal");
            }
        });
    }

    fn has_pending_exit_for_token(&self, token_id: &str) -> bool {
        self.pending_orders
            .values()
            .any(|o| o.kind == MomentumOrderKind::Exit && o.token_id == token_id)
    }

    fn daily_limit_reached(&mut self) -> bool {
        if self.config.max_daily_trades == 0 {
            return false;
        }

        let now = Utc::now();
        if now.date_naive() != self.last_reset.date_naive() {
            self.daily_trades = 0;
            self.last_reset = now;
        }

        self.daily_trades >= self.config.max_daily_trades
    }

    fn in_cooldown(&self, symbol: &str) -> bool {
        if let Some(last_trade) = self.cooldowns.get(symbol) {
            let elapsed = (Utc::now() - *last_trade).num_seconds();
            elapsed < self.config.cooldown_secs as i64
        } else {
            false
        }
    }

    fn pick_entry_event_in_window<'a>(
        &'a self,
        event_list: &'a [EventState],
        now: DateTime<Utc>,
    ) -> Option<&'a EventState> {
        event_list
            .iter()
            .filter(|e| {
                let secs_remaining = (e.end_time - now).num_seconds();
                secs_remaining >= self.config.min_time_remaining_secs as i64
                    && secs_remaining <= self.config.max_time_remaining_secs as i64
            })
            .min_by_key(|e| e.end_time)
    }

    fn estimate_non_directional_fair_value(&self, move_pct: Decimal) -> Decimal {
        let x = move_pct * dec!(100);
        let fair = dec!(0.50) + x * dec!(1.5);
        fair.clamp(dec!(0.01), dec!(0.99))
    }

    fn non_directional_ev_after_costs(
        &self,
        fair_value: Decimal,
        entry_price: Decimal,
    ) -> Option<Decimal> {
        if entry_price <= Decimal::ZERO || fair_value <= Decimal::ZERO {
            return None;
        }

        let fee = entry_price * dec!(0.02);
        let slippage = entry_price * dec!(0.01);
        Some(fair_value - entry_price - fee - slippage)
    }

    fn check_momentum(&self, symbol: &str) -> Option<(Direction, Decimal)> {
        let state = self.cex_prices.get(symbol)?;
        let old_price = state.get_price_at(self.config.lookback_secs)?;

        if old_price <= Decimal::ZERO {
            return None;
        }

        let move_pct = (state.price - old_price) / old_price;
        if move_pct.abs() < self.config.min_move_pct {
            return None;
        }

        let direction = if move_pct > Decimal::ZERO {
            Direction::Up
        } else {
            Direction::Down
        };

        Some((direction, move_pct.abs()))
    }

    async fn check_directional_entry(
        &mut self,
        symbol: &str,
        price: &Decimal,
        timestamp: DateTime<Utc>,
    ) -> Option<StrategyAction> {
        let _cex = self.cex_prices.get(symbol)?;
        let event = {
            let event_list = self.events.get(symbol)?;
            self.pick_entry_event_in_window(event_list, timestamp)?
                .clone()
        };

        let Some(open_price) = event.open_price else {
            return None;
        };
        if open_price <= Decimal::ZERO || *price <= Decimal::ZERO {
            return None;
        }

        let s0 = open_price;
        let st = *price;
        let sigma = self.config.directional_vol_floor.max(1e-9);
        let log_return = ((st / s0).to_f64()?).ln();
        let window_secs = event.window_secs.max(1);
        let t_years = window_secs as f64 / (365.0 * 24.0 * 60.0 * 60.0);
        let z = (log_return + 0.5 * sigma * sigma * t_years) / (sigma * t_years.sqrt());
        let p_hat = normal_cdf(z);
        let effective_p = match self.config.min_confidence.partial_cmp(&0.5) {
            Some(std::cmp::Ordering::Greater) if p_hat >= self.config.min_confidence => p_hat,
            Some(std::cmp::Ordering::Greater) if p_hat <= (1.0 - self.config.min_confidence) => {
                1.0 - p_hat
            }
            Some(_) if p_hat >= 0.5 => p_hat,
            Some(_) => 1.0 - p_hat,
            None => return None,
        };

        let direction = if p_hat >= 0.5 {
            Direction::Up
        } else {
            Direction::Down
        };

        let token_id = match direction {
            Direction::Up => event.up_token_id.clone(),
            Direction::Down => event.down_token_id.clone(),
        };
        let quote = self.pm_quotes.get(&token_id)?;
        let entry_price = quote.best_ask?;
        if entry_price > self.config.max_entry_price || entry_price <= Decimal::ZERO {
            return None;
        }

        let ev_net = effective_p - entry_price.to_f64()? - 0.03;
        if ev_net < self.directional_entry_threshold {
            return None;
        }

        let secs_remaining = (event.end_time - timestamp).num_seconds().max(0) as f64;
        self.record_directional_signal(
            symbol,
            direction,
            &event.event_id,
            &token_id,
            p_hat,
            effective_p,
            ev_net,
            entry_price,
            sigma,
            s0,
            st,
            secs_remaining,
            window_secs,
        )
        .await;

        self.generate_entry(symbol, direction, entry_price)
    }

    fn get_entry_price(&self, symbol: &str, direction: Direction) -> Option<Decimal> {
        let now = Utc::now();
        let event_list = self.events.get(symbol)?;
        let event = self.pick_entry_event_in_window(event_list, now)?;
        let token_id = match direction {
            Direction::Up => &event.up_token_id,
            Direction::Down => &event.down_token_id,
        };
        let quote = self.pm_quotes.get(token_id)?;
        quote.best_ask
    }

    fn generate_entry(
        &mut self,
        symbol: &str,
        direction: Direction,
        entry_price: Decimal,
    ) -> Option<StrategyAction> {
        let now = Utc::now();
        let event = {
            let event_list = self.events.get(symbol)?;
            self.pick_entry_event_in_window(event_list, now)?.clone()
        };

        let (market_slug, token_id, market_side) = match direction {
            Direction::Up => (symbol.to_string(), event.up_token_id.clone(), Side::Up),
            Direction::Down => (symbol.to_string(), event.down_token_id.clone(), Side::Down),
        };

        let shares = if let Some(fixed_amount_usd) = self.fixed_amount_usd {
            let price_f64 = entry_price.to_string().parse::<f64>().ok()?;
            if price_f64 <= 0.0 {
                return None;
            }
            (fixed_amount_usd / price_f64).floor().max(1.0) as u64
        } else {
            self.config.shares_per_trade
        };

        let client_order_id = format!("{}_entry_{}_{}", self.id, symbol, now.timestamp_millis());

        self.pending_orders.insert(
            client_order_id.clone(),
            MomentumOrderTrack {
                kind: MomentumOrderKind::Entry,
                symbol: symbol.to_string(),
                token_id: token_id.clone(),
                side: market_side,
                direction,
                shares,
                price: entry_price,
            },
        );

        info!(
            "[{}] Entry signal: {} {} @ {:.2}¢ ({} shares, ${:.2})",
            self.id,
            direction,
            symbol,
            entry_price * dec!(100),
            shares,
            entry_price.to_string().parse::<f64>().unwrap_or(0.0) * shares as f64,
        );

        Some(super::crypto_submit_intent(
            client_order_id,
            market_slug,
            token_id,
            market_side,
            true,
            shares,
            entry_price,
            5,
        ))
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
                series_ids: all_updown_series_ids(),
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
                if let Some(state) = self.cex_prices.get_mut(symbol) {
                    state.update(*price, *timestamp, self.config.lookback_secs);
                } else {
                    self.cex_prices.insert(
                        symbol.clone(),
                        CexPriceState::new(symbol.clone(), *price, *timestamp),
                    );
                }

                if !self.enabled {
                    return Ok(actions);
                }

                if self.daily_limit_reached() {
                    return Ok(actions);
                }

                if self.in_cooldown(symbol) {
                    return Ok(actions);
                }

                if self.positions.len() >= self.config.max_positions {
                    return Ok(actions);
                }

                if self.positions.values().any(|p| &p.symbol == symbol) {
                    return Ok(actions);
                }

                if self.config.directional_mode {
                    if let Some(action) = self
                        .check_directional_entry(symbol, price, *timestamp)
                        .await
                    {
                        self.cooldowns.insert(symbol.clone(), Utc::now());
                        actions.push(action);
                    }
                    return Ok(actions);
                }

                if let Some((direction, move_pct)) = self.check_momentum(symbol) {
                    match self.get_entry_price(symbol, direction) {
                        Some(entry_price) => {
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
                                        self.generate_entry(symbol, direction, entry_price)
                                    {
                                        self.cooldowns.insert(symbol.clone(), Utc::now());

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
                            let now = Utc::now();
                            if let Some(event_list) = self.events.get(symbol) {
                                if let Some(event) =
                                    self.pick_entry_event_in_window(event_list, now)
                                {
                                    let token_id = match direction {
                                        Direction::Up => &event.up_token_id,
                                        Direction::Down => &event.down_token_id,
                                    };
                                    if let Some(q) = self.pm_quotes.get(token_id) {
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
                let is_new = !self.pm_quotes.contains_key(token_id);
                self.pm_quotes.insert(
                    token_id.clone(),
                    PmQuoteState {
                        token_id: token_id.clone(),
                        best_bid: quote.best_bid,
                        best_ask: quote.best_ask,
                        timestamp: *timestamp,
                    },
                );

                if let Some(pos) = self.positions.get_mut(token_id) {
                    pos.current_price = quote.best_bid.or(quote.best_ask);
                }

                if is_new {
                    info!(
                        "[{}] LOB: token {} bid: {}¢ ask: {}¢",
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

                if !self.config.hold_to_resolution {
                    let trigger = self.positions.get(token_id).and_then(|pos| {
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
                    });

                    if let Some((pos, reason, pnl_pct)) = trigger {
                        if self.has_pending_exit_for_token(&pos.token_id) {
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
                        self.pending_orders.insert(
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

                        actions.push(super::crypto_submit_intent(
                            client_order_id,
                            pos.symbol.clone(),
                            pos.token_id.clone(),
                            pos.side,
                            false,
                            pos.shares,
                            exit_price,
                            8,
                        ));
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
                let Some((symbol, window_secs)) = symbol_and_window_for_series(series_id) else {
                    return Ok(actions);
                };

                let event_vec = self.events.entry(symbol.to_string()).or_default();
                let now = chrono::Utc::now();
                event_vec.retain(|e| e.end_time > now);

                if event_vec.iter().any(|e| e.event_id == *event_id) {
                    return Ok(actions);
                }

                let open_price = if self.config.directional_mode {
                    self.cex_prices.get(symbol).map(|s| s.price)
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
                for list in self.events.values_mut() {
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
        let track = self.pending_orders.get(&order_key).cloned();

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

                            self.positions.insert(
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

                            self.daily_trades += 1;
                        }
                        MomentumOrderKind::Exit => {
                            self.positions.remove(&track.token_id);
                        }
                    }

                    self.pending_orders.remove(&order_key);
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
                self.pending_orders.remove(&order_key);
            }
            crate::domain::OrderStatus::Failed => {
                warn!(
                    "[{}] Order failed: {} - {:?}",
                    self.id, update.order_id, update.error
                );

                self.pending_orders.remove(&order_key);

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
        Ok(vec![])
    }

    fn state(&self) -> StrategyStateInfo {
        let position_count = self.positions.len();
        let pending_count = self.pending_orders.len();
        let total_exposure = self
            .positions
            .values()
            .map(|pos| pos.entry_price * Decimal::from(pos.shares))
            .sum::<Decimal>();

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
        self.positions
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

        if !self.config.hold_to_resolution {
            for pos in self.positions.values() {
                info!(
                    "[{}] Closing position: {} {} shares @ {:?}",
                    self.id, pos.token_id, pos.shares, pos.current_price
                );
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
        self.positions = HashMap::new();
        self.cex_prices = HashMap::new();
        self.pm_quotes = HashMap::new();
        self.events = HashMap::new();
        self.cooldowns = HashMap::new();
        self.daily_trades = 0;
        self.last_reset = Utc::now();
        self.pending_orders = HashMap::new();
    }
}

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

    #[test]
    fn test_generate_entry_respects_timing_window() {
        let mut config = MomentumConfig::default();
        config.min_time_remaining_secs = 300;
        config.max_time_remaining_secs = 900;

        let mut adapter = MomentumStrategyAdapter::new(
            "test_momentum".into(),
            config,
            ExitConfig::default(),
            true,
        );
        let now = Utc::now();
        let up_token = "up_token".to_string();
        let down_token = "down_token".to_string();

        adapter.events.insert(
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

        adapter.pm_quotes.insert(
            up_token.clone(),
            PmQuoteState {
                token_id: up_token.clone(),
                best_bid: Some(dec!(0.40)),
                best_ask: Some(dec!(0.42)),
                timestamp: now,
            },
        );

        assert!(adapter.get_entry_price("BTCUSDT", Direction::Up).is_none());
        assert!(adapter
            .generate_entry("BTCUSDT", Direction::Up, dec!(0.42))
            .is_none());
    }

    #[test]
    fn test_momentum_reset_clears_internal_state() {
        let now = Utc::now();
        let mut adapter = MomentumStrategyAdapter::new(
            "test_momentum".into(),
            MomentumConfig::default(),
            ExitConfig::default(),
            true,
        );

        adapter.positions.insert(
            "token-1".to_string(),
            MomentumPosition {
                token_id: "token-1".to_string(),
                symbol: "BTCUSDT".to_string(),
                direction: Direction::Up,
                side: Side::Up,
                shares: 10,
                entry_price: dec!(0.44),
                current_price: Some(dec!(0.46)),
                opened_at: now,
                order_id: Some("order-1".to_string()),
            },
        );
        adapter.cex_prices.insert(
            "BTCUSDT".to_string(),
            CexPriceState::new("BTCUSDT".to_string(), dec!(100000), now),
        );
        adapter.pm_quotes.insert(
            "token-1".to_string(),
            PmQuoteState {
                token_id: "token-1".to_string(),
                best_bid: Some(dec!(0.43)),
                best_ask: Some(dec!(0.44)),
                timestamp: now,
            },
        );
        adapter.events.insert(
            "BTCUSDT".to_string(),
            vec![EventState {
                event_id: "event-1".to_string(),
                symbol: "BTCUSDT".to_string(),
                up_token_id: "token-1".to_string(),
                down_token_id: "token-2".to_string(),
                end_time: now + chrono::Duration::seconds(600),
                open_price: Some(dec!(99999)),
                window_secs: 300,
            }],
        );
        adapter
            .cooldowns
            .insert("BTCUSDT".to_string(), now - chrono::Duration::seconds(5));
        adapter.daily_trades = 3;
        adapter.pending_orders.insert(
            "client-1".to_string(),
            MomentumOrderTrack {
                kind: MomentumOrderKind::Entry,
                symbol: "BTCUSDT".to_string(),
                token_id: "token-1".to_string(),
                side: Side::Up,
                direction: Direction::Up,
                shares: 10,
                price: dec!(0.44),
            },
        );

        adapter.reset();

        assert!(adapter.positions.is_empty());
        assert!(adapter.cex_prices.is_empty());
        assert!(adapter.pm_quotes.is_empty());
        assert!(adapter.events.is_empty());
        assert!(adapter.cooldowns.is_empty());
        assert!(adapter.pending_orders.is_empty());
        assert_eq!(adapter.daily_trades, 0);
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
}
