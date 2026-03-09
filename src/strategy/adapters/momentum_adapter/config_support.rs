use super::*;
use rust_decimal_macros::dec;
use toml::Value;
use tracing::info;

impl MomentumStrategyAdapter {
    /// Create from TOML configuration
    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow::anyhow!("Invalid TOML: {}", e))?;

        let empty_table = Value::Table(Default::default());
        let entry = config.get("entry").unwrap_or(&empty_table);
        let exit = config.get("exit").unwrap_or(&empty_table);
        let timing = config.get("timing").unwrap_or(&empty_table);
        let risk = config.get("risk").unwrap_or(&empty_table);
        let strategy = config.get("strategy").unwrap_or(&empty_table);

        let momentum_config = build_momentum_config(entry, timing, risk, strategy);
        let directional_entry_threshold = parse_directional_entry_threshold(entry);
        validate_exit_keys(exit)?;
        let exit_config = build_exit_config(exit);

        info!(
            "MomentumAdapter config: directional_mode={} shares={} max_pos={} min_t={}s max_t={}s vol_floor={} dir_entry_th={:.1}%",
            momentum_config.directional_mode,
            momentum_config.shares_per_trade,
            momentum_config.max_positions,
            momentum_config.min_time_remaining_secs,
            momentum_config.max_time_remaining_secs,
            momentum_config.directional_vol_floor,
            directional_entry_threshold * 100.0,
        );

        let mut adapter = Self::new(id, momentum_config, exit_config, dry_run);
        adapter.fixed_amount_usd = risk.get("fixed_amount_usd").and_then(|v| v.as_float());
        adapter.directional_entry_threshold = directional_entry_threshold;
        Ok(adapter)
    }
}

