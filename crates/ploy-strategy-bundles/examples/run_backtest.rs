//! Runnable backtest example for pm_5m_directional.
//!
//! Usage:
//!   cargo run -p ploy-strategy-bundles --example run_backtest -- [config.toml]
//!
//! If no config is given, uses built-in defaults.
//! Generates synthetic market data simulating 1 hour of BTC/ETH/SOL
//! 5-minute binary option windows on Polymarket.

use chrono::{Duration, Utc};
use ploy_strategy_bundles::{
    config::FullConfig, DirectionalStrategy, HistoricalFeed, MarketUpdate, NullRecorder,
    RuntimeConfig, RuntimeMode, SimulatedExecutor, SimulatedExecutorConfig, StrategyRuntime,
};
use ploy_strategy_bundles::strategies::directional::DirectionalConfig;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::BTreeMap;

/// Generate synthetic market data: 1 hour of 5-min windows for 3 symbols.
///
/// Each window: event discovered → spot ticks every 10s → quotes update → event expires.
/// Spot prices random-walk around a base with momentum bursts to trigger signals.
fn generate_synthetic_data(symbols: &[&str], duration_mins: u64) -> Vec<MarketUpdate> {
    let mut updates = Vec::new();
    let start = Utc::now() - Duration::minutes(duration_mins as i64);
    let window_secs = 300u64; // 5 min windows

    let base_prices: Vec<Decimal> = vec![dec!(100000), dec!(3500), dec!(140)]; // BTC, ETH, SOL

    for window_idx in 0..(duration_mins * 60 / window_secs) {
        let window_start = start + Duration::seconds((window_idx * window_secs) as i64);
        let window_end = window_start + Duration::seconds(window_secs as i64);

        for (sym_idx, &symbol) in symbols.iter().enumerate() {
            let base = base_prices[sym_idx % base_prices.len()];
            let event_id = format!("evt-{}-{}", symbol.to_lowercase(), window_idx);
            let up_token = format!("up-{}-{}", symbol.to_lowercase(), window_idx);
            let dn_token = format!("dn-{}-{}", symbol.to_lowercase(), window_idx);

            // Initial spot (open price)
            updates.push(MarketUpdate::SpotPrice {
                symbol: symbol.to_string(),
                price: base,
                ts: window_start,
            });

            // Event discovered
            updates.push(MarketUpdate::EventDiscovered {
                event_id: event_id.clone(),
                symbol: symbol.to_string(),
                up_token: up_token.clone(),
                down_token: dn_token.clone(),
                end_time: window_end,
                window_secs,
            });

            // Simulate spot price ticks every 10s
            // Alternate: odd windows drift up, even windows drift down
            let drift: Decimal = if window_idx % 3 == 0 {
                // Strong up move (1.5%) — should trigger UP entry
                base * dec!(0.015)
            } else if window_idx % 3 == 1 {
                // Strong down move (-1.2%) — should trigger DOWN entry
                -(base * dec!(0.012))
            } else {
                // Flat (0.1%) — should NOT trigger entry
                base * dec!(0.001)
            };

            // Quote updates — price reflects market's lagging assessment
            let up_ask = if drift > Decimal::ZERO {
                dec!(0.55) // market slightly favors up
            } else if drift < -(base * dec!(0.005)) {
                dec!(0.30) // market still thinks it might go up
            } else {
                dec!(0.50) // no-trade zone
            };

            updates.push(MarketUpdate::Quote {
                token_id: up_token.clone(),
                bid: Some(up_ask - dec!(0.01)),
                ask: Some(up_ask),
                ts: window_start + Duration::seconds(5),
            });
            updates.push(MarketUpdate::Quote {
                token_id: dn_token.clone(),
                bid: Some(dec!(1) - up_ask - dec!(0.01)),
                ask: Some(dec!(1) - up_ask),
                ts: window_start + Duration::seconds(5),
            });

            // Spot ticks showing the drift
            for tick in 1..=5 {
                let t = window_start + Duration::seconds(tick * 10);
                let pct = Decimal::from(tick) / dec!(5);
                let price = base + drift * pct;
                updates.push(MarketUpdate::SpotPrice {
                    symbol: symbol.to_string(),
                    price,
                    ts: t,
                });
            }

            // Updated quotes after price move
            let final_price = base + drift;
            let p_up_model = if drift > Decimal::ZERO { dec!(0.80) } else { dec!(0.20) };
            let up_ask_final = if drift.abs() > base * dec!(0.005) {
                // Market catches up partially
                p_up_model - dec!(0.10)
            } else {
                dec!(0.50) // no-trade zone
            };

            updates.push(MarketUpdate::Quote {
                token_id: up_token,
                bid: Some(up_ask_final - dec!(0.01)),
                ask: Some(up_ask_final),
                ts: window_start + Duration::seconds(55),
            });
            updates.push(MarketUpdate::Quote {
                token_id: dn_token,
                bid: Some(dec!(1) - up_ask_final - dec!(0.01)),
                ask: Some(dec!(1) - up_ask_final),
                ts: window_start + Duration::seconds(55),
            });

            // Spot at window midpoint (entry zone: 60-300s remaining)
            updates.push(MarketUpdate::SpotPrice {
                symbol: symbol.to_string(),
                price: final_price,
                ts: window_start + Duration::seconds(120), // 180s remaining
            });

            // Event expires
            updates.push(MarketUpdate::EventExpired {
                event_id,
            });
        }
    }

    // Sort by timestamp
    let base_ts = start;
    updates.sort_by_key(|u| match u {
        MarketUpdate::SpotPrice { ts, .. }
        | MarketUpdate::Quote { ts, .. }
        | MarketUpdate::L2 { ts, .. }
        | MarketUpdate::Kline { ts, .. } => *ts,
        MarketUpdate::EventDiscovered { end_time, .. } => *end_time - Duration::seconds(300),
        MarketUpdate::EventExpired { event_id } => {
            // Place expiry after all ticks for that window
            // Parse window index from event_id
            let idx: i64 = event_id.rsplit('-').next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            base_ts + Duration::seconds(idx * window_secs as i64 + window_secs as i64)
        }
    });

    updates
}

