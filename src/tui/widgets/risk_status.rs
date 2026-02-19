//! Risk Status widget — shows daily loss, queue depth, circuit breaker, exposure

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Row, Table};

use crate::tui::app::TuiApp;

/// Render the risk status panel (~4 lines tall)
pub fn render_risk_status(f: &mut Frame, area: Rect, app: &TuiApp) {
    let risk = &app.risk_state;

    let state_color = match risk.state.as_str() {
        "Normal" => Color::Green,
        "Elevated" => Color::Yellow,
        "Halted" => Color::Red,
        _ => Color::Gray,
    };

    let cb_color = match risk.circuit_breaker.as_str() {
        "Closed" => Color::Green,
        "HalfOpen" => Color::Yellow,
        "Open" => Color::Red,
        _ => Color::Gray,
    };

    let loss_pct = if risk.daily_loss_limit > rust_decimal::Decimal::ZERO {
        ((risk.daily_loss_used / risk.daily_loss_limit) * rust_decimal::Decimal::from(100)).round()
    } else {
        rust_decimal::Decimal::ZERO
    };

    let rows = vec![
        Row::new(vec![
            Span::styled("Risk State", Style::default().fg(Color::DarkGray)),
            Span::styled(risk.state.clone(), Style::default().fg(state_color)),
            Span::styled("Circuit Breaker", Style::default().fg(Color::DarkGray)),
            Span::styled(risk.circuit_breaker.clone(), Style::default().fg(cb_color)),
        ]),
        Row::new(vec![
            Span::styled("Daily Loss", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!(
                    "${} / ${} ({}%)",
                    risk.daily_loss_used, risk.daily_loss_limit, loss_pct
                ),
                Style::default().fg(if loss_pct > rust_decimal::Decimal::from(80) {
                    Color::Red
                } else if loss_pct > rust_decimal::Decimal::from(50) {
                    Color::Yellow
                } else {
                    Color::Green
                }),
            ),
            Span::styled("Queue Depth", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} pending", risk.queue_depth),
                Style::default().fg(if risk.queue_depth > 10 {
                    Color::Yellow
                } else {
                    Color::White
                }),
            ),
        ]),
        Row::new(vec![
            Span::styled("Exposure", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("${}", risk.total_exposure),
                Style::default().fg(Color::White),
            ),
            Span::raw(""),
            Span::raw(""),
        ]),
    ];

    let table = Table::new(
        rows,
        [
            Constraint::Length(16),
            Constraint::Length(28),
            Constraint::Length(16),
            Constraint::Length(20),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Risk Status ")
            .title_style(
                Style::default()
                    .fg(state_color)
                    .add_modifier(Modifier::BOLD),
            )
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    f.render_widget(table, area);
}
