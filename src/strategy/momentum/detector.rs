use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::{debug, info};

use crate::adapters::SpotPrice;

use super::{Direction, MomentumConfig};

/// A detected momentum opportunity
#[derive(Debug, Clone)]
pub struct MomentumSignal {
    pub symbol: String,
    pub direction: Direction,
    pub cex_move_pct: Decimal,
    pub pm_price: Decimal,
    pub edge: Decimal,
    pub confidence: f64,
    pub timestamp: DateTime<Utc>,
}

impl MomentumSignal {
    /// Check if signal is valid for trading
    pub fn is_valid(&self, config: &MomentumConfig) -> bool {
        self.cex_move_pct.abs() >= config.min_move_pct
            && self.pm_price <= config.max_entry_price
            && self.edge >= config.min_edge
    }
}

/// Detects momentum opportunities by comparing CEX prices to Polymarket odds.
pub struct MomentumDetector {
    config: MomentumConfig,
    /// Cached K-line volatility per symbol
    kline_volatility: HashMap<String, Decimal>,
}

impl MomentumDetector {
    pub fn new(config: MomentumConfig) -> Self {
        Self {
            config,
            kline_volatility: HashMap::new(),
        }
    }

    /// Update K-line volatility for a symbol
    pub fn update_kline_volatility(&mut self, symbol: &str, volatility: Decimal) {
        self.kline_volatility.insert(symbol.to_string(), volatility);
    }

    /// Check for momentum signal given CEX and PM prices.
    /// Uses weighted momentum (10s/30s/60s) and volatility-adjusted thresholds.
    pub fn check(
        &self,
        symbol: &str,
        spot: &SpotPrice,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) -> Option<MomentumSignal> {
        let momentum = match spot.weighted_momentum() {
            Some(m) => m,
            None => {
                debug!(
                    "{} insufficient history for weighted momentum, using single timeframe",
                    symbol
                );
                spot.momentum(self.config.lookback_secs)?
            }
        };

        let effective_threshold = self.calculate_effective_threshold(symbol, spot);

        debug!(
            "{} weighted_momentum={:.4}% threshold={:.4}% (vol_adjusted={})",
            symbol,
            momentum * dec!(100),
            effective_threshold * dec!(100),
            self.config.use_volatility_adjustment
        );

        if momentum.abs() < effective_threshold {
            return None;
        }

        let (direction, pm_price) = if momentum > Decimal::ZERO {
            (Direction::Up, up_ask?)
        } else {
            (Direction::Down, down_ask?)
        };

        if self.config.require_vwap_confirmation {
            let vwap = match spot.vwap(self.config.vwap_lookback_secs) {
                Some(v) => v,
                None => {
                    debug!(
                        "{} {} insufficient data for VWAP confirmation (lookback={}s)",
                        symbol, direction, self.config.vwap_lookback_secs
                    );
                    return None;
                }
            };

            let dev = self.config.min_vwap_deviation.max(dec!(0));
            let ok = match direction {
                Direction::Up => spot.price >= vwap * (Decimal::ONE + dev),
                Direction::Down => spot.price <= vwap * (Decimal::ONE - dev),
            };

            if !ok {
                debug!(
                    "{} {} VWAP confirmation failed: spot=${:.4} vwap=${:.4} dev={:.3}%",
                    symbol,
                    direction,
                    spot.price,
                    vwap,
                    dev * dec!(100)
                );
                return None;
            }
        }

        if pm_price > self.config.max_entry_price {
            debug!(
                "{} {} PM price {:.2}¢ > max {:.2}¢, skipping",
                symbol,
                direction,
                pm_price * dec!(100),
                self.config.max_entry_price * dec!(100)
            );
            return None;
        }

        let fair_value = self.estimate_fair_value(momentum);
        let edge = fair_value - pm_price;

        if edge < self.config.min_edge {
            debug!(
                "{} {} edge {:.2}% < min {:.2}%, skipping",
                symbol,
                direction,
                edge * dec!(100),
                self.config.min_edge * dec!(100)
            );
            return None;
        }

        let confidence = self.calculate_confidence(momentum, edge);

        info!(
            "🎯 SIGNAL: {} {} | momentum={:.3}% threshold={:.3}% | PM={:.1}¢ edge={:.1}%",
            symbol,
            direction,
            momentum * dec!(100),
            effective_threshold * dec!(100),
            pm_price * dec!(100),
            edge * dec!(100)
        );

        Some(MomentumSignal {
            symbol: symbol.to_string(),
            direction,
            cex_move_pct: momentum,
            pm_price,
            edge,
            confidence,
            timestamp: Utc::now(),
        })
    }

