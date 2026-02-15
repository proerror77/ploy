//! Positions panel widget
//!
//! Displays UP/DOWN positions with progress bars and PnL.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use rust_decimal::Decimal;

use crate::domain::Side;
use crate::tui::app::TuiApp;
use crate::tui::data::DisplayPosition;
use crate::tui::theme::THEME;

/// Render the positions panel
pub fn render_positions(f: &mut Frame, area: Rect, app: &TuiApp) {
    let block = Block::default()
        .title(" POSITIONS ")
        .title_style(THEME.title_style())
        .borders(Borders::ALL)
        .border_style(THEME.border_style());

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.positions.is_empty() {
        let no_pos = Paragraph::new("No open positions").style(THEME.inactive_style());
        f.render_widget(no_pos, inner);
        return;
    }

    // Split inner area for each position (up to 2)
    let chunks = Layout::vertical(
        app.positions
            .iter()
            .map(|_| Constraint::Length(3))
            .collect::<Vec<_>>(),
    )
    .split(inner);

    for (i, pos) in app.positions.iter().enumerate() {
        if i < chunks.len() {
            render_position_row(f, chunks[i], pos);
        }
    }
}

/// Render a single position row
fn render_position_row(f: &mut Frame, area: Rect, pos: &DisplayPosition) {
    let chunks = Layout::vertical([
        Constraint::Length(1), // Main line with progress bar
        Constraint::Length(1), // Cost/avg line
    ])
    .split(area);

    // Side indicator
    let (side_str, side_style) = match pos.side {
        Side::Up => (" UP  ", THEME.up_style()),
        Side::Down => (" DOWN", THEME.down_style()),
    };

    let arrow = match pos.side {
        Side::Up => "",
        Side::Down => "",
    };

    // Progress bar
    let bar_width = 20usize;
    let filled = (pos.progress_ratio() * bar_width as f64).round() as usize;
    let empty = bar_width.saturating_sub(filled);

    let bar_color = match pos.side {
        Side::Up => Color::Green,
        Side::Down => Color::Red,
    };

    let progress_bar = format!("[{}{}]", "".repeat(filled), "".repeat(empty));

    // PnL formatting
    let pnl_style = THEME.pnl_style(pos.pnl >= Decimal::ZERO);
    let pnl_str = if pos.pnl >= Decimal::ZERO {
        format!("${:+.0}", pos.pnl)
    } else {
        format!("${:.0}", pos.pnl)
    };

    // Main line
    let main_line = Line::from(vec![
        Span::styled(arrow, side_style),
        Span::styled(side_str, side_style),
        Span::raw(" "),
        Span::styled(progress_bar, Style::default().fg(bar_color)),
        Span::raw(format!("  {:>6}", format_shares(pos.shares))),
        Span::raw(format!("  @{:.3}", pos.current_price)),
        Span::raw("  PnL: "),
        Span::styled(pnl_str, pnl_style),
    ]);

    f.render_widget(Paragraph::new(main_line), chunks[0]);

    // Cost/avg line
    let detail_line = Line::from(vec![
        Span::raw("        "),
        Span::styled(
            format!("Cost: ${:.2} | Avg: ${:.4}", pos.cost, pos.avg_price),
            THEME.inactive_style(),
        ),
    ]);

    f.render_widget(Paragraph::new(detail_line), chunks[1]);
}

/// Format share count with commas
fn format_shares(shares: u64) -> String {
    let s = shares.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.insert(0, ',');
        }
        result.insert(0, c);
    }
    result
}
