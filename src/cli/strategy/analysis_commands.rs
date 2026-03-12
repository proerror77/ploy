use super::*;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct AccuracyArgs {
    /// Lookback window in hours (scopes which entry intents are scored)
    #[arg(long, default_value = "12")]
    lookback_hours: u64,

    /// Filter by domain: crypto|sports|politics
    #[arg(long)]
    domain: Option<String>,

    /// Filter by account_id (defaults to all)
    #[arg(long)]
    account_id: Option<String>,

    /// Filter by agent_id (defaults to all)
    #[arg(long)]
    agent_id: Option<String>,

    /// Only include live orders (exclude dry-run)
    #[arg(long)]
    live_only: bool,

    /// Max number of intents to print (latest first)
    #[arg(long, default_value = "200")]
    limit: usize,

    /// Skip refreshing settlement status via Gamma API (uses cached DB rows only)
    #[arg(long)]
    no_refresh: bool,

    /// Database URL (uses DATABASE_URL env var if omitted)
    #[arg(long)]
    database_url: Option<String>,
}

impl AccuracyArgs {
    pub(super) async fn run(self) -> Result<()> {
        report_accuracy_pm_settlement(
            self.lookback_hours,
            self.domain,
            self.account_id,
            self.agent_id,
            self.live_only,
            self.limit,
            self.no_refresh,
            self.database_url,
        )
        .await
    }
}

#[derive(Args, Debug, Clone)]
pub struct DirectionalSignalBacktestArgs {
    /// Lookback window in hours (which directional signals are included)
    #[arg(long, default_value = "168")]
    lookback_hours: u64,

    /// Filter by account_id (defaults to all)
    #[arg(long)]
    account_id: Option<String>,

    /// Filter by agent_id (defaults to all)
    #[arg(long)]
    agent_id: Option<String>,

    /// Only include live signals (exclude context->>'dry_run' = true)
    #[arg(long)]
    live_only: bool,

    /// Max number of signals to backtest (latest first)
    #[arg(long, default_value = "50000")]
    limit: usize,

    /// Skip refreshing settlement status via Gamma API (uses cached DB rows only)
    #[arg(long)]
    no_refresh: bool,

    /// Database URL (uses DATABASE_URL env var if omitted)
    #[arg(long)]
    database_url: Option<String>,
}

impl DirectionalSignalBacktestArgs {
    pub(super) async fn run(self) -> Result<()> {
        run_backtest(
            "directional",
            StrategyBacktestMode::Settlement,
            None,
            None,
            "BTCUSDT,ETHUSDT,SOLUSDT",
            10000.0,
            false,
            false,
            self.lookback_hours,
            self.account_id,
            self.agent_id,
            self.live_only,
            self.limit,
            self.no_refresh,
            false,
            None,
            false,
            self.database_url,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            PmReplayQuality::Strict,
        )
        .await
    }
}

#[derive(Args, Debug, Clone)]
pub struct ExportCryptoLobDatasetArgs {
    /// Lookback window in hours (which entry intents are exported)
    #[arg(long, default_value = "168")]
    lookback_hours: u64,

    /// Filter by account_id (defaults to all)
    #[arg(long)]
    account_id: Option<String>,

    /// Filter by agent_id (defaults to all)
    #[arg(long)]
    agent_id: Option<String>,

    /// Only include live orders (exclude dry-run)
    #[arg(long)]
    live_only: bool,

    /// Skip refreshing settlement status via Gamma API (uses cached DB rows only)
    #[arg(long)]
    no_refresh: bool,

    /// Max number of intents to export (latest first)
    #[arg(long, default_value = "50000")]
    limit: usize,

    /// Output format (default: parquet if built with --features analysis, else csv)
    #[arg(long, value_enum, default_value_t = CryptoLobDatasetFormat::default())]
    format: CryptoLobDatasetFormat,

    /// Output path (default: ./data/crypto_lob_dataset.{csv|parquet})
    #[arg(long)]
    output: Option<PathBuf>,

