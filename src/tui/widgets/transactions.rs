//! Recent Transactions panel widget
//!
//! Displays a scrollable table of recent transactions.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use crate::domain::Side;
use crate::tui::app::TuiApp;
use crate::tui::data::DisplayTransaction;
use crate::tui::theme::THEME;

/// Render the transactions panel
pub fn render_transactions(f: &mut Frame, area: Rect, app: &TuiApp) {
    let block = Block::default()
        .title(" RECENT TRANSACTIONS ")
        .title_style(THEME.title_style())
        .borders(Borders::ALL)
        .border_style(THEME.border_style());

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.transactions.is_empty() {
        let no_tx = Paragraph::new("No recent transactions")
            .style(THEME.inactive_style());
        f.render_widget(no_tx, inner);
        return;
    }

    // Header line
    let header = Line::from(vec![
        Span::styled("  TIME        ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("SIDE   ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("  PRICE   ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("   SIZE   ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(" BTC PRICE  ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("TX HASH", Style::default().add_modifier(Modifier::BOLD)),
    ]);

    // Calculate visible rows
    let header_height = 1;
    let visible_rows = (inner.height as usize).saturating_sub(header_height);

    // Get slice of transactions to display
    let start_idx = app.tx_scroll_offset;
    let end_idx = (start_idx + visible_rows).min(app.transactions.len());
    let visible_txs = &app.transactions[start_idx..end_idx];

    // Build lines
    let mut lines: Vec<Line> = vec![header];

    for tx in visible_txs {
        lines.push(render_transaction_row(tx));
    }

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, inner);

    // Render scrollbar if needed
    if app.transactions.len() > visible_rows {
        let scrollbar = Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some(""))
            .end_symbol(Some(""));

        let mut scrollbar_state = ScrollbarState::default()
            .content_length(app.transactions.len())
            .position(app.tx_scroll_offset);

        f.render_stateful_widget(
            scrollbar,
            area,
            &mut scrollbar_state,
        );
    }
}

/// Render a single transaction row
fn render_transaction_row(tx: &DisplayTransaction) -> Line<'static> {
    let (side_arrow, side_str, side_style) = match tx.side {
        Side::Up => ("", " UP  ", THEME.up_style()),
        Side::Down => ("", " DOWN", THEME.down_style()),
    };

    Line::from(vec![
        Span::raw("  "),
        Span::raw(tx.formatted_time()),
        Span::raw("  "),
        Span::styled(side_arrow, side_style),
        Span::styled(side_str, side_style),
        Span::raw(format!("  ${:.4}", tx.price)),
        Span::raw(format!("  {:>6} $", tx.size)),
        Span::raw(format!("  {:>8}", format_btc_price(tx.btc_price))),
        Span::raw("  "),
        Span::styled(tx.short_hash(), THEME.inactive_style()),
    ])
}

/// Format BTC price with commas
fn format_btc_price(price: rust_decimal::Decimal) -> String {
    let n = price.to_string().parse::<f64>().unwrap_or(0.0) as u64;
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.insert(0, ',');
        }
        result.insert(0, c);
    }
    result
}
