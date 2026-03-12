//! Backtest signal recorder — persists entry/exit/filtered signals and closed trades.
//!
//! Two implementations:
//! - `PgBacktestRecorder` — batched INSERT to `backtest_signals` + `backtest_trades`
//! - `NullRecorder` — no-op for unit tests and quick iteration without DB

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ─────────────────────────────────────────────────────────────
// Signal data
// ─────────────────────────────────────────────────────────────

/// Unified signal data for entry/exit/filtered events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestSignal {
    pub signal_type: SignalType,
    pub symbol: String,
    pub direction: String,
    pub timestamp: DateTime<Utc>,
    pub p_hat: Option<f64>,
    pub ev_net: Option<f64>,
    pub sigma: Option<f64>,
    pub market_price: Option<Decimal>,
    pub spot_price: Option<Decimal>,
    pub s0: Option<Decimal>,
    pub time_remaining_secs: Option<f64>,
    pub filter_reason: Option<String>,
    pub exit_reason: Option<String>,
    pub exit_price: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignalType {
    Entry,
    Exit,
    Filtered,
}

impl SignalType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SignalType::Entry => "entry",
            SignalType::Exit => "exit",
            SignalType::Filtered => "filtered",
        }
    }
}

/// A closed trade pending DB write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTrade {
    pub symbol: String,
    pub direction: String,
    pub entry_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub shares: i32,
    pub pnl: Decimal,
    pub won: bool,
    pub holding_secs: i64,
    pub exit_reason: String,
    pub entry_p_hat: Option<f64>,
    pub entry_ev_net: Option<f64>,
    pub entry_sigma: Option<f64>,
    pub s0: Option<Decimal>,
}

// ─────────────────────────────────────────────────────────────
// Trait
// ─────────────────────────────────────────────────────────────

