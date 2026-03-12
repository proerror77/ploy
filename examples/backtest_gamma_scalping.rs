//! Gamma Scalping (Staggered Arb) Backtest Example
//!
//! Usage:
//!   cargo run --example backtest_gamma_scalping

use chrono::{Duration, Utc};
use ploy::strategy::backtest_feed::{HistoricalFeed, MarketUpdate, UpdateType};
use ploy::strategy::backtest_recorder::NullRecorder;
use ploy::strategy::staggered_arb_backtest::{
    StaggeredArbBacktestConfig, StaggeredArbBacktestEngine,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Gamma Scalping (Staggered Arb) Backtest");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    // Create synthetic test data
    let mut feed = create_synthetic_feed();

    // Configure backtest
    let config = StaggeredArbBacktestConfig {
        symbols: vec!["BTCUSDT".to_string()],
        initial_capital: dec!(1000),
        shares_per_trade: 10,
        max_concurrent_positions: 3,
        direction_threshold: 0.03,
        premium_sum_threshold: Decimal::ONE,
        premium_sum_direction_slope: 1.25,
        premium_sum_obi_slope: 0.25,
        reverse_signal: false,
        max_initial_sum: dec!(1.20),
        max_leg1_price: dec!(0.80),
        merge_target_sum: dec!(0.95),
        min_profit_target: dec!(0.02),
        max_wait_secs: 180,
        entry_after_start_max_secs: 30,
        no_trade_last_secs: 30,
        max_wait_pct: 0.40,
        min_time_remaining_secs: 60,
        max_leg1_loss: dec!(0),
        force_complete_threshold: dec!(0.95),
        protective_close_threshold: dec!(1.03),
        min_ask_price: dec!(0.05),
        min_entry_sum: dec!(0.70),
        allowed_window_durations: vec![300], // 5m only
        window_duration_tolerance: 30,
        min_leg2_delay_secs: 3,
        max_trades_per_event: 0, // unlimited
        mu: 0.0,
        vol_lookback_secs: 300,
        vol_floor: 0.005,
        min_entry_sigma: 0.005,
        max_entry_sigma: 0.03,
        cooldown_secs: 5,
        // Greeks integration
        use_greeks: true,
        min_gamma: 0.0,
        max_theta_cost: 0.0,
        max_fair_value_distance: 0.15,
        delta_weighted_sizing: false,
    };

    println!("Config:");
    println!("  Initial Capital: ${}", config.initial_capital);
    println!("  Shares/Trade: {}", config.shares_per_trade);
    println!("  Max Leg1 Price: ${}", config.max_leg1_price);
    println!("  Merge Target Sum: ${}", config.merge_target_sum);
    println!("  Min Profit Target: ${}", config.min_profit_target);
    println!(
        "  Direction Threshold: {:.1}%",
        config.direction_threshold * 100.0
    );
    println!("  Greeks Enabled: {}\n", config.use_greeks);

    // Run backtest
    let mut engine = StaggeredArbBacktestEngine::new(config, Box::new(NullRecorder));
    let results = engine.run(&mut feed);

    // Print engine summary (includes Greeks analysis)
    engine.print_staggered_summary();

    // Print results
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Results");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    println!("Total Trades: {}", results.total_trades);
    println!("Winning Trades: {}", results.winning_trades);
    println!("Losing Trades: {}", results.losing_trades);
    println!("Win Rate: {:.1}%", results.win_rate);
    println!();
    println!("Total P&L: ${:.2}", results.total_pnl);
    println!("Avg P&L/Trade: ${:.4}", results.avg_pnl_per_trade);
    println!("Profit Factor: {:.2}", results.profit_factor);
    println!();
    println!("Max Drawdown: ${:.2}", results.max_drawdown);
    println!("Sharpe Ratio: {:.2}", results.sharpe_ratio);
    println!("Avg Holding Time: {:.1}s", results.avg_holding_time_secs);
    println!();

    // Print closed trades
    let closed_trades = engine.closed_trades();
    if !closed_trades.is_empty() {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("  Closed Trades (first 10)");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

        for (i, trade) in closed_trades.iter().take(10).enumerate() {
            println!("Trade #{}", i + 1);
            println!("  Direction: {}", trade.leg1_direction);
            println!("  Leg1 Price: ${:.4}", trade.leg1_price);
            if let Some(leg2_price) = trade.leg2_price {
                println!("  Leg2 Price: ${:.4}", leg2_price);
                println!("  Total Sum: ${:.4}", trade.leg1_price + leg2_price);
            }
            println!("  P&L: ${:.4}", trade.pnl);
            println!("  Exit Reason: {}", trade.exit_reason);
            // Greeks at entry
            if let Some(delta) = trade.entry_delta {
                println!("  Greeks @ entry:");
                println!("    Delta: {:.6}", delta);
                println!("    Gamma: {:.6}", trade.entry_gamma.unwrap_or(0.0));
                println!("    Theta: {:.6}/s", trade.entry_theta.unwrap_or(0.0));
                println!(
                    "    Fair Value: {:.4}",
                    trade.entry_fair_value.unwrap_or(0.0)
                );
            }
            println!();
        }
    }
}

