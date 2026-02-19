//! Agent Status widget — displays coordinator agent states in the TUI

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Row, Table};

use crate::tui::data::DisplayAgent;

/// Render the agent status panel
pub fn render_agent_status(f: &mut Frame, area: Rect, agents: &[DisplayAgent]) {
    let header_cells = [
        "Agent",
        "Domain",
        "Status",
        "Pos",
        "Exposure",
        "PnL",
        "WR",
        "LS",
        "Mult",
        "Heartbeat",
    ]
    .iter()
    .map(|h| {
        Cell::from(*h).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    });
    let header = Row::new(header_cells).height(1);

    let rows = agents.iter().map(|a| {
        let status_color = match a.status.as_str() {
            "Running" => Color::Green,
            "Paused" => Color::Yellow,
            "Stopped" => Color::Red,
            "Error" => Color::Red,
            _ => Color::Gray,
        };

        let pnl_color = if a.daily_pnl >= rust_decimal::Decimal::ZERO {
            Color::Green
        } else {
            Color::Red
        };

        let win_rate = a
            .win_rate
            .map(|wr| format!("{:.1}%", wr * 100.0))
            .unwrap_or_else(|| "-".to_string());
        let loss_streak = a
            .loss_streak
            .map(|ls| ls.to_string())
            .unwrap_or_else(|| "-".to_string());
        let multiplier = a
            .size_multiplier
            .map(|m| format!("{:.2}x", m))
            .unwrap_or_else(|| "-".to_string());

        Row::new(vec![
            Cell::from(a.name.clone()).style(Style::default().fg(Color::White)),
            Cell::from(a.domain.clone()).style(Style::default().fg(Color::Magenta)),
            Cell::from(a.status.clone()).style(Style::default().fg(status_color)),
            Cell::from(a.position_count.to_string()).style(Style::default().fg(Color::White)),
            Cell::from(format!("${}", a.exposure)).style(Style::default().fg(Color::White)),
            Cell::from(format!("${}", a.daily_pnl)).style(Style::default().fg(pnl_color)),
            Cell::from(win_rate).style(Style::default().fg(Color::White)),
            Cell::from(loss_streak).style(Style::default().fg(Color::White)),
            Cell::from(multiplier).style(Style::default().fg(Color::White)),
            Cell::from(a.last_heartbeat.clone()).style(Style::default().fg(Color::DarkGray)),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(16),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(5),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(5),
            Constraint::Length(8),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Agents ")
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    f.render_widget(table, area);
}
