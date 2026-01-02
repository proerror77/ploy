//! Footer status bar widget
//!
//! Displays trade stats, volume, and countdown timer.

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::tui::app::TuiApp;
use crate::tui::theme::THEME;

/// Render the footer status bar
pub fn render_footer(f: &mut Frame, area: Rect, app: &TuiApp) {
    let stats = &app.stats;

    // Format volume with commas
    let volume_str = format_currency(stats.volume);

    // Build status indicators
    let mut indicators = vec![];

    if stats.dry_run {
        indicators.push(Span::styled("[DRY RUN]", THEME.highlight_style()));
        indicators.push(Span::raw(" "));
    }

    if !stats.strategy_state.is_empty() {
        indicators.push(Span::styled(
            format!("[{}]", stats.strategy_state.to_uppercase()),
            THEME.border_style()
        ));
    }

    let line = Line::from(vec![
        Span::raw("  Trades: "),
        Span::styled(format!("{}", stats.trade_count), THEME.highlight_style()),
        Span::raw("  "),
        Span::styled("", THEME.inactive_style()),
        Span::raw("  Volume: "),
        Span::styled(volume_str, THEME.highlight_style()),
        Span::raw("  "),
        Span::styled("", THEME.inactive_style()),
        Span::raw("  "),
        Span::styled("", THEME.border_style()),
        Span::raw(" "),
        Span::styled(stats.formatted_remaining(), THEME.highlight_style()),
        Span::raw("  "),
        Span::styled("", THEME.inactive_style()),
        Span::raw("  "),
    ].into_iter().chain(indicators).collect::<Vec<_>>());

    let paragraph = Paragraph::new(line);
    f.render_widget(paragraph, area);
}

/// Format currency with commas and 2 decimal places
fn format_currency(value: rust_decimal::Decimal) -> String {
    let n = value.to_string().parse::<f64>().unwrap_or(0.0);
    let int_part = n.trunc() as i64;
    let frac_part = ((n.fract() * 100.0).round() as i64).abs();

    let int_str = format_with_commas(int_part.unsigned_abs());
    let sign = if int_part < 0 { "-" } else { "" };

    format!("{}${}.{:02}", sign, int_str, frac_part)
}

/// Format a number with commas
fn format_with_commas(n: u64) -> String {
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
