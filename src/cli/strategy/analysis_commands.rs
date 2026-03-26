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
            None, // pm5_min_edge
            None, // pm5_p_entry
            None, // pm5_min_abs_z
            None, // pm5_obi_weight
            None, // pm5_flow_weight
            None, // pm5_microgap_weight
            None, // pm5_min_obi
            None, // pm5_no_trade_min
            None, // pm5_no_trade_max
            None, // pm5_no_trade_override_z
            None, // pm5_no_trade_override_flow
            None, // pm5_max_entry_price
            None, // pm5_vol_floor
            None, // pm5_cooldown_secs
            None, // pm5_max_concurrent
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

    /// For `pm_5m_directional` only, auto-trim sparse requested ranges to the longest contiguous common-coverage window
    #[arg(long)]
    pm5_auto_trim_window: bool,

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

    // ── pm_5m_directional overrides ──

    /// Override pm5m minimum edge threshold (e.g. 0.03)
    #[arg(long)]
    pm5_min_edge: Option<f64>,

    /// Override pm5m entry probability threshold (e.g. 0.62)
    #[arg(long)]
    pm5_p_entry: Option<f64>,

    /// Override pm5m minimum absolute z-score (e.g. 0.35)
    #[arg(long)]
    pm5_min_abs_z: Option<f64>,

    /// Override pm5m OBI weight in composite signal (e.g. 0.75)
    #[arg(long)]
    pm5_obi_weight: Option<f64>,

    /// Override pm5m flow weight in composite signal (e.g. 1.10)
    #[arg(long)]
    pm5_flow_weight: Option<f64>,

    /// Override pm5m microgap weight in composite signal (e.g. 0.40)
    #[arg(long)]
    pm5_microgap_weight: Option<f64>,

    /// Override pm5m minimum OBI threshold (e.g. 0.05)
    #[arg(long)]
    pm5_min_obi: Option<f64>,

    /// Override pm5m no-trade zone lower bound (e.g. 0.45)
    #[arg(long)]
    pm5_no_trade_min: Option<f64>,

    /// Override pm5m no-trade zone upper bound (e.g. 0.55)
    #[arg(long)]
    pm5_no_trade_max: Option<f64>,

    /// Override pm5m z-score threshold to override no-trade zone (e.g. 0.90)
    #[arg(long)]
    pm5_no_trade_override_z: Option<f64>,

    /// Override pm5m flow threshold to override no-trade zone (e.g. 0.45)
    #[arg(long)]
    pm5_no_trade_override_flow: Option<f64>,

    /// Override pm5m maximum entry price (e.g. 0.80)
    #[arg(long)]
    pm5_max_entry_price: Option<f64>,

    /// Override pm5m volatility floor (e.g. 0.0005)
    #[arg(long)]
    pm5_vol_floor: Option<f64>,

    /// Override pm5m cooldown between trades in seconds (e.g. 30)
    #[arg(long)]
    pm5_cooldown_secs: Option<u64>,

    /// Override pm5m max concurrent open trades
    #[arg(long)]
    pm5_max_concurrent: Option<usize>,
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
            self.pm5_auto_trim_window,
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
            self.pm5_min_edge,
            self.pm5_p_entry,
            self.pm5_min_abs_z,
            self.pm5_obi_weight,
            self.pm5_flow_weight,
            self.pm5_microgap_weight,
            self.pm5_min_obi,
            self.pm5_no_trade_min,
            self.pm5_no_trade_max,
            self.pm5_no_trade_override_z,
            self.pm5_no_trade_override_flow,
            self.pm5_max_entry_price,
            self.pm5_vol_floor,
            self.pm5_cooldown_secs,
            self.pm5_max_concurrent,
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
