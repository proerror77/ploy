//! Main UI rendering logic
//!
//! Orchestrates the layout and renders all widgets.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::tui::app::TuiApp;
use crate::tui::theme::THEME;
use crate::tui::widgets;

/// Render the entire UI
pub fn render(f: &mut Frame, app: &TuiApp) {
    // Main vertical layout
    let chunks = Layout::vertical([
        Constraint::Length(7), // Positions panel
        Constraint::Length(5), // Market Analysis panel
        Constraint::Min(8),    // Transactions panel (fills remaining)
        Constraint::Length(1), // Footer status bar
    ])
    .split(f.area());

    // Render each panel
    widgets::render_positions(f, chunks[0], app);
    widgets::render_market_analysis(f, chunks[1], app);
    widgets::render_transactions(f, chunks[2], app);
    widgets::render_footer(f, chunks[3], app);

    // Render help overlay on top of everything
    if app.show_help {
        let overlay_area = centered_rect(52, 14, f.area());
        f.render_widget(Clear, overlay_area);

        let help_lines = vec![
            Line::from(""),
            Line::from(Span::styled("  Keybindings", THEME.title_style())),
            Line::from(""),
            Line::from(vec![
                Span::styled("  q / Esc", THEME.highlight_style()),
                Span::raw("          Quit"),
            ]),
            Line::from(vec![
                Span::styled("  j/k / Up/Down", THEME.highlight_style()),
                Span::raw("    Scroll transactions"),
            ]),
            Line::from(vec![
                Span::styled("  h / ?", THEME.highlight_style()),
                Span::raw("            Toggle help"),
            ]),
            Line::from(vec![
                Span::styled("  [/] / Left/Right", THEME.highlight_style()),
                Span::raw(" Switch market"),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  Press any key to close",
                THEME.inactive_style(),
            )),
        ];

        let block = Block::default()
            .title(Span::styled(" Help ", THEME.title_style()))
            .borders(Borders::ALL)
            .border_style(THEME.border_style());

        let paragraph = Paragraph::new(help_lines).block(block);
        f.render_widget(paragraph, overlay_area);
    }
}

/// Calculate a centered rectangle of given width/height within the parent area
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(x, y, w, h)
}
