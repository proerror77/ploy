//! Runnable backtest example for PM5D strategy variants.
//!
//! Usage (synthetic):
//!   cargo run -p ploy-strategy-bundles --example run_backtest -- [config.toml]
//!
//! Usage (database):
//!   cargo run -p ploy-strategy-bundles --example run_backtest -- \
//!     --config config/strategies/02-pm5d.unified.toml \
//!     --db-url postgresql://... \
//!     --start-date 2026-03-28 \
//!     --end-date 2026-04-03
//!
//! If no --db-url is given, uses synthetic market data.

use chrono::{Duration, NaiveDate, TimeZone, Utc};
use ploy_feed_loaders::{load_from_database_with_options, HistoricalLoadOptions};
use ploy_strategy_bundles::strategies::directional::DirectionalConfig;
use ploy_strategy_bundles::{
    config::FullConfig, DirectionalStrategy, HistoricalFeed, MarketUpdate, NullRecorder,
    ReversalStrategy, RuntimeConfig, RuntimeMode, SimulatedExecutor, SimulatedExecutorConfig,
    StrategyLogic, StrategyRuntime, ThreeLayerStrategy,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::postgres::PgPoolOptions;
use std::collections::BTreeMap;
use std::sync::Arc;

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
            let sym: Arc<str> = Arc::from(symbol);
            let event_id: Arc<str> =
                Arc::from(format!("evt-{}-{}", symbol.to_lowercase(), window_idx));
            let up_token: Arc<str> =
                Arc::from(format!("up-{}-{}", symbol.to_lowercase(), window_idx));
            let dn_token: Arc<str> =
                Arc::from(format!("dn-{}-{}", symbol.to_lowercase(), window_idx));

            // Initial spot (open price)
            updates.push(MarketUpdate::SpotPrice {
                symbol: Arc::clone(&sym),
                price: base,
                ts: window_start,
            });

            // Event discovered
            updates.push(MarketUpdate::EventDiscovered {
                event_id: Arc::clone(&event_id),
                symbol: Arc::clone(&sym),
                up_token: Arc::clone(&up_token),
                down_token: Arc::clone(&dn_token),
                end_time: window_end,
                window_secs,
                price_to_beat: None,
                resolved_up_won: None,
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
                token_id: Arc::clone(&up_token),
                bid: Some(up_ask - dec!(0.01)),
                ask: Some(up_ask),
                ts: window_start + Duration::seconds(5),
                bid_size: None,
                ask_size: None,
            });
            updates.push(MarketUpdate::Quote {
                token_id: Arc::clone(&dn_token),
                bid: Some(dec!(1) - up_ask - dec!(0.01)),
                ask: Some(dec!(1) - up_ask),
                ts: window_start + Duration::seconds(5),
                bid_size: None,
                ask_size: None,
            });

            // Spot ticks showing the drift
            for tick in 1..=5 {
                let t = window_start + Duration::seconds(tick * 10);
                let pct = Decimal::from(tick) / dec!(5);
                let price = base + drift * pct;
                updates.push(MarketUpdate::SpotPrice {
                    symbol: Arc::clone(&sym),
                    price,
                    ts: t,
                });
            }

            // Updated quotes after price move
            let final_price = base + drift;
            let p_up_model = if drift > Decimal::ZERO {
                dec!(0.80)
            } else {
                dec!(0.20)
            };
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
                bid_size: None,
                ask_size: None,
            });
            updates.push(MarketUpdate::Quote {
                token_id: dn_token,
                bid: Some(dec!(1) - up_ask_final - dec!(0.01)),
                ask: Some(dec!(1) - up_ask_final),
                ts: window_start + Duration::seconds(55),
                bid_size: None,
                ask_size: None,
            });

            // Spot at window midpoint (entry zone: 60-300s remaining)
            updates.push(MarketUpdate::SpotPrice {
                symbol: sym,
                price: final_price,
                ts: window_start + Duration::seconds(120), // 180s remaining
            });

            // Event expires
            updates.push(MarketUpdate::EventExpired {
                event_id,
                end_time: window_end,
                resolved_up_won: None, // synthetic data — use spot fallback
            });
        }
    }

    // Sort by timestamp
    updates.sort_by_key(MarketUpdate::sort_ts);

    updates
}

