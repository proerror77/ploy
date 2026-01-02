//! Dashboard runner with live data integration
//!
//! Connects WebSocket data sources to the TUI dashboard.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rust_decimal::Decimal;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::adapters::{
    BinanceWebSocket, PolymarketClient, PolymarketWebSocket, PriceCache, PriceUpdate,
    QuoteCache, QuoteUpdate,
};
use crate::domain::Side;
use crate::error::Result;
use crate::tui::app::TuiApp;
use crate::tui::data::DisplayTransaction;
use crate::tui::event::{AppEvent, KeyAction};
use crate::tui::{init_terminal, restore_terminal, ui};

/// Dashboard configuration
#[derive(Debug, Clone)]
pub struct DashboardConfig {
    /// Series ID to monitor (e.g., "btc-15m")
    pub series: Option<String>,
    /// Symbols to track for BTC price (e.g., "BTCUSDT")
    pub symbols: Vec<String>,
    /// Token IDs to subscribe (UP/DOWN tokens)
    pub token_ids: Vec<String>,
    /// Dry run mode indicator
    pub dry_run: bool,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            series: None,
            symbols: vec!["BTCUSDT".to_string()],
            token_ids: Vec::new(),
            dry_run: true,
        }
    }
}

/// Dashboard runner that manages data sources and TUI
pub struct DashboardRunner {
    config: DashboardConfig,
    app: TuiApp,
    running: Arc<AtomicBool>,
}

impl DashboardRunner {
    /// Create a new dashboard runner
    pub fn new(config: DashboardConfig) -> Self {
        let mut app = TuiApp::new();
        app.set_dry_run(config.dry_run);

        Self {
            config,
            app,
            running: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Run the dashboard with live data
    pub async fn run(mut self) -> Result<()> {
        info!("Starting dashboard...");

        // Initialize terminal
        let mut terminal = init_terminal().map_err(|e| {
            crate::error::PloyError::Internal(format!("Failed to init terminal: {}", e))
        })?;

        // Set up data sources
        let quote_cache = Arc::new(QuoteCache::new());
        let price_cache = Arc::new(PriceCache::default());

        // Create event channel for data updates
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AppEvent>();

        // Spawn Binance price feed if symbols configured
        if !self.config.symbols.is_empty() {
            let symbols = self.config.symbols.clone();
            let event_tx = event_tx.clone();
            let running = Arc::clone(&self.running);

            tokio::spawn(async move {
                Self::run_binance_feed(symbols, event_tx, running).await;
            });
        }

        // Spawn Polymarket quote feed if tokens configured
        if !self.config.token_ids.is_empty() {
            let token_ids = self.config.token_ids.clone();
            let event_tx = event_tx.clone();
            let running = Arc::clone(&self.running);

            tokio::spawn(async move {
                Self::run_polymarket_feed(token_ids, event_tx, running).await;
            });
        }

        // Initial state
        self.app.set_strategy_state("connecting");

        // Main event loop
        loop {
            // Draw the UI
            terminal.draw(|f| ui::render(f, &self.app)).map_err(|e| {
                crate::error::PloyError::Internal(format!("Failed to render: {}", e))
            })?;

            // Handle events with timeout
            tokio::select! {
                // Handle keyboard input
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    if crossterm::event::poll(Duration::from_millis(0)).unwrap_or(false) {
                        if let Ok(crossterm::event::Event::Key(key)) = crossterm::event::read() {
                            let action = KeyAction::from(key);
                            match action {
                                KeyAction::Quit => {
                                    self.running.store(false, Ordering::SeqCst);
                                    self.app.quit();
                                    break;
                                }
                                KeyAction::ScrollUp => self.app.scroll_up(),
                                KeyAction::ScrollDown => self.app.scroll_down(),
                                KeyAction::Help => {
                                    // TODO: Show help overlay
                                }
                                KeyAction::None => {}
                            }
                        }
                    }
                }

                // Handle data events
                Some(event) = event_rx.recv() => {
                    self.handle_event(event);
                }
            }

            if !self.app.is_running() {
                break;
            }
        }

        // Cleanup
        self.running.store(false, Ordering::SeqCst);
        restore_terminal().map_err(|e| {
            crate::error::PloyError::Internal(format!("Failed to restore terminal: {}", e))
        })?;

        info!("Dashboard stopped");
        Ok(())
    }

    /// Handle incoming data events
    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::QuoteUpdate {
                up_bid,
                up_ask,
                down_bid,
                down_ask,
                up_size,
                down_size,
            } => {
                self.app.update_quotes(up_bid, up_ask, down_bid, down_ask, up_size, down_size);
                self.app.set_strategy_state("watching");
            }
            AppEvent::Fill {
                side,
                price,
                size,
                btc_price,
                tx_hash,
            } => {
                let tx = DisplayTransaction::new(Utc::now(), side, price, size, btc_price, tx_hash);
                let volume = price * Decimal::from(size);
                self.app.add_transaction(tx);
                self.app.add_volume(volume);
            }
            AppEvent::PositionUpdate {
                side,
                shares,
                current_price,
                avg_price,
            } => {
                self.app.update_position(side, shares, current_price, avg_price);
            }
            AppEvent::RoundEndTime(end_time) => {
                self.app.set_round_end_time(end_time);
            }
            AppEvent::StrategyState(state) => {
                self.app.set_strategy_state(&state);
            }
            AppEvent::Tick | AppEvent::Key(_) | AppEvent::Resize(_, _) => {
                // Handled in main loop
            }
        }
    }

    /// Run Binance price feed
    async fn run_binance_feed(
        symbols: Vec<String>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        running: Arc<AtomicBool>,
    ) {
        info!("Connecting to Binance WebSocket...");

        let binance_ws = BinanceWebSocket::new(symbols);
        let mut rx = binance_ws.subscribe();

        // Spawn WebSocket runner
        let ws_running = Arc::clone(&running);
        tokio::spawn(async move {
            if let Err(e) = binance_ws.run().await {
                if ws_running.load(Ordering::SeqCst) {
                    error!("Binance WebSocket error: {}", e);
                }
            }
        });

        // Forward price updates
        while running.load(Ordering::SeqCst) {
            match rx.recv().await {
                Ok(update) => {
                    debug!("BTC price: {}", update.price);
                    // Price updates are used when fills occur
                    // We don't send them as separate events
                }
                Err(e) => {
                    if running.load(Ordering::SeqCst) {
                        warn!("Binance channel error: {}", e);
                    }
                    break;
                }
            }
        }
    }

    /// Run Polymarket quote feed
    async fn run_polymarket_feed(
        token_ids: Vec<String>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        running: Arc<AtomicBool>,
    ) {
        info!("Connecting to Polymarket WebSocket...");

        let pm_ws = Arc::new(PolymarketWebSocket::new(
            "wss://ws-subscriptions-clob.polymarket.com/ws/market",
        ));

        // Register tokens (alternate UP/DOWN)
        for (i, token_id) in token_ids.iter().enumerate() {
            let side = if i % 2 == 0 { Side::Up } else { Side::Down };
            pm_ws.register_token(token_id, side).await;
        }

        let mut rx = pm_ws.subscribe_updates();
        let quote_cache = pm_ws.quote_cache().clone();

        // Spawn WebSocket runner
        let pm_ws_clone = Arc::clone(&pm_ws);
        let tokens = token_ids.clone();
        let ws_running = Arc::clone(&running);
        tokio::spawn(async move {
            if let Err(e) = pm_ws_clone.run(tokens).await {
                if ws_running.load(Ordering::SeqCst) {
                    error!("Polymarket WebSocket error: {}", e);
                }
            }
        });

        // Track UP and DOWN quotes separately
        let mut up_quote: Option<crate::domain::Quote> = None;
        let mut down_quote: Option<crate::domain::Quote> = None;

        // Forward quote updates
        while running.load(Ordering::SeqCst) {
            match rx.recv().await {
                Ok(update) => {
                    debug!("Quote update: {:?} {:?}", update.side, update.quote);

                    // Update tracked quotes
                    match update.side {
                        Side::Up => up_quote = Some(update.quote),
                        Side::Down => down_quote = Some(update.quote),
                    }

                    // Send combined update if we have both
                    if let (Some(up), Some(down)) = (&up_quote, &down_quote) {
                        let _ = event_tx.send(AppEvent::QuoteUpdate {
                            up_bid: up.best_bid.unwrap_or_default(),
                            up_ask: up.best_ask.unwrap_or_default(),
                            down_bid: down.best_bid.unwrap_or_default(),
                            down_ask: down.best_ask.unwrap_or_default(),
                            up_size: up.bid_size.unwrap_or_default(),
                            down_size: down.bid_size.unwrap_or_default(),
                        });
                    }
                }
                Err(e) => {
                    if running.load(Ordering::SeqCst) {
                        warn!("Polymarket channel error: {}", e);
                    }
                    break;
                }
            }
        }
    }
}

