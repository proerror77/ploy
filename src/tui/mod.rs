//! Terminal User Interface module
//!
//! Provides a cyberpunk-style dashboard for monitoring trading activity.

pub mod app;
pub mod data;
pub mod event;
pub mod runner;
pub mod theme;
pub mod ui;
pub mod widgets;

#[cfg(test)]
mod tests;

pub use app::TuiApp;
pub use data::{DashboardStats, DisplayAgent, DisplayPosition, DisplayTransaction, MarketState};
pub use event::{AppEvent, EventHandler, KeyAction};
pub use runner::{run_dashboard_auto, DashboardConfig, DashboardRunner};
pub use theme::Theme;

use std::io;
use std::time::Duration;

use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;

/// Initialize the terminal for TUI mode
pub fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

/// Restore the terminal to normal mode
pub fn restore_terminal() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// Run the TUI application
pub async fn run_tui(mut app: TuiApp) -> io::Result<()> {
    // Initialize terminal
    let mut terminal = init_terminal()?;

    // Create event handler with 100ms tick rate
    let events = EventHandler::new(Duration::from_millis(100));

    // Start event loop in background
    let _event_tx = events.sender();
    tokio::spawn(async move {
        events.run().await;
    });

    // Create a new receiver since we moved events
    let (_tx, _rx): (tokio::sync::mpsc::UnboundedSender<AppEvent>, _) =
        tokio::sync::mpsc::unbounded_channel();

    // Main loop - simplified version that handles basic events
    let event_handler = EventHandler::new(Duration::from_millis(100));

    // Run event handler
    let _event_sender = event_handler.sender();
    tokio::spawn(async move {
        event_handler.run().await;
    });

    // Create another receiver for the main loop
    let (main_tx, _main_rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();

    // Spawn event forwarder
    let _forward_tx = main_tx.clone();
    tokio::spawn(async move {
        let handler = EventHandler::new(Duration::from_millis(100));
        let _sender = handler.sender();

        // Run handler
        tokio::spawn(async move {
            handler.run().await;
        });
    });

    // Simple event loop using crossterm directly
    loop {
        // Draw
        terminal.draw(|f| ui::render(f, &app))?;

        // Handle events with timeout
        if crossterm::event::poll(Duration::from_millis(100))? {
            if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                let action = KeyAction::from(key);
                match action {
                    KeyAction::Quit => {
                        app.quit();
                        break;
                    }
                    KeyAction::ScrollUp => app.scroll_up(),
                    KeyAction::ScrollDown => app.scroll_down(),
                    KeyAction::Help => app.toggle_help(),
                    KeyAction::NextMarket => app.next_market(),
                    KeyAction::PrevMarket => app.prev_market(),
                    KeyAction::None => {}
                }
            }
        }

        if !app.is_running() {
            break;
        }
    }

    // Restore terminal
    restore_terminal()?;

    Ok(())
}

/// Run TUI with demo data (for testing)
pub async fn run_demo() -> io::Result<()> {
    let app = TuiApp::new().with_demo_data();
    run_tui(app).await
}
