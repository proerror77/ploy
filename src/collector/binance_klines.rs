//! Binance K-line (candlestick) REST API client
//!
//! Fetches historical K-line data for volatility analysis and pattern recognition.

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::error::{PloyError, Result};

const BINANCE_API_URL: &str = "https://api.binance.com";

/// A single K-line (candlestick)
#[derive(Debug, Clone)]
pub struct Kline {
    pub open_time: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    pub close_time: DateTime<Utc>,
    pub quote_volume: Decimal,
    pub trades: u64,
}

impl Kline {
    /// Calculate the range as a percentage of open price
    pub fn range_pct(&self) -> Decimal {
        if self.open.is_zero() {
            return Decimal::ZERO;
        }
        (self.high - self.low) / self.open
    }

    /// Calculate close-open difference as percentage
    pub fn close_open_pct(&self) -> Decimal {
        if self.open.is_zero() {
            return Decimal::ZERO;
        }
        (self.close - self.open) / self.open
    }

    /// Check if candle closed up (close >= open)
    pub fn is_up(&self) -> bool {
        self.close >= self.open
    }
}

/// Volatility statistics from K-line data
#[derive(Debug, Clone)]
pub struct VolatilityStats {
    /// Average range as percentage
    pub avg_range_pct: Decimal,
    /// Standard deviation of range
    pub range_std: Decimal,
    /// Average absolute close-open change
    pub avg_move_pct: Decimal,
    /// Percentage of candles that closed up
    pub up_ratio: Decimal,
    /// Number of candles analyzed
    pub sample_count: usize,
    /// Last update time
    pub updated_at: DateTime<Utc>,
}

/// Binance K-line API client with caching
pub struct BinanceKlineClient {
    client: reqwest::Client,
    /// Cached volatility stats per symbol
    stats_cache: Arc<RwLock<HashMap<String, VolatilityStats>>>,
    /// Cached raw K-lines per symbol
    klines_cache: Arc<RwLock<HashMap<String, Vec<Kline>>>>,
}

impl BinanceKlineClient {
    /// Create a new K-line client
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            stats_cache: Arc::new(RwLock::new(HashMap::new())),
            klines_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Fetch K-lines from Binance API
    /// interval: "1m", "5m", "15m", "1h", etc.
    /// limit: max 1000
    pub async fn fetch_klines(
        &self,
        symbol: &str,
        interval: &str,
        limit: usize,
    ) -> Result<Vec<Kline>> {
        let url = format!(
            "{}/api/v3/klines?symbol={}&interval={}&limit={}",
            BINANCE_API_URL,
            symbol,
            interval,
            limit.min(1000)
        );

        debug!("Fetching K-lines: {}", url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| PloyError::Internal(format!("K-line request failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(PloyError::Internal(format!(
                "K-line API error: {}",
                response.status()
            )));
        }

        let data: Vec<Vec<serde_json::Value>> = response
            .json()
            .await
            .map_err(|e| PloyError::Internal(format!("K-line parse error: {}", e)))?;

        let klines: Vec<Kline> = data
            .into_iter()
            .filter_map(|row| self.parse_kline_row(&row))
            .collect();