/// Map common series slugs to their numeric IDs
fn resolve_series_id(series: &str) -> &str {
    match series.to_lowercase().as_str() {
        // SOL series
        "sol-15m" | "sol-updown-15m" | "sol" => "10423",
        "sol-4h" | "sol-updown-4h" => "10333",
        // ETH series
        "eth-15m" | "eth-updown-15m" | "eth" => "10191",
        "eth-1h" | "eth-hourly" | "eth-updown-hourly" => "10117",
        "eth-4h" | "eth-updown-4h" => "10332",
        // BTC series (daily markets use different structure)
        "btc-daily" | "btc" => "41", // BTC daily series
        // If already a numeric ID or unknown, pass through
        _ => series,
    }
}

/// Run dashboard with auto-discovery of active markets
pub async fn run_dashboard_auto(series: Option<&str>, dry_run: bool) -> Result<()> {
    info!("Initializing dashboard with auto-discovery...");

    // Create client for market discovery
    let client = PolymarketClient::new("https://clob.polymarket.com", true)?;

    // Determine which series to monitor - resolve slug to numeric ID
    let series_input = series.unwrap_or("sol-15m");
    let series_id = resolve_series_id(series_input);
    info!("Looking for active markets in series: {} (resolved from '{}')", series_id, series_input);

    // Get tokens for the series
    let token_ids = match client.get_series_all_tokens(series_id).await {
        Ok(events) => {
            let tokens: Vec<String> = events
                .iter()
                .flat_map(|(_, up, down)| vec![up.clone(), down.clone()])
                .collect();
            info!("Found {} tokens for series {}", tokens.len(), series_id);
            tokens
        }
        Err(e) => {
            warn!("Failed to get markets for series: {}", e);
            Vec::new()
        }
    };

    if token_ids.is_empty() {
        warn!("No active markets found. Dashboard will show empty state.");
    }

    // Determine which Binance symbol to track based on series
    let binance_symbol = if series_id.starts_with("104") {
        "SOLUSDT"  // SOL series
    } else if series_id.starts_with("101") || series_id.starts_with("103") {
        "ETHUSDT"  // ETH series
    } else {
        "BTCUSDT"  // Default to BTC
    };

    let config = DashboardConfig {
        series: Some(series_id.to_string()),
        symbols: vec![binance_symbol.to_string()],
        token_ids,
        dry_run,
    };

    let runner = DashboardRunner::new(config);
    runner.run().await
}
