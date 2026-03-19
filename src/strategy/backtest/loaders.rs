use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromStr;
use rust_decimal_macros::dec;
use tracing::{info, warn};

use super::{KlineRecord, PMPriceRecord};

/// Load K-line data from CSV file.
/// Expected format: timestamp,symbol,open,high,low,close,volume
pub fn load_klines_from_csv<P: AsRef<Path>>(path: P) -> Result<Vec<KlineRecord>, String> {
    let file = File::open(path).map_err(|error| format!("Failed to open file: {}", error))?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for (index, line) in reader.lines().enumerate() {
        if index == 0 {
            continue;
        }

        let line = line.map_err(|error| format!("Failed to read line {}: {}", index, error))?;
        let parts: Vec<&str> = line.split(',').collect();

        if parts.len() < 7 {
            warn!("Skipping malformed line {}: insufficient columns", index);
            continue;
        }

        let timestamp = parse_timestamp(parts[0])
            .ok_or_else(|| format!("Invalid timestamp at line {}", index))?;

        records.push(KlineRecord {
            timestamp,
            symbol: parts[1].to_string(),
            open: Decimal::from_str(parts[2]).unwrap_or(Decimal::ZERO),
            high: Decimal::from_str(parts[3]).unwrap_or(Decimal::ZERO),
            low: Decimal::from_str(parts[4]).unwrap_or(Decimal::ZERO),
            close: Decimal::from_str(parts[5]).unwrap_or(Decimal::ZERO),
            volume: Decimal::from_str(parts[6]).unwrap_or(Decimal::ZERO),
        });
    }

    info!("Loaded {} K-line records", records.len());
    Ok(records)
}

/// Load PM price data from CSV file.
/// Expected format: timestamp,market_id,condition_id,symbol,threshold,yes_price,no_price,yes_bid,yes_ask,resolution_time,outcome
pub fn load_pm_prices_from_csv<P: AsRef<Path>>(path: P) -> Result<Vec<PMPriceRecord>, String> {
    let file = File::open(path).map_err(|error| format!("Failed to open file: {}", error))?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for (index, line) in reader.lines().enumerate() {
        if index == 0 {
            continue;
        }

        let line = line.map_err(|error| format!("Failed to read line {}: {}", index, error))?;
        let parts: Vec<&str> = line.split(',').collect();

        if parts.len() < 11 {
            warn!("Skipping malformed line {}: insufficient columns", index);
            continue;
        }

        let timestamp = parse_timestamp(parts[0])
            .ok_or_else(|| format!("Invalid timestamp at line {}", index))?;
        let resolution_time = parse_timestamp(parts[9])
            .ok_or_else(|| format!("Invalid resolution_time at line {}", index))?;

        let outcome = match parts[10].trim().to_lowercase().as_str() {
            "yes" | "true" | "1" => Some(true),
            "no" | "false" | "0" => Some(false),
            _ => None,
        };

        records.push(PMPriceRecord {
            timestamp,
            market_id: parts[1].to_string(),
            condition_id: parts[2].to_string(),
            symbol: parts[3].to_string(),
            threshold_price: Decimal::from_str(parts[4]).unwrap_or(Decimal::ZERO),
            yes_price: Decimal::from_str(parts[5]).unwrap_or(dec!(0.5)),
            no_price: Decimal::from_str(parts[6]).unwrap_or(dec!(0.5)),
            yes_bid: Decimal::from_str(parts[7]).unwrap_or(dec!(0.5)),
            yes_ask: Decimal::from_str(parts[8]).unwrap_or(dec!(0.5)),
            resolution_time,
            outcome,
        });
    }

    info!("Loaded {} PM price records", records.len());
    Ok(records)
}

fn parse_timestamp(input: &str) -> Option<DateTime<Utc>> {
    if let Ok(timestamp) = input.parse::<i64>() {
        if timestamp > 1_000_000_000_000 {
            return Utc.timestamp_millis_opt(timestamp).single();
        }
        return Utc.timestamp_opt(timestamp, 0).single();
    }

    if let Ok(timestamp) = DateTime::parse_from_rfc3339(input) {
        return Some(timestamp.with_timezone(&Utc));
    }

    let formats = [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
    ];

    for format in &formats {
        if let Ok(timestamp) = NaiveDateTime::parse_from_str(input, format) {
            return Some(Utc.from_utc_datetime(&timestamp));
        }
    }

    None
}
