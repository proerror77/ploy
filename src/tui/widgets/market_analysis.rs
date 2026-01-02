//! Market Analysis panel widget
//!
//! Displays prices, combined sum, spread, and market statistics.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::tui::app::TuiApp;
use crate::tui::theme::THEME;

/// Render the market analysis panel
pub fn render_market_analysis(f: &mut Frame, area: Rect, app: &TuiApp) {
    let block = Block::default()
        .title(" MARKET ANALYSIS ")
        .title_style(THEME.title_style())
        .borders(Borders::ALL)
        .border_style(THEME.border_style());

    let inner = block.inner(area);
    f.render_widget(block, area);

    let market = &app.market;

    let chunks = Layout::vertical([
        Constraint::Length(1), // Prices line
        Constraint::Length(1), // Stats line
    ]).split(inner);

    // Line 1: UP/DOWN prices and combined
    let spread_style = if market.spread_pct <= dec!(0) {
        THEME.profit_style()
    } else {
        THEME.loss_style()
    };

    let combined_style = if market.combined <= dec!(1) {
        THEME.profit_style()
    } else if market.combined <= dec!(1.02) {
        THEME.highlight_style()
    } else {
        THEME.loss_style()
    };

    let line1 = Line::from(vec![
        Span::raw("  UP: "),
        Span::styled(format!("${:.4}", market.up_price), THEME.up_style()),
        Span::raw("   DOWN: "),
        Span::styled(format!("${:.4}", market.down_price), THEME.down_style()),
        Span::raw("   Combined: "),
        Span::styled(format!("${:.4}", market.combined), combined_style),
        Span::raw("   Spread: "),
        Span::styled(format!("{:+.2}%", market.spread_pct), spread_style),
    ]);

    f.render_widget(Paragraph::new(line1), chunks[0]);

    // Line 2: Pairs, Delta, Total PnL
    let pnl_style = THEME.pnl_style(market.total_pnl >= Decimal::ZERO);
    let pnl_str = if market.total_pnl >= Decimal::ZERO {
        format!("${:+.2}", market.total_pnl)
    } else {
        format!("${:.2}", market.total_pnl)
    };

    let delta_style = if market.delta == 0 {
        THEME.text_style()
    } else if market.delta > 0 {
        THEME.up_style()
    } else {
        THEME.down_style()
    };

    let line2 = Line::from(vec![
        Span::raw("  Pairs: "),
        Span::styled(format_number(market.pairs), THEME.highlight_style()),
        Span::raw(" | Delta: "),
        Span::styled(format!("{:+}", market.delta), delta_style),
        Span::raw(" | Total PnL: "),
        Span::styled(pnl_str, pnl_style),
    ]);

    f.render_widget(Paragraph::new(line2), chunks[1]);
}

/// Format a number with commas
fn format_number(n: u64) -> String {
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
