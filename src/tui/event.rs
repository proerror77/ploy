//! Event handling for TUI
//!
//! Manages keyboard input, tick events, and data updates.

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

/// TUI Events
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// Regular tick for UI refresh
    Tick,
    /// Keyboard input
    Key(KeyEvent),
    /// Terminal resize
    Resize(u16, u16),
    /// Quote update from Polymarket
    QuoteUpdate {
        up_bid: rust_decimal::Decimal,
        up_ask: rust_decimal::Decimal,
        down_bid: rust_decimal::Decimal,
        down_ask: rust_decimal::Decimal,
        up_size: rust_decimal::Decimal,
        down_size: rust_decimal::Decimal,
    },
    /// New fill/transaction
    Fill {
        side: crate::domain::Side,
        price: rust_decimal::Decimal,
        size: u64,
        btc_price: rust_decimal::Decimal,
        tx_hash: String,
    },
    /// Position update
    PositionUpdate {
        side: crate::domain::Side,
        shares: u64,
        current_price: rust_decimal::Decimal,
        avg_price: rust_decimal::Decimal,
    },
    /// Round end time update
    RoundEndTime(Option<chrono::DateTime<chrono::Utc>>),
    /// Strategy state change
    StrategyState(String),
}

/// Event handler that manages the event loop
pub struct EventHandler {
    /// Sender for events
    tx: mpsc::UnboundedSender<AppEvent>,
    /// Receiver for events
    rx: mpsc::UnboundedReceiver<AppEvent>,
    /// Tick rate
    tick_rate: Duration,
}

impl EventHandler {
    /// Create a new event handler
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { tx, rx, tick_rate }
    }

    /// Get a sender for external events
    pub fn sender(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.tx.clone()
    }

    /// Start the event handler loop
    pub async fn run(self) {
        let tick_rate = self.tick_rate;
        let tx = self.tx.clone();

        // Spawn tick task
        let tick_tx = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick_rate);
            loop {
                interval.tick().await;
                if tick_tx.send(AppEvent::Tick).is_err() {
                    break;
                }
            }
        });

        // Spawn keyboard event task
        let key_tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                if event::poll(Duration::from_millis(100)).unwrap_or(false) {
                    match event::read() {
                        Ok(Event::Key(key)) => {
                            if key_tx.send(AppEvent::Key(key)).is_err() {
                                break;
                            }
                        }
                        Ok(Event::Resize(w, h)) => {
                            if key_tx.send(AppEvent::Resize(w, h)).is_err() {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
    }

    /// Get the next event
    pub async fn next(&mut self) -> Option<AppEvent> {
        self.rx.recv().await
    }
}

/// Key action derived from key event
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Quit the application
    Quit,
    /// Scroll up
    ScrollUp,
    /// Scroll down
    ScrollDown,
    /// Show help
    Help,
    /// No action
    None,
}

impl From<KeyEvent> for KeyAction {
    fn from(key: KeyEvent) -> Self {
        match key.code {
            KeyCode::Char('q') => KeyAction::Quit,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::Quit,
            KeyCode::Up | KeyCode::Char('k') => KeyAction::ScrollUp,
            KeyCode::Down | KeyCode::Char('j') => KeyAction::ScrollDown,
            KeyCode::Char('?') => KeyAction::Help,
            KeyCode::Esc => KeyAction::Quit,
            _ => KeyAction::None,
        }
    }
}
