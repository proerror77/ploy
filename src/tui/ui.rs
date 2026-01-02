//! Main UI rendering logic
//!
//! Orchestrates the layout and renders all widgets.

use ratatui::{
    layout::{Constraint, Layout},
    Frame,
};

use crate::tui::app::TuiApp;
use crate::tui::widgets;

/// Render the entire UI
pub fn render(f: &mut Frame, app: &TuiApp) {
    // Main vertical layout
    let chunks = Layout::vertical([
        Constraint::Length(7),   // Positions panel
        Constraint::Length(5),   // Market Analysis panel
        Constraint::Min(8),      // Transactions panel (fills remaining)
        Constraint::Length(1),   // Footer status bar
    ]).split(f.area());

    // Render each panel
    widgets::render_positions(f, chunks[0], app);
    widgets::render_market_analysis(f, chunks[1], app);
    widgets::render_transactions(f, chunks[2], app);
    widgets::render_footer(f, chunks[3], app);
}
