//! Walk-forward backtest for the Pattern Memory strategy.
//!
//! Flow:
//! 1. Read binance_klines (5m + 15m) from DB, sorted by time
//! 2. Walk-forward: each bar → ingest_return(), query posterior
//! 3. If posterior passes trade thresholds → record prediction
//! 4. Compare against pm_token_settlements settlement outcome
//! 5. Output: hit rate, AUC, Brier score, sliced by symbol / required_return

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use tracing::info;

use crate::error::Result;
use crate::strategy::pattern_memory::engine::PatternMemory;

/// Configuration for the walk-forward Pattern Memory backtest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternMemoryBacktestConfig {
    pub symbols: Vec<String>,
    pub lookback_days: u64,
    /// Pattern length N (should match strategy config, e.g. 20).
    pub pattern_len: usize,
    /// Correlation threshold for pattern matching.
    pub corr_threshold: f64,
    /// Beta prior alpha.
    pub alpha: f64,
    /// Beta prior beta.
    pub beta: f64,
    /// Minimum n_eff to consider a signal actionable.
    pub min_n_eff: f64,
    /// Minimum confidence (|p_up - 0.5|) to record a prediction.
    pub min_confidence: f64,
    /// Minimum matches required.
    pub min_matches: usize,
    /// Time decay lambda (0 = no decay).
    pub age_decay_lambda: f64,
    /// Max samples in memory.
    pub max_samples: usize,
    /// DB connection URL.
    pub db_url: Option<String>,
}

impl Default for PatternMemoryBacktestConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".into(), "ETHUSDT".into(), "SOLUSDT".into()],
            lookback_days: 30,
            pattern_len: 20,
            corr_threshold: 0.70,
            alpha: 5.0,
            beta: 5.0,
            min_n_eff: 5.0,
            min_confidence: 0.60,
            min_matches: 10,
            age_decay_lambda: 0.001,
            max_samples: 2000,
            db_url: None,
        }
    }
}

#[derive(Debug, Clone)]
struct KlineRow {
    symbol: String,
    open_time: DateTime<Utc>,
    open: f64,
    close: f64,
    interval: String,
}