    /// Database URL (uses DATABASE_URL env var if omitted)
    #[arg(long)]
    database_url: Option<String>,
}

impl ExportCryptoLobDatasetArgs {
    pub(super) async fn run(self) -> Result<()> {
        export_crypto_lob_dataset(
            self.lookback_hours,
            self.account_id,
            self.agent_id,
            self.live_only,
            self.no_refresh,
            self.limit,
            self.format,
            self.output,
            self.database_url,
        )
        .await
    }
}

#[derive(Args, Debug, Clone)]
pub struct BacktestArgs {
    /// Strategy name (momentum, directional, prob-garch/prob_garch, liquidity-vacuum/liquidity_vacuum, staggered-arb/staggered_arb/gamma_scalping)
    name: String,

    /// Backtest mode: replay historical feed, or score directional signals by settlement
    #[arg(long, value_enum, default_value_t = StrategyBacktestMode::Replay)]
    mode: StrategyBacktestMode,

    /// Start date (ISO 8601)
    #[arg(long)]
    from: Option<String>,

    /// End date (ISO 8601)
    #[arg(long)]
    to: Option<String>,

    /// Symbols (comma-separated)
    #[arg(long, default_value = "BTCUSDT,ETHUSDT,SOLUSDT")]
    symbols: String,

    /// Initial capital (USD)
    #[arg(long, default_value = "10000")]
    capital: f64,

    /// Save results to DB
    #[arg(long)]
    save: bool,

    /// Output JSON
    #[arg(long)]
    json: bool,

    /// Settlement-mode lookback window in hours
    #[arg(long, default_value = "168")]
    lookback_hours: u64,

    /// Settlement-mode filter by account_id (defaults to all)
    #[arg(long)]
    account_id: Option<String>,

    /// Settlement-mode filter by agent_id (defaults to all)
    #[arg(long)]
    agent_id: Option<String>,

    /// Settlement-mode only include live signals (exclude dry-run)
    #[arg(long)]
    live_only: bool,

    /// Settlement-mode max number of signals to score (latest first)
    #[arg(long, default_value = "50000")]
    limit: usize,

    /// Settlement-mode skip Gamma refresh and use cached settlement rows only
    #[arg(long)]
    no_refresh: bool,

    /// Skip Gamma verification phase after replay backtest
    #[arg(long)]
    skip_gamma: bool,

    /// Verify an existing backtest run by UUID
    #[arg(long)]
    verify_run: Option<String>,

    /// Print a database data-availability summary for this backtest and exit
    #[arg(long)]
    diagnose_db: bool,

    /// Database URL (uses DATABASE_URL env var if omitted)
    #[arg(long)]
    database_url: Option<String>,

    /// Liquidity-vacuum profile preset.
    ///
    /// `prod` keeps strict thresholds; `research` is exploratory;
    /// `research-v2` is a higher-quality research baseline.
    #[arg(long, value_enum, default_value_t = LiquidityVacuumProfile::Prod)]
    lv_profile: LiquidityVacuumProfile,

    /// Override liquidity-vacuum price move threshold (fraction, e.g. 0.02 = 2%)
    #[arg(long)]
    lv_price_move_threshold: Option<f64>,

    /// Override liquidity-vacuum volume multiplier threshold (e.g. 3.0)
    #[arg(long)]
    lv_volume_multiplier_threshold: Option<f64>,

    /// Override liquidity-vacuum order concentration threshold (e.g. 0.7)
    #[arg(long)]
    lv_order_concentration_threshold: Option<f64>,

    /// Override liquidity-vacuum entry deviation threshold (fraction)
    #[arg(long)]
    lv_entry_deviation_threshold: Option<f64>,

    /// Override liquidity-vacuum entry z-score threshold (0 disables z-score entry gate)
    #[arg(long)]
    lv_entry_zscore_threshold: Option<f64>,

    /// Override liquidity-vacuum take-profit z-score threshold (0 disables z-score TP)
    #[arg(long)]
    lv_take_profit_zscore_threshold: Option<f64>,