        debug!("Fetched {} K-lines for {}", klines.len(), symbol);
        Ok(klines)
    }

    /// Fetch klines for a given time range (inclusive start, exclusive-ish end).
    ///
    /// Binance returns at most 1000 rows per request, so this paginates forward by `close_time`.
    /// `interval` examples: "1m", "5m", "15m".
    pub async fn fetch_klines_range(
        &self,
        symbol: &str,
        interval: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Kline>> {
        let mut out: Vec<Kline> = Vec::new();
        let mut start_ms = start.timestamp_millis();
        let end_ms = end.timestamp_millis();

        if end_ms <= start_ms {
            return Ok(out);
        }

        // Safety: avoid infinite loops on unexpected API behavior.
        for _ in 0..20_000 {
            if start_ms >= end_ms {
                break;
            }

            let url = format!(
                "{}/api/v3/klines?symbol={}&interval={}&limit=1000&startTime={}&endTime={}",
                BINANCE_API_URL, symbol, interval, start_ms, end_ms
            );

            debug!("Fetching K-lines range: {}", url);

            let response = self
                .client
                .get(&url)
                .send()
                .await
                .map_err(|e| PloyError::Internal(format!("K-line request failed: {}", e)))?;

            if !response.status().is_success() {
                return Err(PloyError::Internal(format!(
                    "K-line API error: {}",
                    response.status()
                )));
            }

            let data: Vec<Vec<serde_json::Value>> = response
                .json()
                .await
                .map_err(|e| PloyError::Internal(format!("K-line parse error: {}", e)))?;

            if data.is_empty() {
                break;
            }

            let mut parsed_batch: Vec<Kline> = data
                .iter()
                .filter_map(|row| self.parse_kline_row(row))
                .collect();

            if parsed_batch.is_empty() {
                break;
            }

            // Advance paging cursor by the last candle close_time.
            let last_close_ms = parsed_batch
                .last()
                .map(|k| k.close_time.timestamp_millis())
                .unwrap_or(start_ms);
            let next_start_ms = last_close_ms.saturating_add(1);
            if next_start_ms <= start_ms {
                break;
            }
            start_ms = next_start_ms;

            out.append(&mut parsed_batch);

            // Light rate limit.
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        }

        out.sort_by_key(|k| k.open_time);
        out.dedup_by_key(|k| k.open_time);
        Ok(out)
    }

    /// Parse a single K-line row from Binance API response
    fn parse_kline_row(&self, row: &[serde_json::Value]) -> Option<Kline> {
        if row.len() < 11 {
            return None;
        }

        let open_time = DateTime::from_timestamp_millis(row[0].as_i64()?)?;
        let close_time = DateTime::from_timestamp_millis(row[6].as_i64()?)?;

        Some(Kline {
            open_time,
            open: row[1].as_str()?.parse().ok()?,
            high: row[2].as_str()?.parse().ok()?,
            low: row[3].as_str()?.parse().ok()?,
            close: row[4].as_str()?.parse().ok()?,
            volume: row[5].as_str()?.parse().ok()?,
            close_time,
            quote_volume: row[7].as_str()?.parse().ok()?,
            trades: row[8].as_u64()?,
        })
    }

    /// Calculate volatility statistics from K-lines
    pub fn calculate_stats(&self, klines: &[Kline]) -> Option<VolatilityStats> {
        if klines.is_empty() {
            return None;
        }

        let n = klines.len();
        let n_dec = Decimal::from(n);

        // Calculate averages
        let total_range: Decimal = klines.iter().map(|k| k.range_pct()).sum();
        let total_move: Decimal = klines.iter().map(|k| k.close_open_pct().abs()).sum();
        let up_count = klines.iter().filter(|k| k.is_up()).count();

        let avg_range_pct = total_range / n_dec;
        let avg_move_pct = total_move / n_dec;
        let up_ratio = Decimal::from(up_count) / n_dec;

        // Calculate standard deviation of range
        let range_variance: Decimal = klines
            .iter()
            .map(|k| {
                let diff = k.range_pct() - avg_range_pct;
                diff * diff
            })
            .sum::<Decimal>()
            / n_dec;

        // Approximate sqrt for Decimal
        let range_std = Decimal::try_from(range_variance.to_f64().unwrap_or(0.0).sqrt())
            .unwrap_or(Decimal::ZERO);

        Some(VolatilityStats {
            avg_range_pct,
            range_std,
            avg_move_pct,
            up_ratio,
            sample_count: n,
            updated_at: Utc::now(),
        })
    }

    /// Fetch and cache 15-minute K-line volatility stats for a symbol
    pub async fn update_volatility_stats(&self, symbol: &str) -> Result<VolatilityStats> {
        // Fetch last 100 15-minute candles (~25 hours of data)
        let klines = self.fetch_klines(symbol, "15m", 100).await?;

        let stats = self
            .calculate_stats(&klines)
            .ok_or_else(|| PloyError::Internal("Failed to calculate stats".to_string()))?;

        info!(
            "{} 15m volatility: avg_range={:.3}%, avg_move={:.3}%, up_ratio={:.1}%",
            symbol,
            stats.avg_range_pct * Decimal::from(100),
            stats.avg_move_pct * Decimal::from(100),
            stats.up_ratio * Decimal::from(100),
        );

        // Cache the results
        {
            let mut cache = self.stats_cache.write().await;
            cache.insert(symbol.to_string(), stats.clone());
        }
        {
            let mut cache = self.klines_cache.write().await;
            cache.insert(symbol.to_string(), klines);
        }

        Ok(stats)
    }

    /// Get cached volatility stats for a symbol
    pub async fn get_stats(&self, symbol: &str) -> Option<VolatilityStats> {
        let cache = self.stats_cache.read().await;
        cache.get(symbol).cloned()
    }

    /// Get the average 15-minute volatility for a symbol
    /// This is what we use for Z-score calculation
    pub async fn get_15m_volatility(&self, symbol: &str) -> Option<Decimal> {
        let stats = self.get_stats(symbol).await?;
        Some(stats.avg_range_pct)
    }

    /// Initialize volatility stats for multiple symbols
    pub async fn initialize_symbols(&self, symbols: &[String]) -> Result<()> {
        info!(
            "Initializing K-line volatility stats for {} symbols",
            symbols.len()
        );

        for symbol in symbols {
            match self.update_volatility_stats(symbol).await {
                Ok(_) => {}
                Err(e) => {
                    warn!("Failed to fetch K-lines for {}: {}", symbol, e);
                }
            }
            // Small delay to avoid rate limiting
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        Ok(())
    }
}

impl Default for BinanceKlineClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_kline_range_pct() {
        let kline = Kline {
            open_time: Utc::now(),
            open: dec!(100),
            high: dec!(105),
            low: dec!(98),
            close: dec!(103),
            volume: dec!(1000),
            close_time: Utc::now(),
            quote_volume: dec!(100000),
            trades: 500,
        };

        // Range = (105 - 98) / 100 = 7%
        assert_eq!(kline.range_pct(), dec!(0.07));

        // Close-open = (103 - 100) / 100 = 3%
        assert_eq!(kline.close_open_pct(), dec!(0.03));

        assert!(kline.is_up());
    }
}
