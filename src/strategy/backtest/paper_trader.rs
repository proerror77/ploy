use std::collections::HashMap;
use std::fs::File;
use std::io::Write as IoWrite;
use std::path::Path;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::calculate_implied_volatility;
use crate::strategy::volatility_arb::{VolatilityArbConfig, VolatilityArbEngine};

/// Paper trading logger - records signals without executing.
pub struct PaperTrader {
    config: VolatilityArbConfig,
    vol_engine: VolatilityArbEngine,
    signals: Vec<PaperSignal>,
    pending_signals: HashMap<String, PaperSignal>,
    log_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperSignal {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub market_id: String,
    pub condition_id: String,
    pub direction: String,
    pub entry_price: Decimal,
    pub fair_value: Decimal,
    pub price_edge: Decimal,
    pub vol_edge_pct: f64,
    pub confidence: f64,
    pub recommended_shares: u64,
    pub buffer_pct: Decimal,
    pub our_volatility: f64,
    pub implied_volatility: f64,
    pub time_remaining_secs: u64,
    pub resolution_time: Option<DateTime<Utc>>,
    pub actual_outcome: Option<bool>,
    pub would_have_won: Option<bool>,
    pub theoretical_pnl: Option<Decimal>,
}

impl PaperTrader {
    pub fn new(config: VolatilityArbConfig, log_file: Option<String>) -> Self {
        Self {
            vol_engine: VolatilityArbEngine::new(config.clone()),
            config,
            signals: Vec::new(),
            pending_signals: HashMap::new(),
            log_file,
        }
    }

    pub fn update_volatility(&mut self, symbol: &str, kline_vol: f64) {
        self.vol_engine.update_kline_volatility(symbol, kline_vol);
    }

    pub fn check_and_record(
        &mut self,
        symbol: &str,
        market_id: &str,
        condition_id: &str,
        spot_price: Decimal,
        threshold_price: Decimal,
        yes_price: Decimal,
        yes_ask: Decimal,
        time_remaining_secs: u64,
        tick_volatility: Option<f64>,
    ) -> Option<PaperSignal> {
        if self.pending_signals.contains_key(market_id) {
            return None;
        }

        let signal = self.vol_engine.analyze_market(
            symbol,
            market_id,
            condition_id,
            spot_price,
            threshold_price,
            yes_price,
            yes_ask,
            time_remaining_secs,
            tick_volatility,
        )?;

        let vol_estimate = self.vol_engine.estimate_volatility(symbol, tick_volatility);
        let implied_vol = calculate_implied_volatility(
            yes_price.to_f64().unwrap_or(0.5),
            signal.buffer_pct.to_f64().unwrap_or(0.0),
            time_remaining_secs as f64 / 900.0,
        )
        .unwrap_or(0.003);

        let paper_signal = PaperSignal {
            timestamp: Utc::now(),
            symbol: symbol.to_string(),
            market_id: market_id.to_string(),
            condition_id: condition_id.to_string(),
            direction: if signal.buy_yes {
                "YES".into()
            } else {
                "NO".into()
            },
            entry_price: signal.market_price,
            fair_value: signal.fair_value,
            price_edge: signal.price_edge,
            vol_edge_pct: signal.vol_edge_pct,
            confidence: signal.confidence,
            recommended_shares: signal.position_size,
            buffer_pct: signal.buffer_pct,
            our_volatility: vol_estimate.combined_vol,
            implied_volatility: implied_vol,
            time_remaining_secs,
            resolution_time: None,
            actual_outcome: None,
            would_have_won: None,
            theoretical_pnl: None,
        };

        self.log_signal(&paper_signal);
        self.pending_signals
            .insert(market_id.to_string(), paper_signal.clone());

        info!(
            symbol,
            direction = paper_signal.direction,
            entry_price = %paper_signal.entry_price,
            fair_value = %paper_signal.fair_value,
            price_edge = %paper_signal.price_edge,
            vol_edge_pct = paper_signal.vol_edge_pct,
            confidence = paper_signal.confidence,
            "📝 Paper signal recorded"
        );

        Some(paper_signal)
    }