fn main() {
    // Parse config from CLI arg or use defaults
    let args: Vec<String> = std::env::args().collect();
    let (strategy_config, sim_config, runtime_config) = if args.len() > 1 {
        let config = FullConfig::from_file(&args[1]).expect("Failed to parse config");
        let sim = config.sim_executor_config();
        let rt = config.runtime_config();
        (config.strategy, sim, rt)
    } else {
        eprintln!("No config file provided, using built-in defaults\n");
        (
            DirectionalConfig {
                symbols: vec!["BTCUSDT".into(), "ETHUSDT".into(), "SOLUSDT".into()],
                vol_floor: 0.001,
                min_probability: 0.62,
                min_z_score: 0.35,
                min_entry_price: 0.15,
                max_entry_price: 0.85,
                no_trade_zone_min: 0.45,
                no_trade_zone_max: 0.55,
                min_edge: 0.05,
                min_time_remaining_secs: 60,
                max_time_remaining_secs: 300,
                cooldown_secs: 60,
                quantity: dec!(25),
                max_positions: 3,
                max_daily_trades: 1000,
            },
            SimulatedExecutorConfig {
                use_spread: true,
                spread_pct: dec!(0.02),
                enable_partial_fills: false,
                enable_market_impact: true,
                impact_coefficient: dec!(0.1),
                ..Default::default()
            },
            RuntimeConfig {
                mode: RuntimeMode::Backtest,
                throttle_hz: None,
                max_updates: None,
            },
        )
    };

    eprintln!("=== pm_5m_directional Backtest ===");
    eprintln!("Mode:    {:?}", runtime_config.mode);
    eprintln!("Symbols: {:?}", strategy_config.symbols);
    eprintln!("Params:  min_edge={:.0}% min_p={:.0}% cooldown={}s",
        strategy_config.min_edge * 100.0,
        strategy_config.min_probability * 100.0,
        strategy_config.cooldown_secs,
    );
    eprintln!();

    // Generate synthetic data (1 hour, 3 symbols)
    let data = generate_synthetic_data(&["BTCUSDT", "ETHUSDT", "SOLUSDT"], 60);
    eprintln!("Generated {} market updates (1 hour synthetic)\n", data.len());

    let strategy = DirectionalStrategy::new(strategy_config);
    let feed = HistoricalFeed::new(data);
    let executor = SimulatedExecutor::new(sim_config);
    let recorder = Box::new(NullRecorder);

    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);

    // Run synchronously via tokio
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let result = rt.block_on(runtime.run());

    // Print results
    let mark_prices = BTreeMap::new();
    let snapshot = runtime.trading().snapshot(&mark_prices);

    eprintln!("=== Results ===");
    eprintln!("Updates processed: {}", result.updates_processed);
    eprintln!("Intents submitted: {}", result.intents_submitted);
    eprintln!("Fills recorded:    {}", result.fills_recorded);
    eprintln!("Elapsed:           {:.2}s", result.elapsed_secs);
    eprintln!();

    if !snapshot.fills.is_empty() {
        eprintln!("=== Fills ===");
        for fill in &snapshot.fills {
            eprintln!(
                "  {} {:?} {}x @ {} (fee: {})",
                fill.token_id, fill.side, fill.quantity, fill.price, fill.fee
            );
        }
        eprintln!();
    }

    if !snapshot.positions.is_empty() {
        eprintln!("=== Positions ===");
        for pos in &snapshot.positions {
            eprintln!(
                "  {} qty={} avg_entry={} realized_pnl={}",
                pos.token_id, pos.net_qty, pos.avg_entry_price, pos.realized_pnl
            );
        }
        eprintln!();
    }

    eprintln!("=== P&L ===");
    eprintln!("Realized:   {}", result.pnl.realized_pnl);
    eprintln!("Fees:       {}", result.pnl.total_fees);
    eprintln!("Net:        {}", result.pnl.net_pnl());
    eprintln!();
    eprintln!("=== Risk ===");
    eprintln!("Open positions:  {}", result.risk.open_positions);
    eprintln!("Active orders:   {}", result.risk.active_orders);
    eprintln!("Gross exposure:  {}", result.risk.gross_exposure);
}
