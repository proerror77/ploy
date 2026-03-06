use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::{Direction, ExitConfig};

/// An open position.
#[derive(Debug, Clone)]
pub struct Position {
    pub token_id: String,
    pub symbol: String,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub entry_notional: Decimal,
    pub shares: u64,
    pub entry_time: DateTime<Utc>,
    pub highest_price: Decimal,
    pub event_end_time: DateTime<Utc>,
    pub event_slug: String,
    pub condition_id: String,
    /// P_hat at entry time (for probability-stop exit rule)
    pub entry_p_hat: Option<f64>,
    /// Chainlink open price (S0) at window start
    pub window_open_price: Option<Decimal>,
}

impl Position {
    /// Calculate current P&L percentage.
    pub fn pnl_pct(&self, current_price: Decimal) -> Decimal {
        if self.entry_price.is_zero() {
            return Decimal::ZERO;
        }
        (current_price - self.entry_price) / self.entry_price
    }

    /// Update highest price seen (for trailing stop).
    pub fn update_high(&mut self, price: Decimal) {
        if price > self.highest_price {
            self.highest_price = price;
        }
    }

    /// Time remaining until event resolution.
    pub fn time_to_resolution(&self) -> ChronoDuration {
        self.event_end_time - Utc::now()
    }
}

/// Reason for exiting a position.
#[derive(Debug, Clone)]
pub enum ExitReason {
    TakeProfit {
        profit_pct: Decimal,
    },
    StopLoss {
        loss_pct: Decimal,
    },
    TrailingStop {
        high: Decimal,
        current: Decimal,
    },
    TimeExit,
    Manual,
    /// Probability model thesis invalidated (p_hat dropped below threshold)
    ProbabilityStop {
        entry_p_hat: f64,
        current_p_hat: f64,
    },
    /// Hard loss limit per trade
    HardStop {
        loss_usd: Decimal,
    },
}

impl std::fmt::Display for ExitReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExitReason::TakeProfit { profit_pct } => {
                write!(f, "TakeProfit({:.1}%)", profit_pct * dec!(100))
            }
            ExitReason::StopLoss { loss_pct } => {
                write!(f, "StopLoss({:.1}%)", loss_pct * dec!(100))
            }
            ExitReason::TrailingStop { high, current } => {
                write!(
                    f,
                    "TrailingStop(high={:.2}¢, cur={:.2}¢)",
                    high * dec!(100),
                    current * dec!(100)
                )
            }
            ExitReason::TimeExit => write!(f, "TimeExit"),
            ExitReason::Manual => write!(f, "Manual"),
            ExitReason::ProbabilityStop {
                entry_p_hat,
                current_p_hat,
            } => {
                write!(
                    f,
                    "ProbStop(entry={:.0}%→{:.0}%)",
                    entry_p_hat * 100.0,
                    current_p_hat * 100.0
                )
            }
            ExitReason::HardStop { loss_usd } => write!(f, "HardStop(${:.2})", loss_usd),
        }
    }
}

/// Manages position exits.
pub struct ExitManager {
    config: ExitConfig,
}

impl ExitManager {
    pub fn new(config: ExitConfig) -> Self {
        Self { config }
    }

    /// Check if position should be exited.
    pub fn check_exit(&self, pos: &Position, current_bid: Decimal) -> Option<ExitReason> {
        let pnl_pct = pos.pnl_pct(current_bid);

        if pnl_pct >= self.config.take_profit_pct {
            return Some(ExitReason::TakeProfit {
                profit_pct: pnl_pct,
            });
        }

        if pnl_pct <= -self.config.stop_loss_pct {
            return Some(ExitReason::StopLoss { loss_pct: -pnl_pct });
        }

        if pos.highest_price > pos.entry_price && current_bid < pos.highest_price {
            let drop_from_high = (pos.highest_price - current_bid) / pos.highest_price;
            if drop_from_high >= self.config.trailing_stop_pct {
                return Some(ExitReason::TrailingStop {
                    high: pos.highest_price,
                    current: current_bid,
                });
            }
        }

        let time_to_resolution = pos.time_to_resolution();
        if time_to_resolution.num_seconds() < self.config.exit_before_resolution_secs as i64 {
            return Some(ExitReason::TimeExit);
        }

        None
    }
}