    /// Enhanced momentum check with all optimizations.
    pub fn check_enhanced(
        &self,
        symbol: &str,
        spot: &SpotPrice,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
        obi: Option<Decimal>,
        time_remaining_secs: i64,
        price_to_beat: Option<Decimal>,
    ) -> Option<MomentumSignal> {
        let (momentum, mtf_agrees) = self.check_multi_timeframe(spot);

        if self.config.require_mtf_agreement && !mtf_agrees {
            debug!("{} MTF disagreement: timeframes not aligned", symbol);
            return None;
        }

        let effective_threshold = if self.config.use_kline_volatility {
            self.calculate_kline_threshold(symbol, spot)
        } else {
            self.calculate_effective_threshold(symbol, spot)
        };

        if momentum.abs() < effective_threshold {
            return None;
        }

        let direction = if momentum > Decimal::ZERO {
            Direction::Up
        } else {
            Direction::Down
        };

        if self.config.min_obi_confirmation > Decimal::ZERO {
            if let Some(obi_val) = obi {
                let obi_confirms = match direction {
                    Direction::Up => obi_val >= self.config.min_obi_confirmation,
                    Direction::Down => obi_val <= -self.config.min_obi_confirmation,
                };

                if !obi_confirms {
                    debug!(
                        "{} OBI {:.2} does not confirm {} direction",
                        symbol, obi_val, direction
                    );
                    return None;
                }
            }
        }

        let pm_price = match direction {
            Direction::Up => up_ask?,
            Direction::Down => down_ask?,
        };

        if pm_price > self.config.max_entry_price {
            return None;
        }

        let fair_value = if self.config.use_price_to_beat {
            self.estimate_fair_value_with_price_to_beat(
                momentum,
                spot.price,
                price_to_beat,
                time_remaining_secs,
            )
        } else {
            self.estimate_fair_value(momentum)
        };

        let time_adjusted_fair_value = if self.config.time_decay_factor > Decimal::ZERO {
            let time_factor = Decimal::from(time_remaining_secs.max(0)) / dec!(900);
            let decay = dec!(1) - (self.config.time_decay_factor * (dec!(1) - time_factor));
            let base = dec!(0.5);
            base + (fair_value - base) * decay
        } else {
            fair_value
        };

        let edge = time_adjusted_fair_value - pm_price;

        if edge < self.config.min_edge {
            debug!(
                "{} {} edge {:.2}% < min {:.2}%",
                symbol,
                direction,
                edge * dec!(100),
                self.config.min_edge * dec!(100)
            );
            return None;
        }

        let confidence = self.calculate_enhanced_confidence(
            momentum,
            edge,
            obi,
            mtf_agrees,
            time_remaining_secs,
        );

        if confidence < self.config.min_confidence {
            debug!(
                "{} {} confidence {:.0}% < min {:.0}%",
                symbol,
                direction,
                confidence * 100.0,
                self.config.min_confidence * 100.0
            );
            return None;
        }

        info!(
            "🎯 ENHANCED SIGNAL: {} {} | mom={:.3}% thr={:.3}% | PM={:.1}¢ FV={:.1}¢ edge={:.1}% | conf={:.0}% | {}s left{}",
            symbol,
            direction,
            momentum * dec!(100),
            effective_threshold * dec!(100),
            pm_price * dec!(100),
            time_adjusted_fair_value * dec!(100),
            edge * dec!(100),
            confidence * 100.0,
            time_remaining_secs,
            if mtf_agrees { " [MTF✓]" } else { "" }
        );

        Some(MomentumSignal {
            symbol: symbol.to_string(),
            direction,
            cex_move_pct: momentum,
            pm_price,
            edge,
            confidence,
            timestamp: Utc::now(),
        })
    }

    /// Calculate effective threshold based on current volatility.
    fn calculate_effective_threshold(&self, symbol: &str, spot: &SpotPrice) -> Decimal {
        if !self.config.use_volatility_adjustment {
            return self.config.min_move_pct;
        }

        let baseline_vol = self
            .config
            .baseline_volatility
            .get(symbol)
            .copied()
            .unwrap_or(dec!(0.001));

        let current_vol = spot
            .volatility(self.config.volatility_lookback_secs)
            .unwrap_or(baseline_vol);

        if baseline_vol.is_zero() {
            return self.config.min_move_pct;
        }

        let vol_ratio = current_vol / baseline_vol;
        let clamped_ratio = vol_ratio.max(dec!(0.5)).min(dec!(2.0));
        let adjusted = self.config.min_move_pct * clamped_ratio;

        debug!(
            "{} vol_adjust: current={:.4}% baseline={:.4}% ratio={:.2} threshold={:.4}%",
            symbol,
            current_vol * dec!(100),
            baseline_vol * dec!(100),
            clamped_ratio,
            adjusted * dec!(100)
        );

        adjusted
    }

    /// Estimate fair value based on CEX momentum.
    fn estimate_fair_value(&self, momentum: Decimal) -> Decimal {
        let base_prob = dec!(0.50);
        let abs_momentum = momentum.abs();
        let momentum_factor = if abs_momentum < dec!(0.001) {
            abs_momentum * dec!(50)
        } else if abs_momentum < dec!(0.005) {
            dec!(0.05) + (abs_momentum - dec!(0.001)) * dec!(40)
        } else {
            dec!(0.21) + (abs_momentum - dec!(0.005)) * dec!(30)
        };

        (base_prob + momentum_factor).min(dec!(0.90))
    }