/// Parse a named flag value from args: `--flag value`
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn main() {
    // Parse CLI flags
    let args: Vec<String> = std::env::args().collect();
    let config_path = flag_value(&args, "--config")
        .or_else(|| args.get(1).filter(|a| !a.starts_with('-')).cloned());
    let db_url = flag_value(&args, "--db-url");
    let data_dir = flag_value(&args, "--data-dir");
    let start_date = flag_value(&args, "--start-date");
    let end_date = flag_value(&args, "--end-date");

    let (strategy_variant, strategy_config, sim_config, runtime_config, backtest_options) =
        if let Some(ref path) = config_path {
            let config = FullConfig::from_file(path).expect("Failed to parse config");
            let sim = config.sim_executor_config();
            let rt = config.runtime_config();
            let strategy_variant = config.runtime.canonical_strategy_variant();
            let backtest_options = HistoricalLoadOptions {
                include_reference_prices: config.backtest_data.include_reference_prices,
                reference_symbols: config
                    .backtest_data
                    .reference_symbols(&config.reference_data),
                include_sports_state: config.backtest_data.include_sports_state,
                require_official_settlement: config.backtest_data.require_official_settlement,
                lob_sample_secs: 30,
            };
            (strategy_variant, config.strategy, sim, rt, backtest_options)
        } else {
            eprintln!("No config file provided, using built-in defaults\n");
            (
                "directional".to_string(),
                DirectionalConfig {
                    symbols: vec!["BTCUSDT".into(), "ETHUSDT".into(), "SOLUSDT".into()],
                    symbol_profiles: std::collections::HashMap::new(),
                    vol_floor: 0.001,
                    min_probability: 0.62,
                    min_z_score: 0.35,
                    min_entry_price: 0.15,
                    max_entry_price: 0.85,
                    no_trade_zone_min: 0.45,
                    no_trade_zone_max: 0.55,
                    min_edge: 0.05,
                    min_deviation_pct: 0.005,
                    min_reversal_consistency: 0.55,
                    min_trend_consistency: 0.50,
                    min_trend_persistence_secs: 0,
                    take_profit_price_delta: 0.10,
                    stop_loss_price_delta: 0.05,
                    max_hold_secs: 120,
                    reversal_bonus_cap: 0.20,
                    use_multiscale_volatility: true,
                    use_price_structure_adjustment: true,
                    reversal_max_distance_pct: 0.015,
                    reversal_max_drift_flip_age_secs: 20,
                    reversal_min_post_flip_drift: 0.0001,
                    reversal_lob_depth_pct: 0.001,
                    reversal_min_lob_depth_ratio: 1.3,
                    reversal_max_ask_for_reversal: 0.25,
                    reversal_max_pm_lag_secs: 30,
                    reversal_take_profit_ask: 0.65,
                    reversal_stop_distance_pct: 0.025,
                    min_time_remaining_secs: 60,
                    max_time_remaining_secs: 300,
                    cooldown_secs: 60,
                    stake_usd: dec!(25),
                    max_positions: 3,
                    max_daily_trades: 1000,
                    max_daily_loss_usd: None,
                    allowed_window_secs: vec![300, 900],
                    three_layer_min_direction_prob: 0.56,
                    three_layer_min_distance_over_sigma: 0.3,
                    three_layer_min_confirmation_score: 0.10,
                    three_layer_min_drift_confirmation: 0.0002,
                    three_layer_min_edge: 0.03,
                    three_layer_min_reward_risk: 1.2,
                    three_layer_take_profit_ask: 0.70,
                    three_layer_stop_distance_pct: 0.020,
                    three_layer_max_pm_lag_secs: 15,
                    three_layer_min_entry_score: 0.30,
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
                    skip_settlement_exits: false,
                },
                HistoricalLoadOptions::default(),
            )
        };

    eprintln!("=== PM5D Backtest ===");
    eprintln!("Mode:    {:?}", runtime_config.mode);
    eprintln!("Variant: {strategy_variant}");
    eprintln!("Symbols: {:?}", strategy_config.symbols);
    eprintln!(
        "Params:  min_edge={:.0}% min_p={:.0}% cooldown={}s",
        strategy_config.min_edge * 100.0,
        strategy_config.min_probability * 100.0,
        strategy_config.cooldown_secs,
    );
    eprintln!();

    // Build tokio runtime for async DB loading
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let stake_usd = strategy_config.stake_usd;
    let strategy: Box<dyn StrategyLogic> = match strategy_variant.as_str() {
        "directional" => Box::new(DirectionalStrategy::new(strategy_config.clone())),
        "reversal" => Box::new(ReversalStrategy::new(strategy_config.clone().into())),
        "three_layer" => Box::new(ThreeLayerStrategy::new(strategy_config.clone().into())),
        other => panic!("unsupported strategy_variant in run_backtest example: {other}"),
    };
    let executor = SimulatedExecutor::new(sim_config);
    let recorder = Box::new(NullRecorder);

    // When --data-dir is set, use StreamingParquetFeed for O(1) memory usage.
    // Otherwise fall back to Vec-backed HistoricalFeed (DB or synthetic).
    if let Some(ref dir) = data_dir {
        #[cfg(not(feature = "parquet-feed"))]
        {
            let _ = dir;
            eprintln!("--data-dir requires the `parquet-feed` feature");
            std::process::exit(1);
        }

        #[cfg(feature = "parquet-feed")]
        {
            use ploy_strategy_bundles::feed::parquet_stream::StreamingParquetFeed;

            let from = start_date.as_deref().unwrap_or("2026-03-28");
            let to = end_date.as_deref().unwrap_or("2026-04-03");
            let from_dt = Utc.from_utc_datetime(
                &NaiveDate::parse_from_str(from, "%Y-%m-%d")
                    .expect("Invalid --start-date (use YYYY-MM-DD)")
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            );
            let to_dt = Utc.from_utc_datetime(
                &NaiveDate::parse_from_str(to, "%Y-%m-%d")
                    .expect("Invalid --end-date (use YYYY-MM-DD)")
                    .and_hms_opt(23, 59, 59)
                    .unwrap(),
            );
            eprintln!("Streaming Parquet data from: {dir} ({from} → {to})");
            let feed = StreamingParquetFeed::new(
                dir,
                &strategy_config.symbols,
                from_dt,
                to_dt,
                &backtest_options,
            );
            let mut runtime =
                StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
            let result = rt.block_on(runtime.run());
            let mark_prices = BTreeMap::new();
            let snapshot = runtime.trading().snapshot(&mark_prices);
            print_results(result, snapshot, stake_usd);
        }
    } else {
        let data: Vec<MarketUpdate> = if let Some(ref url) = db_url {
            let from = start_date.as_deref().unwrap_or("2026-03-28");
            let to = end_date.as_deref().unwrap_or("2026-04-03");
            let from_dt = Utc.from_utc_datetime(
                &NaiveDate::parse_from_str(from, "%Y-%m-%d")
                    .expect("Invalid --start-date (use YYYY-MM-DD)")
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            );
            let to_dt = Utc.from_utc_datetime(
                &NaiveDate::parse_from_str(to, "%Y-%m-%d")
                    .expect("Invalid --end-date (use YYYY-MM-DD)")
                    .and_hms_opt(23, 59, 59)
                    .unwrap(),
            );
            eprintln!("Loading DB data: {} → {}", from, to);
            let pool = rt
                .block_on(PgPoolOptions::new().max_connections(5).connect(url))
                .expect("DB connection failed");
            let symbols: Vec<String> = strategy_config.symbols.clone();
            let updates = rt
                .block_on(load_from_database_with_options(
                    &pool,
                    &symbols,
                    from_dt,
                    to_dt,
                    &backtest_options,
                ))
                .expect("Failed to load from database");
            eprintln!("Loaded {} market updates from DB\n", updates.len());
            print_data_breakdown(&updates);
            updates
        } else {
            let updates = generate_synthetic_data(&["BTCUSDT", "ETHUSDT", "SOLUSDT"], 60);
            eprintln!(
                "Generated {} market updates (1 hour synthetic)\n",
                updates.len()
            );
            updates
        };

        let feed = HistoricalFeed::new(data);
        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
        let result = rt.block_on(runtime.run());
        let mark_prices = BTreeMap::new();
        let snapshot = runtime.trading().snapshot(&mark_prices);
        print_results(result, snapshot, stake_usd);
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn print_data_breakdown(updates: &[MarketUpdate]) {
    let mut spot_count = 0u64;
    let mut quote_count = 0u64;
    let mut event_discovered = 0u64;
    let mut event_expired = 0u64;
    let mut l2_count = 0u64;
    let mut kline_count = 0u64;
    for u in updates {
        match u {
            MarketUpdate::SpotPrice { .. } => spot_count += 1,
            MarketUpdate::AggTrade { .. } => {}
            MarketUpdate::Quote { .. } => quote_count += 1,
            MarketUpdate::EventDiscovered { .. } => event_discovered += 1,
            MarketUpdate::EventExpired { .. } => event_expired += 1,
            MarketUpdate::L2 { .. } | MarketUpdate::L2Depth { .. } => l2_count += 1,
            MarketUpdate::SportsState { .. }
            | MarketUpdate::SportsPregame { .. }
            | MarketUpdate::SportsLive { .. }
            | MarketUpdate::ReferencePrice { .. } => {}
            MarketUpdate::Kline { .. } => kline_count += 1,
        }
    }
    eprintln!(
        "Data breakdown: spot={spot_count} quote={quote_count} discovered={event_discovered} expired={event_expired} l2={l2_count} kline={kline_count}"
    );
}

fn print_results(
    result: ploy_strategy_bundles::RuntimeResult,
    snapshot: ploy_trading::TradingRuntimeSnapshot,
    stake_usd: Decimal,
) {
    let cashflow = snapshot.fill_cashflow_summary();

    eprintln!("=== Results ===");
    eprintln!("Updates processed: {}", result.updates_processed);
    let trade_count = result.fills_recorded / 2;
    eprintln!(
        "Trades:            {} ({} fills)",
        trade_count, result.fills_recorded
    );
    eprintln!("Elapsed:           {:.2}s", result.elapsed_secs);
    eprintln!();

    let fills = &snapshot.fills;
    let mut open_positions: i64 = 0;
    let mut peak_positions: i64 = 0;
    for fill in fills {
        match fill.side {
            ploy_trading::TradeSide::Buy => {
                open_positions += 1;
                if open_positions > peak_positions {
                    peak_positions = open_positions;
                }
            }
            ploy_trading::TradeSide::Sell => {
                open_positions = (open_positions - 1).max(0);
            }
        }
    }
    let peak_capital = rust_decimal::Decimal::from(peak_positions) * stake_usd;

    if !snapshot.fills.is_empty() && std::env::args().any(|a| a == "--show-fills") {
        eprintln!("=== Fills ===");
        for fill in fills {
            eprintln!(
                "  {} {:?} {}x @ {} (fee: {})",
                &fill.token_id[..12],
                fill.side,
                fill.quantity,
                fill.price,
                fill.fee
            );
        }
        eprintln!();
    }

    if !snapshot.positions.is_empty() {
        eprintln!("=== Open Positions ===");
        for pos in &snapshot.positions {
            eprintln!(
                "  {} qty={} avg_entry={} realized_pnl={}",
                &pos.token_id[..12],
                pos.net_qty,
                pos.avg_entry_price,
                pos.realized_pnl
            );
        }
        eprintln!();
    }

    eprintln!("=== P&L ===");
    eprintln!("Realized:          {}", result.pnl.realized_pnl);
    eprintln!("Fees:              {}", result.pnl.total_fees);
    eprintln!("Net:               {}", result.pnl.net_pnl());
    eprintln!();
    eprintln!("=== Capital Usage ===");
    eprintln!("Stake per trade:   ${}", stake_usd);
    eprintln!("Total trades:      {}", trade_count);
    eprintln!(
        "Cumulative cost:   {} (all trades summed)",
        cashflow.deployed_capital()
    );
    eprintln!(
        "Peak concurrent:   {} (max simultaneous open)",
        peak_capital.round_dp(2)
    );
    eprintln!("Sell proceeds:     {}", cashflow.gross_sell_proceeds);
    if let Some(roi) = cashflow.roi_on_deployed_capital() {
        eprintln!(
            "ROI (cumulative):  {}%  ← total profit / total cost (misleading for multi-trade)",
            (roi * Decimal::from(100)).round_dp(2)
        );
    }
    if peak_capital > Decimal::ZERO {
        let roi_peak = result.pnl.net_pnl() / peak_capital * Decimal::from(100);
        eprintln!(
            "ROI (peak capital): {}%  ← net profit / max simultaneous capital",
            roi_peak.round_dp(2)
        );
    }
    eprintln!("Note: quantity is shares/contracts; capital = shares × price");
    eprintln!();
    eprintln!("=== Risk ===");
    eprintln!("Open positions:  {}", result.risk.open_positions);
    eprintln!("Active orders:   {}", result.risk.active_orders);
    eprintln!("Gross exposure:  {}", result.risk.gross_exposure);
}
