use super::*;
use rust_decimal_macros::dec;
use toml::Value;

impl SplitArbStrategyAdapter {
    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow::anyhow!("Invalid TOML: {}", e))?;

        let empty_table = Value::Table(Default::default());
        let entry = config.get("entry").unwrap_or(&empty_table);
        let risk = config.get("risk").unwrap_or(&empty_table);
        let position = config.get("position").unwrap_or(&empty_table);
        let markets = config.get("markets").unwrap_or(&empty_table);

        validate_deprecated_keys(entry, position)?;

        let split_config = build_split_config(entry, risk);
        let mut adapter = Self::new(id, split_config, dry_run);
        adapter.series_ids = normalize_series_ids(markets);
        adapter.fixed_amount_usd = risk.get("fixed_amount_usd").and_then(|v| v.as_float());
        Ok(adapter)
    }
}

fn validate_deprecated_keys(entry: &Value, position: &Value) -> Result<()> {
    if entry.get("max_combined_price").is_some() {
        return Err(crate::error::PloyError::Validation(
            "deprecated key `entry.max_combined_price` is no longer supported; use `entry.target_sum`"
                .to_string(),
        ));
    }
    if entry.get("min_spread").is_some() {
        return Err(crate::error::PloyError::Validation(
            "deprecated key `entry.min_spread` is no longer supported; use `entry.min_profit`"
                .to_string(),
        ));
    }
    if position.get("shares_per_side").is_some() {
        return Err(crate::error::PloyError::Validation(
            "deprecated key `position.shares_per_side` is no longer supported; use `risk.shares`"
                .to_string(),
        ));
    }
    if position.get("max_positions").is_some() {
        return Err(crate::error::PloyError::Validation(
            "deprecated key `position.max_positions` is no longer supported; use `risk.max_unhedged`"
                .to_string(),
        ));
    }
    Ok(())
}

fn build_split_config(entry: &Value, risk: &Value) -> CoreSplitArbConfig {
    let target_sum = read_percentish(entry, "target_sum").unwrap_or(0.98);
    let max_entry = read_percentish(entry, "max_entry").unwrap_or(target_sum / 2.0);
    let min_profit = read_percentish(entry, "min_profit").unwrap_or(0.02);
    let shares = risk
        .get("shares")
        .and_then(|v| v.as_integer())
        .unwrap_or(50) as u64;

    CoreSplitArbConfig {
        max_entry_price: Decimal::try_from(max_entry).unwrap_or(dec!(0.49)),
        target_total_cost: Decimal::try_from(target_sum).unwrap_or(dec!(0.98)),
        min_profit_margin: Decimal::try_from(min_profit).unwrap_or(dec!(0.02)),
        max_hedge_wait_secs: risk
            .get("max_hedge_wait")
            .and_then(|v| v.as_integer())
            .unwrap_or(30) as u64,
        shares_per_trade: shares,
        max_unhedged_positions: risk
            .get("max_unhedged")
            .and_then(|v| v.as_integer())
            .unwrap_or(3) as usize,
        unhedged_stop_loss: Decimal::try_from(
            risk.get("unhedged_stop")
                .and_then(|v| v.as_float())
                .unwrap_or(10.0)
                / 100.0,
        )
        .unwrap_or(dec!(0.10)),
        fee_rate: Decimal::try_from(
            risk.get("fee_rate")
                .and_then(|v| v.as_float())
                .unwrap_or(0.02),
        )
        .unwrap_or(dec!(0.02)),
    }
}

fn read_percentish(table: &Value, key: &str) -> Option<f64> {
    table
        .get(key)
        .and_then(|v| v.as_float())
        .map(|v| if v > 1.0 { v / 100.0 } else { v })
}

fn normalize_series_ids(markets: &Value) -> Vec<String> {
    let mut series_ids: Vec<String> = markets
        .get("series_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    if series_ids.is_empty() {
        return default_split_arb_series_ids();
    }

    series_ids.sort();
    series_ids.dedup();
    series_ids
}
