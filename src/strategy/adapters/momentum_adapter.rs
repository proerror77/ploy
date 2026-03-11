use super::*;

#[path = "momentum_adapter/config_support.rs"]
mod config_support;
#[path = "momentum_adapter/lifecycle_support.rs"]
mod lifecycle_support;
#[path = "momentum_adapter/market_update_support.rs"]
mod market_update_support;
#[path = "momentum_adapter/signal_support.rs"]
mod signal_support;

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
        self.handle_market_update(update).await
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        self.handle_order_update(update).await
    }

    async fn on_tick(&mut self, _now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        Ok(vec![])
    }

    fn state(&self) -> StrategyStateInfo {
        self.state_snapshot()
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.position_infos()
    }

    fn is_active(&self) -> bool {
        self.enabled
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.shutdown_actions().await
    }

    fn reset(&mut self) {
        self.reset_state();
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

    #[tokio::test]
    async fn test_order_fill_promotes_pending_entry_to_position() {
        let mut adapter = MomentumStrategyAdapter::new(
            "test_momentum".into(),
            MomentumConfig::default(),
            ExitConfig::default(),
            true,
        );
        let now = Utc::now();

        adapter.pending_orders.insert(
            "client-1".to_string(),
            MomentumOrderTrack {
                kind: MomentumOrderKind::Entry,
                symbol: "BTCUSDT".to_string(),
                token_id: "token-1".to_string(),
                side: Side::Up,
                direction: Direction::Up,
                shares: 7,
                price: dec!(0.44),
            },
        );

        let actions = adapter
            .on_order_update(&OrderUpdate {
                order_id: "venue-1".to_string(),
                client_order_id: Some("client-1".to_string()),
                status: crate::domain::OrderStatus::Filled,
                filled_qty: 7,
                avg_fill_price: Some(dec!(0.45)),
                timestamp: now,
                error: None,
            })
            .await
            .expect("filled update should succeed");

        assert!(adapter.pending_orders.is_empty());
        assert_eq!(adapter.daily_trades, 1);
        assert_eq!(adapter.positions.len(), 1);

        let position = adapter.positions.get("token-1").expect("position inserted");
        assert_eq!(position.shares, 7);
        assert_eq!(position.entry_price, dec!(0.45));
        assert!(actions
            .iter()
            .any(|action| matches!(action, StrategyAction::LogEvent { .. })));
    }

    #[tokio::test]
    async fn test_failed_order_clears_pending_and_emits_alert() {
        let mut adapter = MomentumStrategyAdapter::new(
            "test_momentum".into(),
            MomentumConfig::default(),
            ExitConfig::default(),
            true,
        );

        adapter.pending_orders.insert(
            "client-2".to_string(),
            MomentumOrderTrack {
                kind: MomentumOrderKind::Exit,
                symbol: "BTCUSDT".to_string(),
                token_id: "token-2".to_string(),
                side: Side::Up,
                direction: Direction::Up,
                shares: 3,
                price: dec!(0.41),
            },
        );

        let actions = adapter
            .on_order_update(&OrderUpdate {
                order_id: "venue-2".to_string(),
                client_order_id: Some("client-2".to_string()),
                status: crate::domain::OrderStatus::Failed,
                filled_qty: 0,
                avg_fill_price: None,
                timestamp: Utc::now(),
                error: Some("rejected".to_string()),
            })
            .await
            .expect("failed update should succeed");

        assert!(adapter.pending_orders.is_empty());
        assert!(actions.iter().any(|action| matches!(
            action,
            StrategyAction::Alert {
                level: AlertLevel::Warning,
                ..
            }
        )));
    }
}
