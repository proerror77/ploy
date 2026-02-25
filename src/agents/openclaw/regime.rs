//! Market regime detection from Binance volatility data
//!
//! Classifies the current market into one of four regimes:
//! - HighVol: short-term vol significantly exceeds long-term (spike)
//! - LowVol: short-term vol is well below long-term (quiet)
//! - Trending: strong directional consistency in recent price moves
//! - Ranging: neither trending nor vol-anomalous (mean-reverting)

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::adapters::binance_ws::BinanceWebSocket;

use super::config::RegimeConfig;

/// Market regime classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MarketRegime {
    /// Elevated volatility — favor vol-straddle or defensive strategies
    HighVol,
    /// Suppressed volatility — favor arb-only (tight spreads, low risk)
    LowVol,
    /// Strong directional move — favor momentum / directional
    Trending,
    /// Range-bound, mean-reverting — favor arb with moderate sizing
    Ranging,
}

impl std::fmt::Display for MarketRegime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarketRegime::HighVol => write!(f, "HighVol"),
            MarketRegime::LowVol => write!(f, "LowVol"),
            MarketRegime::Trending => write!(f, "Trending"),
            MarketRegime::Ranging => write!(f, "Ranging"),
        }
    }
}

/// Point-in-time regime reading with supporting data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegimeSnapshot {
    pub regime: MarketRegime,
    /// Confidence in the classification (0.0-1.0)
    pub confidence: f64,
    /// Short-term BTC volatility
    pub btc_vol_short: Option<Decimal>,
    /// Long-term BTC volatility
    pub btc_vol_long: Option<Decimal>,
    /// Vol ratio (short / long)
    pub vol_ratio: Option<f64>,
    /// Trend direction consistency (0.0 = no trend, 1.0 = perfect trend)
    pub trend_strength: Option<f64>,
    /// When this snapshot was computed
    pub computed_at: DateTime<Utc>,
}

/// Stateful regime detector — requires consecutive confirmations before transitioning
pub struct RegimeDetector {
    config: RegimeConfig,
    btc_symbol: String,
    binance_ws: Arc<BinanceWebSocket>,

    /// Current confirmed regime
    current_regime: MarketRegime,
    /// Candidate regime (what the raw signal says)
    candidate_regime: MarketRegime,
    /// How many consecutive ticks the candidate has been observed
    candidate_count: u32,
}

impl RegimeDetector {
    pub fn new(
        config: RegimeConfig,
        btc_symbol: String,
        binance_ws: Arc<BinanceWebSocket>,
    ) -> Self {
        Self {
            config,
            btc_symbol,
            binance_ws,
            current_regime: MarketRegime::Ranging,
            candidate_regime: MarketRegime::Ranging,
            candidate_count: 0,
        }
    }

    /// Current confirmed regime
    pub fn current(&self) -> MarketRegime {
        self.current_regime
    }

    /// Compute regime from latest market data. Returns (snapshot, changed).
    pub async fn tick(&mut self) -> (RegimeSnapshot, bool) {
        let cache = self.binance_ws.price_cache();
        let vol_short = cache.volatility(&self.btc_symbol, self.config.vol_short_secs).await;
        let vol_long = cache.volatility(&self.btc_symbol, self.config.vol_long_secs).await;
        let momentum_short = cache.momentum(&self.btc_symbol, self.config.trend_window_secs).await;

        let (raw_regime, confidence, vol_ratio, trend_strength) =
            self.classify(vol_short, vol_long, momentum_short);

        // Confirmation logic: require N consecutive same-regime readings
        let changed = if raw_regime == self.candidate_regime {
            self.candidate_count += 1;
            if self.candidate_count >= self.config.confirmation_count
                && self.candidate_regime != self.current_regime
            {
                let old = self.current_regime;
                self.current_regime = self.candidate_regime;
                debug!(
                    old = %old,
                    new = %self.current_regime,
                    confidence,
                    "regime transition confirmed"
                );
                true
            } else {
                false
            }
        } else {
            // Reset candidate counter
            self.candidate_regime = raw_regime;
            self.candidate_count = 1;
            false
        };

        let snapshot = RegimeSnapshot {
            regime: self.current_regime,
            confidence,
            btc_vol_short: vol_short,
            btc_vol_long: vol_long,
            vol_ratio,
            trend_strength,
            computed_at: Utc::now(),
        };

        (snapshot, changed)
    }

    /// Classify raw signals into regime + confidence
    fn classify(
        &self,
        vol_short: Option<Decimal>,
        vol_long: Option<Decimal>,
        momentum: Option<Decimal>,
    ) -> (MarketRegime, f64, Option<f64>, Option<f64>) {
        let vol_ratio = match (vol_short, vol_long) {
            (Some(s), Some(l)) if !l.is_zero() => {
                Some(s.to_string().parse::<f64>().unwrap_or(0.0)
                    / l.to_string().parse::<f64>().unwrap_or(1.0))
            }
            _ => None,
        };

        // Trend strength: absolute momentum normalized by volatility
        let trend_strength = match (momentum, vol_short) {
            (Some(m), Some(v)) if !v.is_zero() => {
                let m_f = m.to_string().parse::<f64>().unwrap_or(0.0).abs();
                let v_f = v.to_string().parse::<f64>().unwrap_or(1.0);
                Some((m_f / v_f).min(1.0))
            }
            _ => None,
        };

        // Classification priority: HighVol > Trending > LowVol > Ranging
        if let Some(ratio) = vol_ratio {
            if ratio > self.config.high_vol_ratio {
                let confidence = ((ratio - self.config.high_vol_ratio) / self.config.high_vol_ratio)
                    .min(1.0)
                    .max(0.5);
                return (MarketRegime::HighVol, confidence, vol_ratio, trend_strength);
            }

            if let Some(ts) = trend_strength {
                if ts > self.config.trend_threshold {
                    let confidence = ((ts - self.config.trend_threshold)
                        / (1.0 - self.config.trend_threshold))
                        .min(1.0)
                        .max(0.5);
                    return (MarketRegime::Trending, confidence, vol_ratio, trend_strength);
                }
            }

            if ratio < self.config.low_vol_ratio {
                let confidence = ((self.config.low_vol_ratio - ratio) / self.config.low_vol_ratio)
                    .min(1.0)
                    .max(0.5);
                return (MarketRegime::LowVol, confidence, vol_ratio, trend_strength);
            }
        }

        // Default: Ranging
        (MarketRegime::Ranging, 0.5, vol_ratio, trend_strength)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regime_display() {
        assert_eq!(MarketRegime::HighVol.to_string(), "HighVol");
        assert_eq!(MarketRegime::Trending.to_string(), "Trending");
    }

    #[test]
    fn regime_equality() {
        assert_eq!(MarketRegime::LowVol, MarketRegime::LowVol);
        assert_ne!(MarketRegime::HighVol, MarketRegime::LowVol);
    }
}