    /// Override liquidity-vacuum stop-loss z-score threshold (0 disables z-score SL)
    #[arg(long)]
    lv_stop_loss_zscore_threshold: Option<f64>,

    /// Override liquidity-vacuum EMA-band take-profit threshold (fraction, e.g. 0.03)
    #[arg(long)]
    lv_take_profit_ema_band_pct: Option<f64>,

    /// Override liquidity-vacuum stop-loss threshold (fraction, e.g. 0.25)
    #[arg(long)]
    lv_stop_loss_pct: Option<f64>,

    /// Override liquidity-vacuum minimum edge buffer above fees (fraction)
    #[arg(long)]
    lv_min_edge_buffer: Option<f64>,

    /// Override liquidity-vacuum z-score lookback sample size
    #[arg(long)]
    lv_zscore_lookback_samples: Option<usize>,

    /// Override liquidity-vacuum max holding seconds (0 disables max-hold exit)
    #[arg(long)]
    lv_max_holding_secs: Option<u64>,

    /// Override staggered-arb entry window (seconds after event start; 0 disables)
    #[arg(long)]
    sa_entry_after_start_max_secs: Option<u64>,

    /// Minimum PM event-window quality for pm_5m_directional replay.
    #[arg(long, value_enum, default_value_t = PmReplayQuality::Strict)]
    pm5_event_quality: PmReplayQuality,
}

impl BacktestArgs {
    pub(super) async fn run(self) -> Result<()> {
        run_backtest(
            &self.name,
            self.mode,
            self.from,
            self.to,
            &self.symbols,
            self.capital,
            self.save,
            self.json,
            self.lookback_hours,
            self.account_id,
            self.agent_id,
            self.live_only,
            self.limit,
            self.no_refresh,
            self.skip_gamma,
            self.verify_run,
            self.diagnose_db,
            self.database_url,
            Some(self.lv_profile),
            self.lv_price_move_threshold,
            self.lv_volume_multiplier_threshold,
            self.lv_order_concentration_threshold,
            self.lv_entry_deviation_threshold,
            self.lv_entry_zscore_threshold,
            self.lv_take_profit_zscore_threshold,
            self.lv_stop_loss_zscore_threshold,
            self.lv_take_profit_ema_band_pct,
            self.lv_stop_loss_pct,
            self.lv_min_edge_buffer,
            self.lv_zscore_lookback_samples,
            self.lv_max_holding_secs,
            self.sa_entry_after_start_max_secs,
            self.pm5_event_quality,
        )
        .await
    }
}

#[derive(Args, Debug, Clone)]
pub struct BacktestListArgs {
    #[arg(long)]
    database_url: Option<String>,
    #[arg(long, default_value = "20")]
    limit: usize,
}

impl BacktestListArgs {
    pub(super) async fn run(self) -> Result<()> {
        run_backtest_list(self.database_url, self.limit).await
    }
}

#[derive(Args, Debug, Clone)]
pub struct BacktestDiffArgs {
    run1: String,
    run2: String,
    #[arg(long)]
    database_url: Option<String>,
}

impl BacktestDiffArgs {
    pub(super) async fn run(self) -> Result<()> {
        run_backtest_diff(&self.run1, &self.run2, self.database_url).await
    }
}

#[derive(Args, Debug, Clone)]
pub struct LiveBacktestCompareArgs {
    /// Backtest run UUID
    run_id: String,

    /// Lookback window in hours for live order observations
    #[arg(long, default_value = "72")]
    lookback_hours: u64,

    /// Filter by account_id (defaults to all)
    #[arg(long)]
    account_id: Option<String>,

    /// Filter by strategy_id (defaults to all)
    #[arg(long)]
    strategy_id: Option<String>,

    /// Database URL (uses DATABASE_URL env var if omitted)
    #[arg(long)]
    database_url: Option<String>,
}

impl LiveBacktestCompareArgs {
    pub(super) async fn run(self) -> Result<()> {
        run_live_backtest_compare(
            &self.run_id,
            self.lookback_hours,
            self.account_id,
            self.strategy_id,
            self.database_url,
        )
        .await
    }
}
