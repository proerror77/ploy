//! Dashboard runner with live data integration
//!
//! Connects WebSocket data sources to the TUI dashboard.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "api")]
use chrono::DateTime;
use chrono::Utc;
#[cfg(feature = "api")]
use reqwest::header::{HeaderMap, HeaderValue};
use rust_decimal::Decimal;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::adapters::{PolymarketClient, PriceCache, QuoteCache};
use crate::data_plane::{DataPlaneConfig, DataPlaneFreshness, PlatformDataPlane};
use crate::domain::Side;
use crate::error::Result;
use crate::tui::app::{ActiveTab, PendingAction, TuiApp};
use crate::tui::data::{
    DisplayAgent, DisplayOperatorAction, DisplayOperatorClaimer, DisplayOperatorDomain,
    DisplayOperatorSummary, DisplayRiskState, DisplayTransaction,
};
use crate::tui::event::{AppEvent, KeyAction};
use crate::tui::{init_terminal, restore_terminal, ui};

#[cfg(feature = "api")]
use crate::api::types::{
    OperatorAction, OperatorActionRequest, OperatorActionResponse, OperatorStatusResponse,
};

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
    /// Operator API base URL
    pub api_base_url: String,
    /// Operator/admin auth token
    pub admin_token: Option<String>,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            series: None,
            symbols: vec!["BTCUSDT".to_string()],
            token_ids: Vec::new(),
            dry_run: true,
            api_base_url: default_api_base_url(),
            admin_token: default_admin_token(),
        }
    }
}

fn default_api_base_url() -> String {
    let port = std::env::var("PLOY_API_PORT")
        .ok()
        .and_then(|raw| raw.trim().parse::<u16>().ok())
        .unwrap_or(8081);
    format!("http://127.0.0.1:{port}")
}

fn default_admin_token() -> Option<String> {
    std::env::var("PLOY_API_ADMIN_TOKEN")
        .or_else(|_| std::env::var("PLOY_ADMIN_TOKEN"))
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
}

