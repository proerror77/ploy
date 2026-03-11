use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

mod replay_tables;
mod token_settlements;

pub(crate) use replay_tables::backfill_pm_replay_tables;
pub(crate) use token_settlements::backfill_pm_token_settlements;

fn parse_optional_bound(value: Option<&str>, flag: &str) -> Result<Option<DateTime<Utc>>> {
    let parsed = value
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        })
        .transpose()
        .with_context(|| format!("Invalid {} date (use ISO 8601 format)", flag))?
        .map(|dt| dt.with_timezone(&Utc));

    Ok(parsed)
}

fn validate_window(from_dt: &Option<DateTime<Utc>>, to_dt: &Option<DateTime<Utc>>) -> Result<()> {
    if let (Some(from_dt), Some(to_dt)) = (from_dt.as_ref(), to_dt.as_ref()) {
        if to_dt <= from_dt {
            anyhow::bail!("--to must be after --from");
        }
    }

    Ok(())
}

fn parse_symbol_filter(symbols: &str) -> (Vec<String>, Option<Vec<String>>) {
    let symbol_list: Vec<String> = symbols
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let symbols_param = if symbol_list.is_empty() {
        None
    } else {
        Some(symbol_list.clone())
    };

    (symbol_list, symbols_param)
}

fn resolve_database_url(database_url: Option<String>) -> String {
    database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_symbol_filter, validate_window};
    use chrono::{TimeZone, Utc};

    #[test]
    fn parse_symbol_filter_drops_blank_entries() {
        let (symbols, symbols_param) = parse_symbol_filter(" BTCUSDT, ,ETHUSDT ,, SOLUSDT ");

        assert_eq!(symbols, vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
        assert_eq!(
            symbols_param,
            Some(vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(),
                "SOLUSDT".to_string(),
            ])
        );
    }

    #[test]
    fn parse_symbol_filter_returns_none_for_empty_input() {
        let (symbols, symbols_param) = parse_symbol_filter(" , , ");

        assert!(symbols.is_empty());
        assert_eq!(symbols_param, None);
    }

    #[test]
    fn validate_window_rejects_non_increasing_range() {
        let from_dt = Some(Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap());
        let to_dt = Some(Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap());

        let err = validate_window(&from_dt, &to_dt).expect_err("expected invalid range");
        assert!(err.to_string().contains("--to must be after --from"));
    }
}
