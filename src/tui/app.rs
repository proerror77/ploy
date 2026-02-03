//! TUI Application state management
//!
//! Manages all display state for the dashboard.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::domain::Side;
use crate::tui::data::{DashboardStats, DisplayPosition, DisplayTransaction, MarketState};

/// Maximum number of transactions to keep in history
const MAX_TRANSACTIONS: usize = 100;

/// TUI Application state
pub struct TuiApp {
    /// Active positions
    pub positions: Vec<DisplayPosition>,
    /// Current market state
    pub market: MarketState,
    /// Recent transactions (newest first)
    pub transactions: Vec<DisplayTransaction>,
    /// Dashboard statistics
    pub stats: DashboardStats,
    /// Scroll offset for transactions list
    pub tx_scroll_offset: usize,
    /// Is the app running
    pub running: bool,
    /// Last update timestamp
    pub last_update: DateTime<Utc>,
    /// Show help overlay
    pub show_help: bool,
    /// Available markets for switching
    pub available_markets: Vec<String>,
    /// Currently selected market index
    pub selected_market_idx: usize,
}

impl Default for TuiApp {
    fn default() -> Self {
        Self::new()
    }
}

impl TuiApp {
    /// Create a new TUI app with default state
    pub fn new() -> Self {
        Self {
            positions: Vec::new(),
            market: MarketState::default(),
            transactions: Vec::new(),
            stats: DashboardStats::default(),
            tx_scroll_offset: 0,
            running: true,
            last_update: Utc::now(),
            show_help: false,
            available_markets: vec![
                "SOL-15m".to_string(),
                "ETH-15m".to_string(),
                "BTC-Daily".to_string(),
            ],
            selected_market_idx: 0,
        }
    }

    /// Check if app should continue running
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Signal the app to quit
    pub fn quit(&mut self) {
        self.running = false;
    }

    /// Update market quotes
    pub fn update_quotes(
        &mut self,
        up_bid: Decimal,
        up_ask: Decimal,
        down_bid: Decimal,
        down_ask: Decimal,
        up_size: Decimal,
        down_size: Decimal,
    ) {
        self.market = MarketState::from_quotes(
            up_bid, up_ask, down_bid, down_ask, up_size, down_size
        );

        // Update position-related market stats
        let (up_shares, down_shares, total_pnl) = self.calculate_position_stats();
        self.market.with_positions(up_shares, down_shares, total_pnl);

        self.last_update = Utc::now();
    }

    /// Calculate position statistics
    fn calculate_position_stats(&self) -> (u64, u64, Decimal) {
        let mut up_shares = 0u64;
        let mut down_shares = 0u64;
        let mut total_pnl = Decimal::ZERO;

        for pos in &self.positions {
            match pos.side {
                Side::Up => up_shares += pos.shares,
                Side::Down => down_shares += pos.shares,
            }
            total_pnl += pos.pnl;
        }

        (up_shares, down_shares, total_pnl)
    }

    /// Update or add a position
    pub fn update_position(&mut self, side: Side, shares: u64, current_price: Decimal, avg_price: Decimal) {
        // Remove existing position for this side
        self.positions.retain(|p| p.side != side);

        if shares > 0 {
            self.positions.push(DisplayPosition::new(side, shares, current_price, avg_price));
        }

        // Re-sort: UP first, then DOWN
        self.positions.sort_by(|a, b| {
            match (&a.side, &b.side) {
                (Side::Up, Side::Down) => std::cmp::Ordering::Less,
                (Side::Down, Side::Up) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            }
        });
    }

    /// Clear all positions
    pub fn clear_positions(&mut self) {
        self.positions.clear();
    }

    /// Add a new transaction
    pub fn add_transaction(&mut self, tx: DisplayTransaction) {
        self.transactions.insert(0, tx);

        // Keep only MAX_TRANSACTIONS
        if self.transactions.len() > MAX_TRANSACTIONS {
            self.transactions.truncate(MAX_TRANSACTIONS);
        }

        // Update stats
        self.stats.trade_count += 1;
    }

    /// Update volume
    pub fn add_volume(&mut self, amount: Decimal) {
        self.stats.volume += amount;
    }

    /// Set round end time
    pub fn set_round_end_time(&mut self, end_time: Option<DateTime<Utc>>) {
        self.stats.round_end_time = end_time;
    }

    /// Set strategy state
    pub fn set_strategy_state(&mut self, state: &str) {
        self.stats.strategy_state = state.to_string();
    }

    /// Set dry run mode
    pub fn set_dry_run(&mut self, dry_run: bool) {
        self.stats.dry_run = dry_run;
    }

    /// Scroll transactions up
    pub fn scroll_up(&mut self) {
        self.tx_scroll_offset = self.tx_scroll_offset.saturating_sub(1);
    }

    /// Scroll transactions down
    pub fn scroll_down(&mut self) {
        if self.tx_scroll_offset < self.transactions.len().saturating_sub(1) {
            self.tx_scroll_offset += 1;
        }
    }

    /// Reset scroll to top
    pub fn scroll_to_top(&mut self) {
        self.tx_scroll_offset = 0;
    }

    /// Toggle help overlay
    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    /// Switch to next market
    pub fn next_market(&mut self) {
        if !self.available_markets.is_empty() {
            self.selected_market_idx = (self.selected_market_idx + 1) % self.available_markets.len();
        }
    }

    /// Switch to previous market
    pub fn prev_market(&mut self) {
        if !self.available_markets.is_empty() {
            if self.selected_market_idx == 0 {
                self.selected_market_idx = self.available_markets.len() - 1;
            } else {
                self.selected_market_idx -= 1;
            }
        }
    }

    /// Create demo data for testing
    pub fn with_demo_data(mut self) -> Self {
        // Demo positions
        self.positions.push(DisplayPosition::new(
            Side::Up,
            36598,
            dec!(0.4820),
            dec!(0.4830),
        ));
        self.positions.push(DisplayPosition::new(
            Side::Down,
            36317,
            dec!(0.5420),
            dec!(0.4743),
        ));

        // Demo market
        self.market = MarketState::from_quotes(
            dec!(0.4780), dec!(0.4816),
            dec!(0.5380), dec!(0.5423),
            dec!(1000), dec!(1200),
        );
        self.market.with_positions(36598, 36317, dec!(2417));

        // Demo transactions
        for i in 0..10 {
            let side = if i % 2 == 0 { Side::Up } else { Side::Down };
            let price = if side == Side::Up { dec!(0.4602) } else { dec!(0.4983) };
            self.transactions.push(DisplayTransaction::new(
                Utc::now() - chrono::Duration::seconds(i * 5),
                side,
                price,
                287 + (i as u64 * 50),
                dec!(97136) + Decimal::from(i * 100),
                format!("0x{:016x}abcdef", i),
            ));
        }

        // Demo stats
        self.stats.trade_count = 127;
        self.stats.volume = dec!(34902.87);
        self.stats.round_end_time = Some(Utc::now() + chrono::Duration::seconds(27));
        self.stats.dry_run = true;
        self.stats.strategy_state = "watching".to_string();

        self
    }
}