    /// Calculate confidence score (0.0 to 1.0).
    fn calculate_confidence(&self, momentum: Decimal, edge: Decimal) -> f64 {
        let momentum_score = (momentum.abs() / dec!(0.005)).min(Decimal::ONE);
        let edge_score = (edge / dec!(0.15)).min(Decimal::ONE);
        let score = momentum_score * dec!(0.4) + edge_score * dec!(0.6);

        score
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.5)
            .clamp(0.0, 1.0)
    }

    /// Check multi-timeframe momentum agreement.
    fn check_multi_timeframe(&self, spot: &SpotPrice) -> (Decimal, bool) {
        let mom_10s = spot.momentum(10);
        let mom_30s = spot.momentum(30);
        let mom_60s = spot.momentum(60);

        let weighted = match (mom_10s, mom_30s, mom_60s) {
            (Some(m10), Some(m30), Some(m60)) => {
                m10 * dec!(0.2) + m30 * dec!(0.3) + m60 * dec!(0.5)
            }
            (Some(m10), Some(m30), None) => m10 * dec!(0.4) + m30 * dec!(0.6),
            (Some(m), _, _) | (_, Some(m), _) | (_, _, Some(m)) => m,
            _ => return (Decimal::ZERO, false),
        };

        let all_agree = match (mom_10s, mom_30s, mom_60s) {
            (Some(m10), Some(m30), Some(m60)) => {
                (m10 > Decimal::ZERO && m30 > Decimal::ZERO && m60 > Decimal::ZERO)
                    || (m10 < Decimal::ZERO && m30 < Decimal::ZERO && m60 < Decimal::ZERO)
            }
            (Some(m10), Some(m30), None) => {
                (m10 > Decimal::ZERO && m30 > Decimal::ZERO)
                    || (m10 < Decimal::ZERO && m30 < Decimal::ZERO)
            }
            _ => false,
        };

        (weighted, all_agree)
    }

    /// Calculate threshold using K-line historical volatility.
    fn calculate_kline_threshold(&self, symbol: &str, spot: &SpotPrice) -> Decimal {
        let kline_vol = self.kline_volatility.get(symbol).copied();

        let current_vol = if let Some(vol) = kline_vol {
            vol
        } else {
            spot.volatility(self.config.volatility_lookback_secs)
                .unwrap_or(dec!(0.001))
        };

        let baseline_vol = self
            .config
            .baseline_volatility
            .get(symbol)
            .copied()
            .unwrap_or(dec!(0.001));

        if baseline_vol.is_zero() {
            return self.config.min_move_pct;
        }

        let vol_ratio = (current_vol / baseline_vol).max(dec!(0.5)).min(dec!(2.0));
        self.config.min_move_pct * vol_ratio
    }

    /// Estimate fair value considering price-to-beat.
    fn estimate_fair_value_with_price_to_beat(
        &self,
        momentum: Decimal,
        current_price: Decimal,
        price_to_beat: Option<Decimal>,
        time_remaining_secs: i64,
    ) -> Decimal {
        let base_fv = self.estimate_fair_value(momentum);

        let price_threshold = match price_to_beat {
            Some(p) => p,
            None => return base_fv,
        };

        let distance_pct = if price_threshold > Decimal::ZERO {
            (current_price - price_threshold) / price_threshold
        } else {
            return base_fv;
        };

        let time_factor = dec!(1) - (Decimal::from(time_remaining_secs.max(0)) / dec!(900));
        let direction_matches = (momentum > Decimal::ZERO && distance_pct > Decimal::ZERO)
            || (momentum < Decimal::ZERO && distance_pct < Decimal::ZERO);

        if direction_matches {
            let boost = distance_pct.abs() * time_factor * dec!(0.5);
            (base_fv + boost).min(dec!(0.95))
        } else {
            let reduction = distance_pct.abs() * dec!(0.3);
            (base_fv - reduction).max(dec!(0.35))
        }
    }

    /// Enhanced confidence calculation.
    fn calculate_enhanced_confidence(
        &self,
        momentum: Decimal,
        edge: Decimal,
        obi: Option<Decimal>,
        mtf_agrees: bool,
        time_remaining_secs: i64,
    ) -> f64 {
        let mut score: f64 = 0.0;

        let mom_score = (momentum.abs() / dec!(0.005)).min(Decimal::ONE);
        score += mom_score.to_string().parse::<f64>().unwrap_or(0.0) * 0.25;

        let edge_score = (edge / dec!(0.15)).min(Decimal::ONE);
        score += edge_score.to_string().parse::<f64>().unwrap_or(0.0) * 0.25;

        if let Some(obi_val) = obi {
            let obi_strength = (obi_val.abs() / dec!(0.2)).min(Decimal::ONE);
            score += obi_strength.to_string().parse::<f64>().unwrap_or(0.0) * 0.15;
        }

        if mtf_agrees {
            score += 0.15;
        }

        let time_factor = 1.0 - (time_remaining_secs.max(0) as f64 / 900.0);
        score += time_factor * 0.20;

        score.clamp(0.0, 1.0)
    }
}