    pub fn record_resolution(&mut self, market_id: &str, outcome: bool) {
        if let Some(mut signal) = self.pending_signals.remove(market_id) {
            signal.resolution_time = Some(Utc::now());
            signal.actual_outcome = Some(outcome);

            let would_have_won = if signal.direction == "YES" {
                outcome
            } else {
                !outcome
            };
            signal.would_have_won = Some(would_have_won);

            let entry_price = signal.entry_price;
            let shares = signal.recommended_shares;
            let exit_price = if would_have_won {
                Decimal::ONE
            } else {
                Decimal::ZERO
            };
            let cost = entry_price * Decimal::from(shares);
            let revenue = exit_price * Decimal::from(shares);
            let fees = cost * self.config.pm_fee_rate;
            let pnl = revenue - cost - fees;
            signal.theoretical_pnl = Some(pnl);

            info!(
                market_id,
                direction = signal.direction,
                outcome = if outcome { "YES" } else { "NO" },
                would_have_won,
                theoretical_pnl = %pnl,
                "📊 Paper trade resolved"
            );

            self.log_resolution(&signal);
            self.signals.push(signal);
        }
    }

    pub fn statistics(&self) -> PaperTradingStats {
        let resolved: Vec<_> = self
            .signals
            .iter()
            .filter(|signal| signal.would_have_won.is_some())
            .collect();

        if resolved.is_empty() {
            return PaperTradingStats::default();
        }

        let total = resolved.len() as u64;
        let wins = resolved
            .iter()
            .filter(|signal| signal.would_have_won == Some(true))
            .count() as u64;
        let total_pnl: Decimal = resolved
            .iter()
            .filter_map(|signal| signal.theoretical_pnl)
            .sum();

        let avg_vol_edge = resolved
            .iter()
            .map(|signal| signal.vol_edge_pct)
            .sum::<f64>()
            / resolved.len() as f64;
        let avg_confidence =
            resolved.iter().map(|signal| signal.confidence).sum::<f64>() / resolved.len() as f64;

        PaperTradingStats {
            total_signals: total,
            winning_signals: wins,
            win_rate: wins as f64 / total as f64,
            theoretical_pnl: total_pnl,
            avg_vol_edge,
            avg_confidence,
            pending_signals: self.pending_signals.len() as u64,
        }
    }

    pub fn signals(&self) -> &[PaperSignal] {
        &self.signals
    }

    pub fn pending(&self) -> &HashMap<String, PaperSignal> {
        &self.pending_signals
    }

    fn log_signal(&self, signal: &PaperSignal) {
        if let Some(ref path) = self.log_file {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let json = serde_json::to_string(signal).unwrap_or_default();
                let _ = writeln!(file, "{}", json);
            }
        }
    }

    fn log_resolution(&self, signal: &PaperSignal) {
        if let Some(ref path) = self.log_file {
            let resolution_path = path.replace(".json", "_resolved.json");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(resolution_path)
            {
                let json = serde_json::to_string(signal).unwrap_or_default();
                let _ = writeln!(file, "{}", json);
            }
        }
    }

    pub fn export_csv<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let mut file = File::create(path).map_err(|e| e.to_string())?;

        writeln!(file, "timestamp,symbol,market_id,direction,entry_price,fair_value,price_edge,vol_edge_pct,confidence,our_vol,implied_vol,buffer_pct,time_remaining,outcome,won,pnl")
            .map_err(|e| e.to_string())?;

        for signal in &self.signals {
            writeln!(
                file,
                "{},{},{},{},{},{},{},{:.4},{:.4},{:.6},{:.6},{},{},{},{},{}",
                signal.timestamp.format("%Y-%m-%d %H:%M:%S"),
                signal.symbol,
                signal.market_id,
                signal.direction,
                signal.entry_price,
                signal.fair_value,
                signal.price_edge,
                signal.vol_edge_pct,
                signal.confidence,
                signal.our_volatility,
                signal.implied_volatility,
                signal.buffer_pct,
                signal.time_remaining_secs,
                signal
                    .actual_outcome
                    .map_or("pending".to_string(), |outcome| if outcome {
                        "YES".to_string()
                    } else {
                        "NO".to_string()
                    }),
                signal
                    .would_have_won
                    .map_or("pending".to_string(), |won| if won {
                        "WIN".to_string()
                    } else {
                        "LOSS".to_string()
                    }),
                signal
                    .theoretical_pnl
                    .map_or("0".to_string(), |pnl| pnl.to_string()),
            )
            .map_err(|e| e.to_string())?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaperTradingStats {
    pub total_signals: u64,
    pub winning_signals: u64,
    pub win_rate: f64,
    pub theoretical_pnl: Decimal,
    pub avg_vol_edge: f64,
    pub avg_confidence: f64,
    pub pending_signals: u64,
}
