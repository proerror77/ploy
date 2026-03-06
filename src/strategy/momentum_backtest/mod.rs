//! Momentum strategy backtest engine.
//!
//! Reuses the live `MomentumDetector.check()` and `SpotPrice` types to ensure
//! the backtest signal logic is identical to production. "Change strategy once,
//! live + backtest automatically agree."
//!
//! Usage:
//!   ploy strategy backtest momentum --symbols BTCUSDT --save --json

mod config;
mod engine;
mod persistence;

pub use config::MomentumBacktestConfig;
pub use engine::MomentumBacktestEngine;
pub use persistence::save_backtest_results;

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    use super::{MomentumBacktestConfig, MomentumBacktestEngine};
    use ploy_backtest::{HistoricalFeed, MarketUpdate};

    /// Build a simple mock feed for testing
    fn mock_feed(updates: Vec<MarketUpdate>) -> HistoricalFeed {
        HistoricalFeed::new(updates)
    }

    #[test]
    fn test_engine_empty_feed() {
        let config =
            MomentumBacktestConfig::default_with_symbols(vec!["BTCUSDT".into()], dec!(10000));
        let mut engine = MomentumBacktestEngine::new(config);
        let mut feed = mock_feed(vec![]);
        let results = engine.run(&mut feed);

        assert_eq!(results.total_trades, 0);
        assert_eq!(results.total_pnl, Decimal::ZERO);
    }

    #[test]
    fn test_sharpe_calculation() {
        // With no trades, sharpe should be 0
        assert_eq!(
            ploy_backtest::strategies::calculate_momentum_sharpe(&[]),
            0.0
        );
    }
}
