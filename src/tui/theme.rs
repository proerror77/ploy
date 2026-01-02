//! Theme and color definitions for the TUI dashboard
//!
//! Cyberpunk-style color scheme with cyan borders, green UP, red DOWN.

use ratatui::style::{Color, Modifier, Style};

/// Theme configuration for the dashboard
#[derive(Debug, Clone)]
pub struct Theme {
    /// Border color (cyan)
    pub border: Color,
    /// Title color
    pub title: Color,
    /// UP side color (green)
    pub up: Color,
    /// DOWN side color (red)
    pub down: Color,
    /// Profit color (green)
    pub profit: Color,
    /// Loss color (red)
    pub loss: Color,
    /// Highlight/accent color (yellow)
    pub highlight: Color,
    /// Inactive/dim color
    pub inactive: Color,
    /// Normal text color
    pub text: Color,
    /// Background color
    pub bg: Color,
    /// Progress bar filled color
    pub progress_filled: Color,
    /// Progress bar empty color
    pub progress_empty: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            border: Color::Cyan,
            title: Color::Cyan,
            up: Color::Green,
            down: Color::Red,
            profit: Color::Green,
            loss: Color::Red,
            highlight: Color::Yellow,
            inactive: Color::DarkGray,
            text: Color::White,
            bg: Color::Reset,
            progress_filled: Color::Green,
            progress_empty: Color::DarkGray,
        }
    }
}

impl Theme {
    /// Get style for borders
    pub fn border_style(&self) -> Style {
        Style::default().fg(self.border)
    }

    /// Get style for titles
    pub fn title_style(&self) -> Style {
        Style::default().fg(self.title).add_modifier(Modifier::BOLD)
    }

    /// Get style for UP side
    pub fn up_style(&self) -> Style {
        Style::default().fg(self.up)
    }

    /// Get style for DOWN side
    pub fn down_style(&self) -> Style {
        Style::default().fg(self.down)
    }

    /// Get style for profit values
    pub fn profit_style(&self) -> Style {
        Style::default().fg(self.profit)
    }

    /// Get style for loss values
    pub fn loss_style(&self) -> Style {
        Style::default().fg(self.loss)
    }

    /// Get style for highlighted text
    pub fn highlight_style(&self) -> Style {
        Style::default().fg(self.highlight)
    }

    /// Get style for inactive/dim text
    pub fn inactive_style(&self) -> Style {
        Style::default().fg(self.inactive)
    }

    /// Get style for normal text
    pub fn text_style(&self) -> Style {
        Style::default().fg(self.text)
    }

    /// Get style for PnL based on value (positive = profit, negative = loss)
    pub fn pnl_style(&self, is_positive: bool) -> Style {
        if is_positive {
            self.profit_style()
        } else {
            self.loss_style()
        }
    }
}

/// Global theme instance
pub static THEME: std::sync::LazyLock<Theme> = std::sync::LazyLock::new(Theme::default);