/// Dashboard runner that manages data sources and TUI
pub struct DashboardRunner {
    config: DashboardConfig,
    app: TuiApp,
    running: Arc<AtomicBool>,
    #[cfg(feature = "api")]
    http: reqwest::Client,
    #[cfg(feature = "api")]
    last_operator_refresh: Option<std::time::Instant>,
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
            #[cfg(feature = "api")]
            http: reqwest::Client::new(),
            #[cfg(feature = "api")]
            last_operator_refresh: None,
        }
    }

    #[cfg(feature = "api")]
    fn operator_api_headers(&self) -> Option<HeaderMap> {
        let token = self.config.admin_token.as_deref()?;
        let mut headers = HeaderMap::new();
        headers.insert("x-ploy-admin-token", HeaderValue::from_str(token).ok()?);
        Some(headers)
    }

    #[cfg(feature = "api")]
    fn operator_refresh_due(&self) -> bool {
        match self.last_operator_refresh {
            Some(last) => last.elapsed() >= Duration::from_secs(5),
            None => true,
        }
    }

    #[cfg(feature = "api")]
    async fn refresh_operator_status(&mut self) {
        let Some(headers) = self.operator_api_headers() else {
            return;
        };
        match self
            .http
            .get(format!("{}/api/operator/status", self.config.api_base_url))
            .headers(headers)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<OperatorStatusResponse>().await {
                    Ok(status) => {
                        apply_operator_status(&mut self.app, status);
                        self.last_operator_refresh = Some(std::time::Instant::now());
                    }
                    Err(error) => {
                        self.app
                            .set_last_error(format!("operator status decode: {}", error));
                    }
                }
            }
            Ok(resp) => {
                self.app
                    .set_last_error(format!("operator status request failed: {}", resp.status()));
            }
            Err(error) => {
                self.app
                    .set_last_error(format!("operator status request error: {}", error));
            }
        }
    }

    #[cfg(feature = "api")]
    async fn submit_operator_action(&mut self, action: OperatorAction, domain: Option<String>) {
        let Some(headers) = self.operator_api_headers() else {
            self.app
                .set_last_error("operator API admin token is not configured".to_string());
            return;
        };
        let scope = if domain.is_some() {
            crate::api::types::OperatorScope::Domain
        } else {
            crate::api::types::OperatorScope::Global
        };
        let label = operator_action_label(action, domain.as_deref(), scope);
        let request = OperatorActionRequest {
            action,
            scope,
            domain: domain.clone(),
            requested_by: "tui".to_string(),
            reason: None,
        };

        match self
            .http
            .post(format!("{}/api/operator/actions", self.config.api_base_url))
            .headers(headers)
            .json(&request)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<OperatorActionResponse>().await {
                    Ok(response) => {
                        self.app.push_operator_action(DisplayOperatorAction {
                            action_id: response.action_id,
                            label,
                            accepted: response.accepted,
                            message: response.message,
                            requested_by: "tui".to_string(),
                        });
                        self.refresh_operator_status().await;
                    }
                    Err(error) => {
                        self.app
                            .set_last_error(format!("operator action decode: {}", error));
                    }
                }
            }
            Ok(resp) => {
                self.app
                    .set_last_error(format!("operator action failed: {}", resp.status()));
            }
            Err(error) => {
                self.app
                    .set_last_error(format!("operator action request error: {}", error));
            }
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
        let _quote_cache = Arc::new(QuoteCache::new());
        let _price_cache = Arc::new(PriceCache::default());

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
        #[cfg(feature = "api")]
        self.refresh_operator_status().await;

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
                            // If we're editing the filter input, treat keys as text entry.
                            if self.app.filter_mode {
                                use crossterm::event::KeyCode;
                                match key.code {
                                    KeyCode::Esc => {
                                        self.app.filter_mode = false;
                                        self.app.filter_input.clear();
                                    }
                                    KeyCode::Enter => {
                                        self.app.filter_mode = false;
                                    }
                                    KeyCode::Backspace => {
                                        self.app.filter_input.pop();
                                    }
                                    KeyCode::Char(c) => {
                                        // Ignore control/alt combos and only accept plain chars.
                                        if key.modifiers.is_empty()
                                            || key.modifiers == crossterm::event::KeyModifiers::SHIFT
                                        {
                                            self.app.filter_input.push(c);
                                        }
                                    }
                                    _ => {}
                                }
                                continue;
                            }

                            // If a modal is open, only accept confirm/dismiss keys.
                            if self.app.modal.is_some() {
                                use crossterm::event::KeyCode;
                                match key.code {
                                    KeyCode::Char('y') | KeyCode::Enter => {
                                        if let Some(action) = self.app.confirm_modal() {
                                            #[cfg(feature = "api")]
                                            {
                                                let selected_domain = if self.app.active_tab == ActiveTab::Operator {
                                                    self.app
                                                        .selected_operator_domain()
                                                        .map(ToString::to_string)
                                                } else {
                                                    None
                                                };
                                                match action {
                                                    PendingAction::PauseAgents => {
                                                        self.submit_operator_action(
                                                            OperatorAction::Pause,
                                                            selected_domain,
                                                        )
                                                        .await;
                                                    }
                                                    PendingAction::ResumeAgents => {
                                                        self.submit_operator_action(
                                                            OperatorAction::Resume,
                                                            selected_domain,
                                                        )
                                                        .await;
                                                    }
                                                    PendingAction::ForceClose => {
                                                        self.submit_operator_action(
                                                            OperatorAction::ForceClose,
                                                            selected_domain,
                                                        )
                                                        .await;
                                                    }
                                                }
                                            }

                                            #[cfg(not(feature = "api"))]
                                            match action {
                                                PendingAction::PauseAgents => {
                                                    self.app.set_strategy_state("paused");
                                                }
                                                PendingAction::ResumeAgents => {
                                                    self.app.set_strategy_state("running");
                                                }
                                                PendingAction::ForceClose => {
                                                    self.app.set_strategy_state("halted");
                                                }
                                            }
                                        }
                                    }
                                    KeyCode::Char('n') | KeyCode::Esc => {
                                        self.app.dismiss_modal();
                                    }
                                    _ => {}
                                }
                                continue;
                            }

                            let action = KeyAction::from(key);
                            match action {
                                KeyAction::Quit => {
                                    self.running.store(false, Ordering::SeqCst);
                                    self.app.quit();
                                    break;
                                }
                                KeyAction::ScrollUp => self.app.scroll_up(),
                                KeyAction::ScrollDown => self.app.scroll_down(),
                                KeyAction::Help => self.app.toggle_help(),
                                KeyAction::NextMarket => self.app.next_market(),
                                KeyAction::PrevMarket => self.app.prev_market(),
                                KeyAction::ToggleTab => self.app.toggle_tab(),
                                KeyAction::PauseAgents => {
                                    let target = operator_modal_target(&self.app);
                                    self.app.show_modal(
                                        format!("Pause {target}? [y/N]"),
                                        PendingAction::PauseAgents,
                                    );
                                }
                                KeyAction::ResumeAgents => {
                                    let target = operator_modal_target(&self.app);
                                    self.app.show_modal(
                                        format!("Resume {target}? [y/N]"),
                                        PendingAction::ResumeAgents,
                                    );
                                }
                                KeyAction::EmergencyClose => {
                                    let target = operator_modal_target(&self.app);
                                    self.app.show_modal(
                                        format!("Force close {target}? [y/N]"),
                                        PendingAction::ForceClose,
                                    );
                                }
                                KeyAction::EnterFilter => {
                                    self.app.filter_mode = true;
                                    self.app.filter_input.clear();
                                }
                                KeyAction::ClaimCheck => {
                                    #[cfg(feature = "api")]
                                    self.submit_operator_action(OperatorAction::ClaimCheck, None).await;
                                }
                                KeyAction::ClaimRun => {
                                    #[cfg(feature = "api")]
                                    self.submit_operator_action(OperatorAction::ClaimRun, None).await;
                                }
                                KeyAction::RefreshOperator => {
                                    #[cfg(feature = "api")]
                                    self.refresh_operator_status().await;
                                }
                                KeyAction::Confirm | KeyAction::Dismiss => {}
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

            #[cfg(feature = "api")]
            if self.app.active_tab == ActiveTab::Operator && self.operator_refresh_due() {
                self.refresh_operator_status().await;
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
                self.app
                    .update_quotes(up_bid, up_ask, down_bid, down_ask, up_size, down_size);
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
                self.app
                    .update_position(side, shares, current_price, avg_price);
            }
            AppEvent::RoundEndTime(end_time) => {
                self.app.set_round_end_time(end_time);
            }
            AppEvent::StrategyState(state) => {
                self.app.set_strategy_state(&state);
            }
            AppEvent::BinancePrice { symbol, price } => {
                self.app.update_binance_price(symbol, price);
            }
            AppEvent::ConnectionStatus(connected) => {
                self.app.set_connection_status(connected);
            }
            AppEvent::Error(msg) => {
                self.app.set_last_error(msg);
            }
            AppEvent::AgentUpdate(snaps) => {
                let agents = snaps.iter().map(DisplayAgent::from_snapshot).collect();
                self.app.update_agents(agents);
            }
            AppEvent::RiskUpdate {
                state,
                daily_loss_used,
                daily_loss_limit,
                queue_depth,
                circuit_breaker,
                total_exposure,
            } => {
                self.app.update_risk_state(DisplayRiskState {
                    state: format!("{:?}", state),
                    daily_loss_used,
                    daily_loss_limit,
                    queue_depth,
                    circuit_breaker,
                    total_exposure,
                });
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

        let data_plane = Arc::new(PlatformDataPlane::new(
            DataPlaneConfig {
                binance_spot_symbols: symbols,
                ..Default::default()
            },
            Arc::new(DataPlaneFreshness::new()),
        ));
        let Some(binance_ws) = data_plane.binance_ws() else {
            let _ = event_tx.send(AppEvent::Error(
                "Binance data plane is missing websocket adapter".to_string(),
            ));
            return;
        };
        if let Err(e) = data_plane.start(Vec::new()).await {
            let _ = event_tx.send(AppEvent::Error(format!("Binance data plane start: {}", e)));
            return;
        }

        let mut rx = binance_ws.subscribe();

        let mut first_update = true;

        // Forward price updates
        while running.load(Ordering::SeqCst) {
            match rx.recv().await {
                Ok(update) => {
                    debug!("BTC price: {}", update.price);
                    if first_update {
                        let _ = event_tx.send(AppEvent::ConnectionStatus(true));
                        first_update = false;
                    }
                    let _ = event_tx.send(AppEvent::BinancePrice {
                        symbol: update.symbol,
                        price: update.price,
                    });
                }
                Err(e) => {
                    if running.load(Ordering::SeqCst) {
                        warn!("Binance channel error: {}", e);
                        let _ = event_tx.send(AppEvent::Error(format!("Binance: {}", e)));
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

        let data_plane = Arc::new(PlatformDataPlane::new(
            DataPlaneConfig {
                polymarket_ws_url: "wss://ws-subscriptions-clob.polymarket.com/ws/market"
                    .to_string(),
                ..Default::default()
            },
            Arc::new(DataPlaneFreshness::new()),
        ));
        let Some(pm_ws) = data_plane.polymarket_ws() else {
            let _ = event_tx.send(AppEvent::Error(
                "Polymarket data plane is missing websocket adapter".to_string(),
            ));
            return;
        };

        // Register tokens (alternate UP/DOWN)
        for (i, token_id) in token_ids.iter().enumerate() {
            let side = if i % 2 == 0 { Side::Up } else { Side::Down };
            pm_ws.register_token(token_id, side).await;
        }
        if let Err(e) = data_plane.start(Vec::new()).await {
            let _ = event_tx.send(AppEvent::Error(format!(
                "Polymarket data plane start: {}",
                e
            )));
            return;
        }

        let mut rx = pm_ws.subscribe_updates();
        let _quote_cache = pm_ws.quote_cache().clone();

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

fn operator_modal_target(app: &TuiApp) -> String {
    if app.active_tab == ActiveTab::Operator {
        if let Some(domain) = app.selected_operator_domain() {
            return format!("{domain} domain");
        }
    }
    "all domains".to_string()
}

#[cfg(feature = "api")]
fn decimal_from_f64(value: f64) -> Decimal {
    value
        .to_string()
        .parse::<Decimal>()
        .unwrap_or(Decimal::ZERO)
}

#[cfg(feature = "api")]
fn format_operator_timestamp(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(|ts| ts.format("%H:%M:%S").to_string())
}

#[cfg(feature = "api")]
fn operator_action_label(
    action: OperatorAction,
    domain: Option<&str>,
    scope: crate::api::types::OperatorScope,
) -> String {
    let action_label = match action {
        OperatorAction::Pause => "pause",
        OperatorAction::Resume => "resume",
        OperatorAction::ForceClose => "force_close",
        OperatorAction::ClaimCheck => "claim_check",
        OperatorAction::ClaimRun => "claim_run",
    };
    match (scope, domain) {
        (crate::api::types::OperatorScope::Domain, Some(domain)) => {
            format!("{action_label}:{domain}")
        }
        _ => action_label.to_string(),
    }
}

#[cfg(feature = "api")]
fn apply_operator_status(app: &mut TuiApp, status: OperatorStatusResponse) {
    app.update_operator_summary(DisplayOperatorSummary {
        runtime_mode: status.runtime_mode,
        account_id: status.account_id,
        dry_run: status.dry_run,
        system_status: status.system_status,
        risk_state: status.risk_state,
        queue_depth: status.queue_depth,
    });
    app.update_operator_domains(
        status
            .domains
            .into_iter()
            .map(|domain| DisplayOperatorDomain {
                domain: domain.domain,
                ingress_mode: domain.ingress_mode,
                paused: domain.paused,
                exposure: decimal_from_f64(domain.exposure_usd),
                daily_pnl: decimal_from_f64(domain.daily_pnl_usd),
            })
            .collect(),
    );
    app.update_operator_claimer(DisplayOperatorClaimer {
        enabled: status.claimer.enabled,
        pending_redeemable_count: status.claimer.pending_redeemable_count,
        pending_redeemable_notional_usd: decimal_from_f64(
            status.claimer.pending_redeemable_notional_usd,
        ),
        last_checked_label: format_operator_timestamp(status.claimer.last_checked_at),
        last_run_label: format_operator_timestamp(status.claimer.last_run_at),
        last_error: status.claimer.last_error,
    });
    app.operator_actions = status
        .recent_actions
        .into_iter()
        .map(|action| DisplayOperatorAction {
            action_id: action.action_id,
            label: operator_action_label(action.action, action.domain.as_deref(), action.scope),
            accepted: action.accepted,
            message: action.message,
            requested_by: action.requested_by,
        })
        .collect();
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
    info!(
        "Looking for active markets in series: {} (resolved from '{}')",
        series_id, series_input
    );

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
        "SOLUSDT" // SOL series
    } else if series_id.starts_with("101") || series_id.starts_with("103") {
        "ETHUSDT" // ETH series
    } else {
        "BTCUSDT" // Default to BTC
    };

    let config = DashboardConfig {
        series: Some(series_id.to_string()),
        symbols: vec![binance_symbol.to_string()],
        token_ids,
        dry_run,
        api_base_url: default_api_base_url(),
        admin_token: default_admin_token(),
    };

    let mut runner = DashboardRunner::new(config);

    // Set available markets from series info
    let mut market_names = vec![series_input.to_string()];
    // Add common related series for switching
    match series_input.to_lowercase().as_str() {
        s if s.starts_with("sol") => {
            market_names = vec!["SOL-15m".into(), "SOL-4h".into()];
        }
        s if s.starts_with("eth") => {
            market_names = vec!["ETH-15m".into(), "ETH-1h".into(), "ETH-4h".into()];
        }
        s if s.starts_with("btc") => {
            market_names = vec!["BTC-Daily".into()];
        }
        _ => {}
    }
    runner.app.set_markets(market_names);

    runner.run().await
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "api")]
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::{get, post},
        Json, Router,
    };
    #[cfg(feature = "api")]
    use chrono::Utc;
    #[cfg(feature = "api")]
    use std::sync::Arc;
    #[cfg(feature = "api")]
    use tokio::{net::TcpListener, sync::Mutex};

    #[cfg(feature = "api")]
    use super::{apply_operator_status, DashboardConfig, DashboardRunner};
    #[cfg(feature = "api")]
    use crate::api::types::{
        OperatorAction, OperatorActionRequest, OperatorActionResponse, OperatorClaimerStatus,
        OperatorDomainStatus, OperatorRecentAction, OperatorScope, OperatorStatusResponse,
    };
    use crate::tui::app::TuiApp;

    #[cfg(feature = "api")]
    #[derive(Clone)]
    struct TestOperatorApiState {
        status: Arc<Mutex<OperatorStatusResponse>>,
        actions: Arc<Mutex<Vec<OperatorActionRequest>>>,
    }

    #[cfg(feature = "api")]
    fn sample_operator_status() -> OperatorStatusResponse {
        OperatorStatusResponse {
            runtime_mode: "platform".to_string(),
            account_id: "acct-1".to_string(),
            dry_run: true,
            system_status: "running".to_string(),
            risk_state: "normal".to_string(),
            queue_depth: 3,
            domains: vec![OperatorDomainStatus {
                domain: "crypto".to_string(),
                ingress_mode: "running".to_string(),
                paused: false,
                exposure_usd: 12.5,
                daily_pnl_usd: 1.25,
            }],
            claimer: OperatorClaimerStatus {
                enabled: true,
                pending_redeemable_count: 2,
                pending_redeemable_notional_usd: 4.5,
                last_checked_at: Some(Utc::now()),
                last_run_at: None,
                last_error: None,
            },
            recent_actions: Vec::new(),
        }
    }

    #[cfg(feature = "api")]
    async fn spawn_operator_test_server() -> (String, Arc<Mutex<Vec<OperatorActionRequest>>>) {
        async fn status_handler(
            State(state): State<TestOperatorApiState>,
            headers: HeaderMap,
        ) -> Result<Json<OperatorStatusResponse>, StatusCode> {
            let token = headers
                .get("x-ploy-admin-token")
                .and_then(|value| value.to_str().ok());
            if token != Some("test-token") {
                return Err(StatusCode::UNAUTHORIZED);
            }
            Ok(Json(state.status.lock().await.clone()))
        }

        async fn action_handler(
            State(state): State<TestOperatorApiState>,
            headers: HeaderMap,
            Json(request): Json<OperatorActionRequest>,
        ) -> Result<Json<OperatorActionResponse>, StatusCode> {
            let token = headers
                .get("x-ploy-admin-token")
                .and_then(|value| value.to_str().ok());
            if token != Some("test-token") {
                return Err(StatusCode::UNAUTHORIZED);
            }

            state.actions.lock().await.push(request.clone());
            let action_id = format!("act-{}", state.actions.lock().await.len());
            let recent = OperatorRecentAction {
                action_id: action_id.clone(),
                action: request.action,
                scope: request.scope,
                domain: request.domain.clone(),
                accepted: true,
                message: "ok".to_string(),
                requested_by: request.requested_by.clone(),
                requested_at: Utc::now(),
            };
            state.status.lock().await.recent_actions = vec![recent];

            Ok(Json(OperatorActionResponse {
                accepted: true,
                action_id,
                action: request.action,
                scope: request.scope,
                effective_targets: request
                    .domain
                    .clone()
                    .map(|value| vec![value])
                    .unwrap_or_else(|| vec!["global".to_string()]),
                message: "ok".to_string(),
                requested_at: Utc::now(),
            }))
        }

        let state = TestOperatorApiState {
            status: Arc::new(Mutex::new(sample_operator_status())),
            actions: Arc::new(Mutex::new(Vec::new())),
        };
        let actions = state.actions.clone();
        let app = Router::new()
            .route("/api/operator/status", get(status_handler))
            .route("/api/operator/actions", post(action_handler))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test operator api");
        let address = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve operator api");
        });
        (format!("http://{}", address), actions)
    }

    #[cfg(feature = "api")]
    #[test]
    fn apply_operator_status_populates_operator_panels() {
        let mut app = TuiApp::new();
        apply_operator_status(
            &mut app,
            OperatorStatusResponse {
                recent_actions: vec![OperatorRecentAction {
                    action_id: "act-1".to_string(),
                    action: OperatorAction::Pause,
                    scope: OperatorScope::Global,
                    domain: None,
                    accepted: true,
                    message: "paused".to_string(),
                    requested_by: "ops".to_string(),
                    requested_at: Utc::now(),
                }],
                ..sample_operator_status()
            },
        );

        assert_eq!(app.operator_summary.account_id, "acct-1");
        assert_eq!(app.operator_domains.len(), 1);
        assert_eq!(app.operator_domains[0].domain, "crypto");
        assert!(app.operator_claimer.enabled);
        assert_eq!(app.operator_actions.len(), 1);
        assert_eq!(app.operator_actions[0].action_id, "act-1");
    }

    #[cfg(feature = "api")]
    #[tokio::test(flavor = "current_thread")]
    async fn operator_refresh_pulls_status_from_api() {
        let (base_url, _actions) = spawn_operator_test_server().await;
        let mut runner = DashboardRunner::new(DashboardConfig {
            api_base_url: base_url,
            admin_token: Some("test-token".to_string()),
            ..DashboardConfig::default()
        });

        runner.refresh_operator_status().await;

        assert_eq!(runner.app.operator_summary.account_id, "acct-1");
        assert_eq!(runner.app.operator_domains.len(), 1);
        assert_eq!(runner.app.operator_domains[0].domain, "crypto");
    }

    #[cfg(feature = "api")]
    #[tokio::test(flavor = "current_thread")]
    async fn operator_action_posts_to_api_and_refreshes_status() {
        let (base_url, actions) = spawn_operator_test_server().await;
        let mut runner = DashboardRunner::new(DashboardConfig {
            api_base_url: base_url,
            admin_token: Some("test-token".to_string()),
            ..DashboardConfig::default()
        });

        runner
            .submit_operator_action(OperatorAction::Pause, Some("crypto".to_string()))
            .await;

        let captured = actions.lock().await.clone();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].action, OperatorAction::Pause);
        assert_eq!(captured[0].domain.as_deref(), Some("crypto"));
        assert_eq!(runner.app.operator_actions.len(), 1);
        assert_eq!(runner.app.operator_actions[0].label, "pause:crypto");
    }
}