fn build_momentum_config(
    entry: &Value,
    timing: &Value,
    risk: &Value,
    strategy: &Value,
) -> MomentumConfig {
    let mode = strategy
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("predictive");

    let hold_to_resolution = mode == "confirmatory";

    let symbols: Vec<String> = entry
        .get("symbols")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| {
            vec![
                "BTCUSDT".into(),
                "ETHUSDT".into(),
                "SOLUSDT".into(),
                "XRPUSDT".into(),
            ]
        });

    let mut baseline_volatility = std::collections::HashMap::new();
    baseline_volatility.insert("BTCUSDT".into(), dec!(0.0005));
    baseline_volatility.insert("ETHUSDT".into(), dec!(0.0008));
    baseline_volatility.insert("SOLUSDT".into(), dec!(0.0015));
    baseline_volatility.insert("XRPUSDT".into(), dec!(0.0012));

    MomentumConfig {
        min_move_pct: Decimal::try_from(
            entry
                .get("min_move")
                .and_then(|v| v.as_float())
                .unwrap_or(0.05)
                / 100.0,
        )
        .unwrap_or(dec!(0.0005)),
        max_entry_price: Decimal::try_from(
            entry
                .get("max_entry")
                .and_then(|v| v.as_float())
                .unwrap_or(45.0)
                / 100.0,
        )
        .unwrap_or(dec!(0.45)),
        min_edge: Decimal::try_from(
            entry
                .get("min_edge")
                .and_then(|v| v.as_float())
                .unwrap_or(5.0)
                / 100.0,
        )
        .unwrap_or(dec!(0.05)),
        lookback_secs: 5,
        use_volatility_adjustment: entry
            .get("use_volatility_adjustment")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        baseline_volatility,
        volatility_lookback_secs: entry
            .get("volatility_lookback")
            .and_then(|v| v.as_integer())
            .unwrap_or(60) as u64,
        shares_per_trade: risk
            .get("shares")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .or_else(|| {
                entry
                    .get("shares_per_trade")
                    .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            })
            .unwrap_or(100.0) as u64,
        max_positions: risk
            .get("max_positions")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .or_else(|| {
                entry
                    .get("max_positions")
                    .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            })
            .unwrap_or(5.0) as usize,
        cooldown_secs: timing
            .get("cooldown_secs")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .or_else(|| {
                entry
                    .get("cooldown_secs")
                    .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            })
            .unwrap_or(60.0) as u64,
        max_daily_trades: risk
            .get("max_daily_trades")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .or_else(|| {
                entry
                    .get("max_daily_trades")
                    .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            })
            .unwrap_or(50.0) as u32,
        symbols,
        hold_to_resolution,
        min_time_remaining_secs: timing
            .get("min_time_remaining")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .or_else(|| {
                entry
                    .get("min_time_remaining")
                    .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            })
            .unwrap_or(300.0) as u64,
        max_time_remaining_secs: timing
            .get("max_time_remaining")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .or_else(|| {
                entry
                    .get("max_time_remaining")
                    .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            })
            .unwrap_or(900.0) as u64,
        max_window_exposure_usd: Decimal::try_from(
            risk.get("max_window_exposure")
                .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
                .or_else(|| {
                    entry
                        .get("max_window_exposure")
                        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
                })
                .unwrap_or(25.0),
        )
        .unwrap_or(dec!(25)),
        best_edge_only: entry
            .get("best_edge_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        signal_collection_delay_ms: entry
            .get("signal_delay_ms")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(2000.0) as u64,
        require_mtf_agreement: entry
            .get("require_mtf_agreement")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        min_obi_confirmation: Decimal::try_from(
            entry
                .get("min_obi_confirmation")
                .and_then(|v| v.as_float())
                .unwrap_or(5.0)
                / 100.0,
        )
        .unwrap_or(dec!(0.05)),
        use_kline_volatility: entry
            .get("use_kline_volatility")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        time_decay_factor: Decimal::try_from(
            entry
                .get("time_decay_factor")
                .and_then(|v| v.as_float())
                .unwrap_or(30.0)
                / 100.0,
        )
        .unwrap_or(dec!(0.30)),
        use_price_to_beat: entry
            .get("use_price_to_beat")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        dynamic_position_sizing: risk
            .get("dynamic_position_sizing")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        min_confidence: entry
            .get("min_confidence")
            .and_then(|v| v.as_float())
            .unwrap_or(0.5),
        use_kelly_sizing: risk
            .get("use_kelly_sizing")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        kelly_fraction_cap: Decimal::try_from(
            risk.get("kelly_fraction_cap")
                .and_then(|v| v.as_float())
                .unwrap_or(0.25),
        )
        .unwrap_or(dec!(0.25)),
        require_vwap_confirmation: entry
            .get("require_vwap_confirmation")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        vwap_lookback_secs: entry
            .get("vwap_lookback_secs")
            .and_then(|v| v.as_integer())
            .unwrap_or(60) as u64,
        min_vwap_deviation: Decimal::try_from(
            entry
                .get("min_vwap_deviation")
                .and_then(|v| v.as_float())
                .unwrap_or(0.0)
                / 100.0,
        )
        .unwrap_or(dec!(0)),
        directional_mode: entry
            .get("directional_mode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        directional_vol_floor: entry
            .get("directional_vol_floor")
            .and_then(|v| v.as_float())
            .unwrap_or(0.005),
    }
}

fn parse_directional_entry_threshold(entry: &Value) -> f64 {
    entry
        .get("directional_entry_threshold")
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .map(|v| if v > 1.0 { v / 100.0 } else { v })
        .unwrap_or(0.08)
        .clamp(0.0, 1.0)
}

fn validate_exit_keys(exit: &Value) -> Result<()> {
    if exit.get("take_profit").is_some() {
        return Err(crate::error::PloyError::Validation(
            "deprecated key `exit.take_profit` is no longer supported; use `exit.exit_edge_floor_pct`"
                .to_string(),
        ));
    }
    if exit.get("stop_loss").is_some() {
        return Err(crate::error::PloyError::Validation(
            "deprecated key `exit.stop_loss` is no longer supported; use `exit.exit_price_band_pct`"
                .to_string(),
        ));
    }
    Ok(())
}

fn build_exit_config(exit: &Value) -> ExitConfig {
    ExitConfig {
        take_profit_pct: Decimal::try_from(
            exit.get("exit_edge_floor_pct")
                .and_then(|v| v.as_float())
                .unwrap_or(20.0)
                / 100.0,
        )
        .unwrap_or(dec!(0.20)),
        stop_loss_pct: Decimal::try_from(
            exit.get("exit_price_band_pct")
                .and_then(|v| v.as_float())
                .unwrap_or(12.0)
                / 100.0,
        )
        .unwrap_or(dec!(0.12)),
        trailing_stop_pct: Decimal::try_from(
            exit.get("trailing_stop")
                .and_then(|v| v.as_float())
                .unwrap_or(10.0)
                / 100.0,
        )
        .unwrap_or(dec!(0.10)),
        exit_before_resolution_secs: exit
            .get("exit_before_resolution")
            .and_then(|v| v.as_integer())
            .unwrap_or(30) as u64,
    }
}