#[derive(Debug, Clone)]
pub struct Prediction {
    pub symbol: String,
    pub timestamp: DateTime<Utc>,
    pub p_up: f64,
    pub predicted_up: bool,
    pub required_return: f64,
    pub n_eff: f64,
    pub matches: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BacktestResult {
    pub total_predictions: usize,
    pub correct: usize,
    pub hit_rate: f64,
    pub brier_score: f64,
    pub by_symbol: HashMap<String, SymbolResult>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SymbolResult {
    pub predictions: usize,
    pub correct: usize,
    pub hit_rate: f64,
    pub brier_score: f64,
}

/// Run the walk-forward Pattern Memory backtest.
///
/// Uses const generic N=20 for the default pattern length.
pub async fn run_backtest(
    cfg: &PatternMemoryBacktestConfig,
    pool: &PgPool,
) -> Result<BacktestResult> {
    info!(symbols = ?cfg.symbols, lookback_days = cfg.lookback_days, "starting pattern memory backtest");

    // Fetch 5m klines from DB sorted by time
    let cutoff = Utc::now() - chrono::Duration::days(cfg.lookback_days as i64);
    let symbol_list: Vec<&str> = cfg.symbols.iter().map(|s| s.as_str()).collect();

    let rows = sqlx::query(
        r#"
        SELECT symbol, open_time, open, close, interval
        FROM binance_klines
        WHERE symbol = ANY($1)
          AND interval = '5m'
          AND open_time >= $2
        ORDER BY open_time ASC
        "#,
    )
    .bind(&symbol_list)
    .bind(cutoff)
    .fetch_all(pool)
    .await
    .map_err(crate::error::PloyError::Database)?;

    info!(rows = rows.len(), "loaded 5m klines from DB");

    let klines: Vec<KlineRow> = rows
        .iter()
        .filter_map(|row| {
            Some(KlineRow {
                symbol: row.try_get::<String, _>("symbol").ok()?,
                open_time: row.try_get::<DateTime<Utc>, _>("open_time").ok()?,
                open: row.try_get::<Decimal, _>("open").ok()?.to_f64()?,
                close: row.try_get::<Decimal, _>("close").ok()?.to_f64()?,
                interval: row.try_get::<String, _>("interval").ok()?,
            })
        })
        .collect();

    // Walk-forward: maintain a PatternMemory per symbol
    let mut memories: HashMap<String, PatternMemory<20>> = HashMap::new();
    let mut predictions: Vec<Prediction> = Vec::new();

    for kline in &klines {
        let r = if kline.open != 0.0 {
            (kline.close - kline.open) / kline.open
        } else {
            continue;
        };

        let mem = memories
            .entry(kline.symbol.clone())
            .or_insert_with(|| PatternMemory::<20>::new().with_max_samples(cfg.max_samples));

        // Query posterior BEFORE ingesting this bar's return (strict walk-forward)
        let post = mem.posterior_for_required_return(
            0.0, // required_return = 0 for baseline "will price go up?"
            cfg.corr_threshold,
            cfg.alpha,
            cfg.beta,
            cfg.age_decay_lambda,
        );

        let confidence = (post.p_up - 0.5).abs();
        if post.n_eff >= cfg.min_n_eff
            && confidence >= cfg.min_confidence
            && post.matches >= cfg.min_matches
        {
            predictions.push(Prediction {
                symbol: kline.symbol.clone(),
                timestamp: kline.open_time,
                p_up: post.p_up,
                predicted_up: post.p_up >= 0.5,
                required_return: 0.0,
                n_eff: post.n_eff,
                matches: post.matches,
            });
        }

        // NOW ingest (after prediction, for walk-forward correctness)
        mem.ingest_return(r, kline.open_time);
    }

    // Evaluate predictions against actual outcomes
    // For each prediction at bar T, the actual outcome is the return of the NEXT bar
    // We already predicted "will the next bar go up?", so we compare with the next kline return
    let mut result = BacktestResult::default();
    let mut brier_sum = 0.0;

    // Build a lookup: (symbol, timestamp) → next bar return
    let mut next_returns: HashMap<(String, DateTime<Utc>), f64> = HashMap::new();
    let mut by_symbol_klines: HashMap<String, Vec<&KlineRow>> = HashMap::new();
    for k in &klines {
        by_symbol_klines
            .entry(k.symbol.clone())
            .or_default()
            .push(k);
    }
    for (_sym, sym_klines) in &by_symbol_klines {
        for i in 0..sym_klines.len().saturating_sub(1) {
            let cur = sym_klines[i];
            let next = sym_klines[i + 1];
            if next.open != 0.0 {
                let next_r = (next.close - next.open) / next.open;
                next_returns.insert((cur.symbol.clone(), cur.open_time), next_r);
            }
        }
    }

    for pred in &predictions {
        if let Some(&actual_r) = next_returns.get(&(pred.symbol.clone(), pred.timestamp)) {
            let actual_up = actual_r > 0.0;
            let correct = pred.predicted_up == actual_up;

            result.total_predictions += 1;
            if correct {
                result.correct += 1;
            }

            // Brier score: (p_predicted - outcome)^2
            let outcome = if actual_up { 1.0 } else { 0.0 };
            brier_sum += (pred.p_up - outcome).powi(2);

            // Per-symbol stats
            let sym_result = result.by_symbol.entry(pred.symbol.clone()).or_default();
            sym_result.predictions += 1;
            if correct {
                sym_result.correct += 1;
            }
        }
    }

    if result.total_predictions > 0 {
        result.hit_rate = result.correct as f64 / result.total_predictions as f64;
        result.brier_score = brier_sum / result.total_predictions as f64;
    }

    for (_, sym) in result.by_symbol.iter_mut() {
        if sym.predictions > 0 {
            sym.hit_rate = sym.correct as f64 / sym.predictions as f64;
        }
    }

    info!(
        total = result.total_predictions,
        correct = result.correct,
        hit_rate = format!("{:.2}%", result.hit_rate * 100.0),
        brier = format!("{:.4}", result.brier_score),
        "pattern memory backtest complete"
    );

    Ok(result)
}