/// Decouples signal persistence from the backtest engine.
///
/// All methods are sync because the engine's `run()` loop is sync.
/// `PgBacktestRecorder` buffers in memory; call `flush_async()` / `finalize()`
/// from async context after the engine finishes.
pub trait BacktestRecorder: Send {
    fn record_entry(&mut self, signal: &BacktestSignal);
    fn record_exit(&mut self, signal: &BacktestSignal);
    fn record_filtered(&mut self, signal: &BacktestSignal, reason: &str);
    fn record_trade(&mut self, trade: &PendingTrade);
    fn flush(&mut self) -> anyhow::Result<()>;
    /// Downcast support — enables recovering the concrete type after `take_recorder()`.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

// ─────────────────────────────────────────────────────────────
// NullRecorder — no-op for tests
// ─────────────────────────────────────────────────────────────

pub struct NullRecorder;

impl BacktestRecorder for NullRecorder {
    fn record_entry(&mut self, _signal: &BacktestSignal) {}
    fn record_exit(&mut self, _signal: &BacktestSignal) {}
    fn record_filtered(&mut self, _signal: &BacktestSignal, _reason: &str) {}
    fn record_trade(&mut self, _trade: &PendingTrade) {}
    fn flush(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// ─────────────────────────────────────────────────────────────
// PgBacktestRecorder — batched DB writes
// ─────────────────────────────────────────────────────────────

const BATCH_THRESHOLD: usize = 500;

pub struct PgBacktestRecorder {
    pool: sqlx::PgPool,
    run_id: Uuid,
    signal_buffer: Vec<BacktestSignal>,
    trade_buffer: Vec<PendingTrade>,
}

impl PgBacktestRecorder {
    fn config_hash(config: &serde_json::Value) -> String {
        use std::hash::{Hash, Hasher};
        let json = serde_json::to_string(config).unwrap_or_default();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        json.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Create a new recorder and INSERT the `backtest_runs` row.
    ///
    /// This is async because it writes to DB. Call from an async context
    /// (e.g. before entering the sync engine loop).
    pub async fn new(
        pool: sqlx::PgPool,
        strategy: &str,
        mode: &str,
        config: &serde_json::Value,
        symbols: &[String],
    ) -> anyhow::Result<Self> {
        let run_id = Uuid::new_v4();
        let primary_insert = sqlx::query(
            "INSERT INTO backtest_runs (run_id, strategy, mode, config_json, symbols)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(run_id)
        .bind(strategy)
        .bind(mode)
        .bind(config)
        .bind(symbols);

        if let Err(primary_err) = primary_insert.execute(&pool).await {
            // Compatibility path for legacy 019 schema constraints that may still exist
            // on partially-migrated environments (strategy_id/config_hash/started_at NOT NULL).
            let compat_insert = sqlx::query(
                "INSERT INTO backtest_runs
                 (run_id, strategy, mode, config_json, symbols, strategy_id, config_hash, started_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(run_id)
            .bind(strategy)
            .bind(mode)
            .bind(config)
            .bind(symbols)
            .bind(strategy)
            .bind(Self::config_hash(config))
            .bind(Utc::now());

            if let Err(compat_err) = compat_insert.execute(&pool).await {
                return Err(anyhow::anyhow!(
                    "failed to create backtest run row (primary insert error: {}; compatibility insert error: {})",
                    primary_err,
                    compat_err
                ));
            }

            tracing::warn!(
                %run_id,
                strategy,
                mode,
                "backtest run inserted via legacy-compat path (strategy_id/config_hash/started_at)"
            );
        }

        tracing::info!(%run_id, strategy, mode, "backtest run created");

        Ok(Self {
            pool,
            run_id,
            signal_buffer: Vec::with_capacity(BATCH_THRESHOLD),
            trade_buffer: Vec::with_capacity(BATCH_THRESHOLD),
        })
    }

    pub fn run_id(&self) -> Uuid {
        self.run_id
    }

    /// Flush all buffered signals and trades to DB.
    pub async fn flush_async(&mut self) -> anyhow::Result<()> {
        self.flush_signals().await?;
        self.flush_trades().await?;
        Ok(())
    }

    /// Flush signals + trades, then update `backtest_runs` with summary metrics.
    pub async fn finalize(
        &mut self,
        data_start: Option<DateTime<Utc>>,
        data_end: Option<DateTime<Utc>>,
        total_trades: i32,
        win_rate: f64,
        total_pnl: Decimal,
        sharpe_ratio: f64,
        max_drawdown: Decimal,
        profit_factor: f64,
    ) -> anyhow::Result<()> {
        self.flush_async().await?;

        sqlx::query(
            "UPDATE backtest_runs
             SET data_start = $2, data_end = $3, total_trades = $4,
                 win_rate = $5, total_pnl = $6, sharpe_ratio = $7,
                 max_drawdown = $8, profit_factor = $9
             WHERE run_id = $1",
        )
        .bind(self.run_id)
        .bind(data_start)
        .bind(data_end)
        .bind(total_trades)
        .bind(win_rate)
        .bind(total_pnl)
        .bind(sharpe_ratio)
        .bind(max_drawdown)
        .bind(profit_factor)
        .execute(&self.pool)
        .await?;

        tracing::info!(run_id = %self.run_id, total_trades, "backtest run finalized");
        Ok(())
    }

    async fn flush_signals(&mut self) -> anyhow::Result<()> {
        if self.signal_buffer.is_empty() {
            return Ok(());
        }

        let signals = std::mem::take(&mut self.signal_buffer);
        for chunk in signals.chunks(BATCH_THRESHOLD) {
            let mut qb = sqlx::QueryBuilder::new(
                "INSERT INTO backtest_signals \
                 (run_id, signal_type, symbol, direction, timestamp, \
                  p_hat, ev_net, sigma, market_price, spot_price, \
                  s0, time_remaining_secs, filter_reason, exit_reason, exit_price) ",
            );
            qb.push_values(chunk, |mut b, s| {
                b.push_bind(self.run_id)
                    .push_bind(s.signal_type.as_str())
                    .push_bind(&s.symbol)
                    .push_bind(&s.direction)
                    .push_bind(s.timestamp)
                    .push_bind(s.p_hat)
                    .push_bind(s.ev_net)
                    .push_bind(s.sigma)
                    .push_bind(s.market_price)
                    .push_bind(s.spot_price)
                    .push_bind(s.s0)
                    .push_bind(s.time_remaining_secs)
                    .push_bind(&s.filter_reason)
                    .push_bind(&s.exit_reason)
                    .push_bind(s.exit_price);
            });
            qb.build().execute(&self.pool).await?;
        }

        tracing::debug!(run_id = %self.run_id, count = signals.len(), "flushed signals");
        Ok(())
    }

    async fn flush_trades(&mut self) -> anyhow::Result<()> {
        if self.trade_buffer.is_empty() {
            return Ok(());
        }

        let trades = std::mem::take(&mut self.trade_buffer);
        for chunk in trades.chunks(BATCH_THRESHOLD) {
            let mut qb = sqlx::QueryBuilder::new(
                "INSERT INTO backtest_trades \
                 (run_id, symbol, direction, entry_time, exit_time, \
                  entry_price, exit_price, shares, pnl, won, \
                  holding_secs, exit_reason, entry_p_hat, entry_ev_net, \
                  entry_sigma, s0) ",
            );
            qb.push_values(chunk, |mut b, t| {
                b.push_bind(self.run_id)
                    .push_bind(&t.symbol)
                    .push_bind(&t.direction)
                    .push_bind(t.entry_time)
                    .push_bind(t.exit_time)
                    .push_bind(t.entry_price)
                    .push_bind(t.exit_price)
                    .push_bind(t.shares)
                    .push_bind(t.pnl)
                    .push_bind(t.won)
                    .push_bind(t.holding_secs)
                    .push_bind(&t.exit_reason)
                    .push_bind(t.entry_p_hat)
                    .push_bind(t.entry_ev_net)
                    .push_bind(t.entry_sigma)
                    .push_bind(t.s0);
            });
            qb.build().execute(&self.pool).await?;
        }

        tracing::debug!(run_id = %self.run_id, count = trades.len(), "flushed trades");
        Ok(())
    }
}

impl BacktestRecorder for PgBacktestRecorder {
    fn record_entry(&mut self, signal: &BacktestSignal) {
        self.signal_buffer.push(signal.clone());
    }

    fn record_exit(&mut self, signal: &BacktestSignal) {
        self.signal_buffer.push(signal.clone());
    }

    fn record_filtered(&mut self, signal: &BacktestSignal, reason: &str) {
        let mut s = signal.clone();
        s.filter_reason = Some(reason.to_string());
        s.signal_type = SignalType::Filtered;
        self.signal_buffer.push(s);
    }

    fn record_trade(&mut self, trade: &PendingTrade) {
        self.trade_buffer.push(trade.clone());
    }

    fn flush(&mut self) -> anyhow::Result<()> {
        // No-op in sync context. Use flush_async() from async context instead.
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