/// Create synthetic market data for testing
fn create_synthetic_feed() -> HistoricalFeed {
    let mut updates = Vec::new();
    let start_time = Utc::now() - Duration::hours(1);

    // Scenario: BTC price oscillates, creating staggered arb opportunities
    let event_slug = "btc-5m-test".to_string();
    let symbol = "BTCUSDT".to_string();

    // T=0: Event starts, BTC @ $100,000
    let t0 = start_time;
    updates.push(MarketUpdate {
        timestamp: t0,
        symbol: symbol.clone(),
        update_type: UpdateType::SpotTrade {
            price: dec!(100000),
            quantity: Some(dec!(1.0)),
        },
    });

    // T=1s: Event discovered
    updates.push(MarketUpdate {
        timestamp: t0 + Duration::seconds(1),
        symbol: symbol.clone(),
        update_type: UpdateType::EventState {
            event_slug: event_slug.clone(),
            end_time: Some(t0 + Duration::seconds(300)), // 5 min window
            price_to_beat: Some(dec!(100000)),
            outcome: None,
        },
    });

    // T=2s: Initial quotes (sum = $0.95, reasonable entry point)
    updates.push(MarketUpdate {
        timestamp: t0 + Duration::seconds(2),
        symbol: symbol.clone(),
        update_type: UpdateType::PmQuote {
            event_slug: event_slug.clone(),
            token_id: "up-token".to_string(),
            side: ploy::domain::Side::Up,
            best_bid: Some(dec!(0.45)),
            best_ask: Some(dec!(0.48)),
        },
    });
    updates.push(MarketUpdate {
        timestamp: t0 + Duration::seconds(2),
        symbol: symbol.clone(),
        update_type: UpdateType::PmQuote {
            event_slug: event_slug.clone(),
            token_id: "down-token".to_string(),
            side: ploy::domain::Side::Down,
            best_bid: Some(dec!(0.45)),
            best_ask: Some(dec!(0.47)),
        },
    });

    // T=10s: BTC rises to $100,200 (0.2% move)
    updates.push(MarketUpdate {
        timestamp: t0 + Duration::seconds(10),
        symbol: symbol.clone(),
        update_type: UpdateType::SpotTrade {
            price: dec!(100200),
            quantity: Some(dec!(0.5)),
        },
    });

    // T=11s: UP ask rises slightly, DOWN ask falls (sum = $0.98)
    updates.push(MarketUpdate {
        timestamp: t0 + Duration::seconds(11),
        symbol: symbol.clone(),
        update_type: UpdateType::PmQuote {
            event_slug: event_slug.clone(),
            token_id: "up-token".to_string(),
            side: ploy::domain::Side::Up,
            best_bid: Some(dec!(0.48)),
            best_ask: Some(dec!(0.51)),
        },
    });
    updates.push(MarketUpdate {
        timestamp: t0 + Duration::seconds(11),
        symbol: symbol.clone(),
        update_type: UpdateType::PmQuote {
            event_slug: event_slug.clone(),
            token_id: "down-token".to_string(),
            side: ploy::domain::Side::Down,
            best_bid: Some(dec!(0.44)),
            best_ask: Some(dec!(0.46)),
        },
    });

    // T=60s: BTC drops to $99,900 (0.1% down from start)
    updates.push(MarketUpdate {
        timestamp: t0 + Duration::seconds(60),
        symbol: symbol.clone(),
        update_type: UpdateType::SpotTrade {
            price: dec!(99900),
            quantity: Some(dec!(0.8)),
        },
    });

    // T=61s: DOWN ask rises, UP ask falls (sum = $0.88, profitable!)
    // This creates a profitable arbitrage: buy both for < $0.95 total
    updates.push(MarketUpdate {
        timestamp: t0 + Duration::seconds(61),
        symbol: symbol.clone(),
        update_type: UpdateType::PmQuote {
            event_slug: event_slug.clone(),
            token_id: "up-token".to_string(),
            side: ploy::domain::Side::Up,
            best_bid: Some(dec!(0.38)),
            best_ask: Some(dec!(0.40)),
        },
    });
    updates.push(MarketUpdate {
        timestamp: t0 + Duration::seconds(61),
        symbol: symbol.clone(),
        update_type: UpdateType::PmQuote {
            event_slug: event_slug.clone(),
            token_id: "down-token".to_string(),
            side: ploy::domain::Side::Down,
            best_bid: Some(dec!(0.45)),
            best_ask: Some(dec!(0.47)),
        },
    });

    // T=300s: Event settles (UP wins)
    updates.push(MarketUpdate {
        timestamp: t0 + Duration::seconds(300),
        symbol: symbol.clone(),
        update_type: UpdateType::EventState {
            event_slug: event_slug.clone(),
            end_time: Some(t0 + Duration::seconds(300)),
            price_to_beat: Some(dec!(100000)),
            outcome: Some(true), // UP wins
        },
    });

    HistoricalFeed::new(updates)
}
