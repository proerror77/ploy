//! Main UI rendering logic
//!
//! Orchestrates the layout and renders all widgets.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::tui::app::{ActiveTab, ModalState, TuiApp};
use crate::tui::theme::THEME;
use crate::tui::widgets;

const KEYBINDINGS: &[(&str, &str)] = &[
    ("q / Ctrl+C", "Quit"),
    ("j/k / Up/Down", "Scroll transactions"),
    ("h / ?", "Toggle help"),
    ("[ / ] / Left/Right", "Switch market"),
    ("Tab", "Toggle view"),
    ("/", "Filter transactions"),
    ("p", "Pause agents (confirm)"),
    ("r", "Resume agents (confirm)"),
    ("x", "Emergency close (confirm)"),
    ("Esc / n", "Cancel modal/filter"),
    ("Enter / y", "Confirm modal"),
];

/// Render the entire UI
pub fn render(f: &mut Frame, app: &TuiApp) {
    match app.active_tab {
        ActiveTab::Portfolio => render_portfolio(f, app),
        ActiveTab::AgentMonitor => render_agent_monitor(f, app),
    }

    // Render help overlay on top of everything
    if app.show_help {
        render_help(f, app);
    }

    // Render modal overlay
    if let Some(modal) = &app.modal {
        render_modal(f, app, modal);
    }

    // Render filter input overlay (only while editing)
    if app.filter_mode {
        render_filter_overlay(f, app);
    }
}

fn render_portfolio(f: &mut Frame, app: &TuiApp) {
    let chunks = Layout::vertical([
        Constraint::Length(7), // Positions panel
        Constraint::Length(5), // Market Analysis panel
        Constraint::Length(5), // Risk panel
        Constraint::Min(8),    // Transactions panel (fills remaining)
        Constraint::Length(1), // Footer status bar
    ])
    .split(f.area());

    widgets::render_positions(f, chunks[0], app);
    widgets::render_market_analysis(f, chunks[1], app);
    widgets::render_risk_status(f, chunks[2], app);
    widgets::render_transactions(f, chunks[3], app);
    widgets::render_footer(f, chunks[4], app);
}

fn render_agent_monitor(f: &mut Frame, app: &TuiApp) {
    let chunks = Layout::vertical([
        Constraint::Length(5), // Risk panel
        Constraint::Min(10),   // Agents table
        Constraint::Length(1), // Footer
    ])
    .split(f.area());

    widgets::render_risk_status(f, chunks[0], app);
    widgets::render_agent_status(f, chunks[1], &app.agent_snapshots);
    widgets::render_footer(f, chunks[2], app);
}

fn render_help(f: &mut Frame, _app: &TuiApp) {
    let overlay_height = (KEYBINDINGS.len() as u16) + 5;
    let overlay_area = centered_rect(60, overlay_height, f.area());
    f.render_widget(Clear, overlay_area);

    let mut help_lines: Vec<Line> = Vec::with_capacity(KEYBINDINGS.len() + 6);
    help_lines.push(Line::from(""));
    help_lines.push(Line::from(Span::styled("  Keybindings", THEME.title_style())));
    help_lines.push(Line::from(""));

    for (keys, desc) in KEYBINDINGS {
        help_lines.push(Line::from(vec![
            Span::styled(format!("  {}", keys), THEME.highlight_style()),
            Span::raw(format!("  {}", desc)),
        ]));
    }

    help_lines.push(Line::from(""));
    help_lines.push(Line::from(Span::styled(
        "  Press h/? to close",
        THEME.inactive_style(),
    )));

    let block = Block::default()
        .title(Span::styled(" Help ", THEME.title_style()))
        .borders(Borders::ALL)
        .border_style(THEME.border_style());

    let paragraph = Paragraph::new(help_lines).block(block);
    f.render_widget(paragraph, overlay_area);
}

fn render_modal(f: &mut Frame, app: &TuiApp, modal: &ModalState) {
    let overlay_area = centered_rect(66, 9, f.area());
    f.render_widget(Clear, overlay_area);

    let message = match modal {
        ModalState::Confirm { message, .. } => message.as_str(),
    };

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled("  Confirm", THEME.title_style())),
        Line::from(""),
        Line::from(Span::raw(format!("  {}", message))),
        Line::from(""),
    ];

    if app.stats.dry_run {
        lines.push(Line::from(Span::styled(
            "  DRY RUN: commands will not execute live trades",
            THEME.highlight_style(),
        )));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        "  Enter/y = confirm    Esc/n = cancel",
        THEME.inactive_style(),
    )));

    let block = Block::default()
        .title(Span::styled(" Action ", THEME.title_style()))
        .borders(Borders::ALL)
        .border_style(THEME.border_style());

    f.render_widget(Paragraph::new(lines).block(block), overlay_area);
}

fn render_filter_overlay(f: &mut Frame, app: &TuiApp) {
    let overlay_area = centered_rect(66, 5, f.area());
    f.render_widget(Clear, overlay_area);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled("  Filter Transactions", THEME.title_style())),
        Line::from(Span::styled(
            format!("  /{}", app.filter_input),
            THEME.highlight_style(),
        )),
        Line::from(Span::styled(
            "  Enter = apply    Esc = cancel",
            THEME.inactive_style(),
        )),
    ];

    let block = Block::default()
        .title(Span::styled(" Filter ", THEME.title_style()))
        .borders(Borders::ALL)
        .border_style(THEME.border_style());

    f.render_widget(Paragraph::new(lines).block(block), overlay_area);
}

/// Calculate a centered rectangle of given width/height within the parent area
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(x, y, w, h)
}
