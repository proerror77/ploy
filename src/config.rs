use config::{Config, ConfigError, Environment, File};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

mod env_overrides;

use crate::agent_runtime::AgentRiskParams;

/// Main configuration structure
#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    /// Execution/account scope (single DB, multiple accounts)
    #[serde(default)]
    pub account: AccountConfig,
    pub market: MarketConfig,
    pub strategy: StrategyConfig,
    pub execution: ExecutionConfig,
    pub risk: RiskConfig,
    pub database: DatabaseConfig,
    pub dry_run: DryRunConfig,
    #[serde(default)]
    pub kalshi: KalshiConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    /// Agent framework control-plane mode.
    #[serde(default)]
    pub agent_framework: AgentFrameworkConfig,
    /// Health server port (default: 8080)
    #[serde(default)]
    pub health_port: Option<u16>,
    /// API server port (default: 8081, when `api` feature is enabled)
    #[serde(default)]
    pub api_port: Option<u16>,
    /// Optional always-on external event mispricing agent (Arena → Polymarket)
    #[serde(default)]
    pub event_edge_agent: Option<EventEdgeAgentConfig>,
    /// Optional NBA Q3→Q4 comeback trading agent
    #[serde(default)]
    pub nba_comeback: Option<NbaComebackConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountConfig {
    /// A stable identifier for scoping DB writes (e.g. "default", "acct1", "tango21").
    #[serde(default = "default_account_id")]
    pub id: String,
    /// Optional address metadata (for human ops/debugging).
    #[serde(default)]
    pub wallet_address: Option<String>,
    /// Optional label (e.g. "Main", "Paper", "Sports").
    #[serde(default)]
    pub label: Option<String>,
}

impl Default for AccountConfig {
    fn default() -> Self {
        Self {
            id: default_account_id(),
            wallet_address: None,
            label: None,
        }
    }
}

fn default_account_id() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentFrameworkConfig {
    /// Agent framework mode:
    /// - "internal": built-in Rust agents are enabled by config flags.
    /// - "openclaw": OpenClaw orchestrates agents; built-in agent runtime can be disabled.
    #[serde(default = "default_agent_framework_mode")]
    pub mode: String,
    /// If true and mode=openclaw, disable built-in agent runtime entrypoints.
    #[serde(default = "default_agent_framework_hard_disable")]
    pub hard_disable_internal_agents: bool,
}

impl Default for AgentFrameworkConfig {
    fn default() -> Self {
        Self {
            mode: default_agent_framework_mode(),
            hard_disable_internal_agents: default_agent_framework_hard_disable(),
        }
    }
}

impl AgentFrameworkConfig {
    pub fn is_openclaw_mode(&self) -> bool {
        self.mode.eq_ignore_ascii_case("openclaw")
    }
}

fn default_agent_framework_mode() -> String {
    "internal".to_string()
}

fn default_agent_framework_hard_disable() -> bool {
    false
}

/// Entry mode for crypto-managed runtime configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CryptoEntryMode {
    /// Original arbitrage-only mode: require sum_of_asks < threshold.
    ArbOnly,
    /// Directional mode: trade based on momentum edge alone, no sum constraint.
    Directional,
    /// Volatility straddle: buy both UP and DOWN when sum < straddle_threshold.
    VolStraddle,
}

fn default_crypto_entry_mode() -> CryptoEntryMode {
    CryptoEntryMode::Directional
}

fn default_crypto_exit_edge_floor() -> Decimal {
    Decimal::new(2, 2)
}

fn default_crypto_exit_price_band() -> Decimal {
    Decimal::new(5, 2)
}

fn default_crypto_oracle_lag_buffer_secs() -> u64 {
    3
}

fn default_crypto_max_spread_pct() -> Decimal {
    Decimal::new(10, 2)
}

fn default_crypto_straddle_threshold() -> Decimal {
    Decimal::new(99, 2)
}

fn default_crypto_straddle_min_vol() -> Decimal {
    Decimal::ZERO
}

fn default_crypto_min_signal_score() -> Decimal {
    Decimal::new(40, 2)
}

/// Neutral config owner for the canonical crypto runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoTradingConfig {
    pub agent_id: String,
    pub name: String,
    pub coins: Vec<String>,
    pub sum_threshold: Decimal,
    pub min_momentum_1s: f64,
    #[serde(default)]
    pub min_window_move_pct: Decimal,
    #[serde(default = "default_crypto_exit_edge_floor")]
    pub min_edge: Decimal,
    pub event_refresh_secs: u64,
    pub min_time_remaining_secs: u64,
    pub max_time_remaining_secs: u64,
    pub prefer_close_to_end: bool,
    #[serde(default)]
    pub entry_cooldown_secs: u64,
    #[serde(default)]
    pub require_mtf_agreement: bool,
    pub default_shares: u64,
    #[serde(default = "default_crypto_exit_edge_floor")]
    pub exit_edge_floor: Decimal,
    #[serde(default = "default_crypto_exit_price_band")]
    pub exit_price_band: Decimal,
    pub enable_price_exits: bool,
    pub min_hold_secs: u64,
    pub risk_params: AgentRiskParams,
    pub heartbeat_interval_secs: u64,
    #[serde(default = "default_crypto_entry_mode")]
    pub entry_mode: CryptoEntryMode,
    #[serde(default = "default_crypto_oracle_lag_buffer_secs")]
    pub oracle_lag_buffer_secs: u64,
    #[serde(default = "default_crypto_max_spread_pct")]
    pub max_spread_pct: Decimal,
    #[serde(default = "default_crypto_straddle_threshold")]
    pub straddle_threshold: Decimal,
    #[serde(default = "default_crypto_straddle_min_vol")]
    pub straddle_min_vol: Decimal,
    #[serde(default = "default_crypto_min_signal_score")]
    pub min_signal_score: Decimal,
}

impl Default for CryptoTradingConfig {
    fn default() -> Self {
        Self {
            agent_id: "crypto".into(),
            name: "Crypto Momentum".into(),
            coins: vec!["BTC".into(), "ETH".into(), "SOL".into(), "XRP".into()],
            sum_threshold: Decimal::new(96, 2),
            min_momentum_1s: 0.001,
            min_window_move_pct: Decimal::new(1, 4),
            min_edge: Decimal::new(2, 2),
            event_refresh_secs: 30,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            prefer_close_to_end: true,
            entry_cooldown_secs: 0,
            require_mtf_agreement: true,
            default_shares: 100,
            exit_edge_floor: default_crypto_exit_edge_floor(),
            exit_price_band: default_crypto_exit_price_band(),
            enable_price_exits: false,
            min_hold_secs: 20,
            risk_params: AgentRiskParams::conservative(),
            heartbeat_interval_secs: 5,
            entry_mode: default_crypto_entry_mode(),
            oracle_lag_buffer_secs: default_crypto_oracle_lag_buffer_secs(),
            max_spread_pct: default_crypto_max_spread_pct(),
            straddle_threshold: default_crypto_straddle_threshold(),
            straddle_min_vol: default_crypto_straddle_min_vol(),
            min_signal_score: default_crypto_min_signal_score(),
        }
    }
}

/// Neutral config owner for the registered politics/event-edge runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoliticsTradingConfig {
    pub agent_id: String,
    pub name: String,
    pub poll_interval_secs: u64,
    pub heartbeat_interval_secs: u64,
    pub risk_params: AgentRiskParams,
}

impl Default for PoliticsTradingConfig {
    fn default() -> Self {
        Self {
            agent_id: "politics".into(),
            name: "Event Edge".into(),
            poll_interval_secs: 300,
            heartbeat_interval_secs: 5,
            risk_params: AgentRiskParams::conservative(),
        }
    }
}

/// Neutral config owner for the registered sports runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SportsTradingConfig {
    #[serde(default = "default_account_id")]
    pub account_id: String,
    pub agent_id: String,
    pub name: String,
    pub poll_interval_secs: u64,
    pub heartbeat_interval_secs: u64,
    pub risk_params: AgentRiskParams,
}

impl Default for SportsTradingConfig {
    fn default() -> Self {
        Self {
            account_id: default_account_id(),
            agent_id: "sports".into(),
            name: "NBA Comeback".into(),
            poll_interval_secs: 30,
            heartbeat_interval_secs: 5,
            risk_params: AgentRiskParams::conservative(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventEdgeAgentConfig {
    /// Enable the agent inside `ploy run`
    #[serde(default)]
    pub enabled: bool,
    /// Polymarket event IDs to monitor (preferred)
    #[serde(default)]
    pub event_ids: Vec<String>,
    /// Polymarket event titles to discover via Gamma `title_contains`
    #[serde(default)]
    pub titles: Vec<String>,
    /// Poll interval seconds
    #[serde(default = "default_event_edge_interval_secs")]
    pub interval_secs: u64,
    /// Minimum edge (p_true - ask) to consider entering
    #[serde(default = "default_event_edge_min_edge")]
    pub min_edge: Decimal,
    /// Max entry price (ask) to pay
    #[serde(default = "default_event_edge_max_entry")]
    pub max_entry: Decimal,
    /// Shares per order
    #[serde(default = "default_event_edge_shares")]
    pub shares: u64,
    /// If true, places orders when conditions are met (respects global dry_run)
    #[serde(default)]
    pub trade: bool,
    /// Cooldown seconds per token (avoid repeated buys)
    #[serde(default = "default_event_edge_cooldown_secs")]
    pub cooldown_secs: u64,
    /// Maximum notional spend per UTC day (simple safety guard)
    #[serde(default = "default_event_edge_max_daily_spend_usd")]
    pub max_daily_spend_usd: Decimal,
    /// Agent framework to use: "deterministic", "event_driven", or "claude_agent_sdk"
    #[serde(default = "default_event_edge_framework")]
    pub framework: String,
    /// Claude model override for framework mode (optional)
    #[serde(default)]
    pub model: Option<String>,
    /// Maximum Claude turns per cycle (framework mode)
    #[serde(default = "default_event_edge_claude_max_turns")]
    pub claude_max_turns: u32,
}

impl EventEdgeAgentConfig {
    /// Validate config invariants. Returns list of problems (empty = valid).
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.min_edge <= Decimal::ZERO {
            errors.push(format!("min_edge must be > 0, got {}", self.min_edge));
        }
        if self.max_entry <= Decimal::ZERO || self.max_entry >= Decimal::ONE {
            errors.push(format!(
                "max_entry must be in (0, 1), got {}",
                self.max_entry
            ));
        }
        if self.shares == 0 {
            errors.push("shares must be > 0".to_string());
        }
        if self.max_daily_spend_usd <= Decimal::ZERO {
            errors.push(format!(
                "max_daily_spend_usd must be > 0, got {}",
                self.max_daily_spend_usd
            ));
        }
        errors
    }
}

impl Default for EventEdgeAgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            event_ids: Vec::new(),
            titles: Vec::new(),
            interval_secs: default_event_edge_interval_secs(),
            min_edge: default_event_edge_min_edge(),
            max_entry: default_event_edge_max_entry(),
            shares: default_event_edge_shares(),
            trade: false,
            cooldown_secs: default_event_edge_cooldown_secs(),
            max_daily_spend_usd: default_event_edge_max_daily_spend_usd(),
            framework: default_event_edge_framework(),
            model: None,
            claude_max_turns: default_event_edge_claude_max_turns(),
        }
    }
}

fn default_event_edge_interval_secs() -> u64 {
    30
}

fn default_event_edge_min_edge() -> Decimal {
    Decimal::new(8, 2) // 0.08
}

fn default_event_edge_max_entry() -> Decimal {
    Decimal::new(75, 2) // 0.75
}

fn default_event_edge_shares() -> u64 {
    100
}

fn default_event_edge_cooldown_secs() -> u64 {
    120
}

fn default_event_edge_max_daily_spend_usd() -> Decimal {
    Decimal::new(50, 0) // $50
}

fn default_event_edge_framework() -> String {
    "deterministic".to_string()
}

fn default_event_edge_claude_max_turns() -> u32 {
    30
}

/// NBA Q3→Q4 comeback trading agent configuration
#[derive(Debug, Clone, Deserialize)]
pub struct NbaComebackConfig {
    /// Enable the agent
    #[serde(default)]
    pub enabled: bool,
    /// Minimum edge (adjusted_win_prob - market_price) to enter
    #[serde(default = "default_nba_comeback_min_edge")]
    pub min_edge: Decimal,
    /// Maximum entry price (YES ask) to pay
    #[serde(default = "default_nba_comeback_max_entry_price")]
    pub max_entry_price: Decimal,
    /// Shares per order
    #[serde(default = "default_nba_comeback_shares")]
    pub shares: u64,
    /// Cooldown seconds per game (avoid repeated buys on same game)
    #[serde(default = "default_nba_comeback_cooldown_secs")]
    pub cooldown_secs: u64,
    /// Maximum notional spend per UTC day
    #[serde(default = "default_nba_comeback_max_daily_spend")]
    pub max_daily_spend_usd: Decimal,
    /// Minimum point deficit to consider (inclusive)
    #[serde(default = "default_nba_comeback_min_deficit")]
    pub min_deficit: i32,
    /// Maximum point deficit to consider (inclusive)
    #[serde(default = "default_nba_comeback_max_deficit")]
    pub max_deficit: i32,
    /// Target quarter to scan (3 = look for comebacks entering Q4)
    #[serde(default = "default_nba_comeback_target_quarter")]
    pub target_quarter: u8,
    /// ESPN poll interval in seconds
    #[serde(default = "default_nba_comeback_poll_interval")]
    pub espn_poll_interval_secs: u64,
    /// Minimum historical comeback rate to consider a team
    #[serde(default = "default_nba_comeback_min_rate")]
    pub min_comeback_rate: f64,
    /// Season string for DB lookups (e.g. "2025-26")
    #[serde(default = "default_nba_comeback_season")]
    pub season: String,
    /// Enable Grok live search as independent signal source
    #[serde(default)]
    pub grok_enabled: bool,
    /// Grok query interval in seconds (default 300 = 5 min)
    #[serde(default = "default_grok_interval")]
    pub grok_interval_secs: u64,
    /// Minimum edge from Grok signal to trigger trade
    #[serde(default = "default_grok_min_edge")]
    pub grok_min_edge: Decimal,
    /// Minimum Grok confidence to act on signal (0.0 to 1.0)
    #[serde(default = "default_grok_min_confidence")]
    pub grok_min_confidence: f64,
    /// Decision cooldown per game in seconds (default 60).
    /// Separate from the trade cooldown — prevents spamming Grok with
    /// redundant decision requests for the same game within a minute.
    #[serde(default = "default_grok_decision_cooldown")]
    pub grok_decision_cooldown_secs: u64,
    /// Enable rule-based fallback when Grok is unavailable for ESPN signals.
    /// ESPN comeback path has its own statistical model, so it can operate
    /// independently. Grok signal path has NO fallback.
    #[serde(default = "default_grok_fallback_enabled")]
    pub grok_fallback_enabled: bool,
    /// Minimum reward-to-risk ratio to consider a trade (default 4.0).
    /// reward_risk = (1 - price) / price. At 4.0x, max price ≈ $0.20.
    /// Opportunities below this threshold are filtered before querying Grok.
    #[serde(default = "default_min_reward_risk_ratio")]
    pub min_reward_risk_ratio: f64,
    /// Minimum expected value to consider a trade (default 0.05 = 5%).
    /// expected_value = fair_value - market_price.
    #[serde(default = "default_min_expected_value")]
    pub min_expected_value: f64,
    /// Kelly criterion fraction cap (default 0.25 = 25% of bankroll).
    /// Limits position sizing even when Kelly suggests larger bets.
    #[serde(default = "default_kelly_fraction_cap")]
    pub kelly_fraction_cap: f64,
    /// Halt opening NEW risk (new entries / scale-ins) once daily realized PnL
    /// drops below this negative threshold.
    #[serde(default = "default_performance_daily_loss_limit")]
    pub performance_daily_loss_limit_usd: Decimal,
    /// Minimum number of settled trades required before win-rate based sizing
    /// adjustment is activated.
    #[serde(default = "default_performance_min_settled_trades")]
    pub performance_min_settled_trades: u64,
    /// If settled-trade win rate drops below this threshold, reduce position size.
    #[serde(default = "default_performance_min_win_rate")]
    pub performance_min_win_rate: f64,
    /// Position-size multiplier applied when win rate is below threshold.
    #[serde(default = "default_performance_low_winrate_multiplier")]
    pub performance_low_winrate_multiplier: f64,
    /// Consecutive loss threshold that triggers additional size reduction.
    #[serde(default = "default_performance_loss_streak_threshold")]
    pub performance_loss_streak_threshold: u32,
    /// Additional size multiplier applied when consecutive losses hit threshold.
    #[serde(default = "default_performance_loss_streak_multiplier")]
    pub performance_loss_streak_multiplier: f64,
    // ── Kelly scaling-in ─────────────────────────────────────────
    /// Enable Kelly-proportional scaling-in (add to positions when price drops
    /// but fundamentals remain strong). Each cycle recalculates Kelly optimal
    /// total exposure and adds the delta if conditions are met.
    #[serde(default)]
    pub scaling_enabled: bool,
    /// Maximum number of additional entries per game beyond the initial (default 3)
    #[serde(default = "default_scaling_max_adds")]
    pub scaling_max_adds: u32,
    /// Minimum price drop (%) from last entry before adding (default 5.0)
    #[serde(default = "default_scaling_min_price_drop_pct")]
    pub scaling_min_price_drop_pct: f64,
    /// Maximum total exposure per game in USD (default 50)
    #[serde(default = "default_scaling_max_game_exposure")]
    pub scaling_max_game_exposure_usd: Decimal,
    /// Comeback rate must retain this fraction of initial rate to scale in (default 0.70)
    #[serde(default = "default_scaling_min_comeback_retention")]
    pub scaling_min_comeback_retention: f64,
    /// Minimum game time remaining in minutes for scaling-in (default 8.0)
    #[serde(default = "default_scaling_min_time_remaining")]
    pub scaling_min_time_remaining_mins: f64,
    /// Enable early exits before final settlement (take-profit / stop-loss).
    #[serde(default = "default_early_exit_enabled")]
    pub early_exit_enabled: bool,
    /// Take-profit trigger as percentage gain from average entry (default 15%).
    #[serde(default = "default_early_exit_take_profit_pct")]
    pub early_exit_take_profit_pct: f64,
    /// Stop-loss trigger as percentage drawdown from average entry (default 20%).
    #[serde(default = "default_early_exit_stop_loss_pct")]
    pub early_exit_stop_loss_pct: f64,
}

fn default_nba_comeback_min_edge() -> Decimal {
    Decimal::new(5, 2) // 0.05 = 5%
}
fn default_nba_comeback_max_entry_price() -> Decimal {
    Decimal::new(75, 2) // 0.75
}
fn default_nba_comeback_shares() -> u64 {
    50
}
fn default_nba_comeback_cooldown_secs() -> u64 {
    300 // 5 minutes per game
}
fn default_nba_comeback_max_daily_spend() -> Decimal {
    Decimal::new(100, 0) // $100
}
fn default_nba_comeback_min_deficit() -> i32 {
    1
}
fn default_nba_comeback_max_deficit() -> i32 {
    15
}
fn default_nba_comeback_target_quarter() -> u8 {
    3
}
fn default_nba_comeback_poll_interval() -> u64 {
    30
}
fn default_nba_comeback_min_rate() -> f64 {
    0.15 // 15%
}
fn default_nba_comeback_season() -> String {
    "2025-26".to_string()
}

fn default_grok_interval() -> u64 {
    300 // 5 minutes
}

fn default_grok_min_edge() -> Decimal {
    Decimal::new(8, 2) // 0.08 = 8%
}

fn default_grok_min_confidence() -> f64 {
    0.6
}

fn default_grok_decision_cooldown() -> u64 {
    60 // 1 minute
}

fn default_grok_fallback_enabled() -> bool {
    true
}

fn default_min_reward_risk_ratio() -> f64 {
    4.0
}

fn default_min_expected_value() -> f64 {
    0.05
}

fn default_kelly_fraction_cap() -> f64 {
    0.25
}

fn default_performance_daily_loss_limit() -> Decimal {
    Decimal::new(30, 0) // $30 daily realized loss stop
}

fn default_performance_min_settled_trades() -> u64 {
    10
}

fn default_performance_min_win_rate() -> f64 {
    0.45
}

fn default_performance_low_winrate_multiplier() -> f64 {
    0.60
}

fn default_performance_loss_streak_threshold() -> u32 {
    3
}

fn default_performance_loss_streak_multiplier() -> f64 {
    0.50
}

fn default_scaling_max_adds() -> u32 {
    3
}

fn default_scaling_min_price_drop_pct() -> f64 {
    5.0 // 5%
}

fn default_scaling_max_game_exposure() -> Decimal {
    Decimal::new(50, 0) // $50
}

fn default_scaling_min_comeback_retention() -> f64 {
    0.70 // 70% of initial comeback rate
}

fn default_scaling_min_time_remaining() -> f64 {
    8.0 // 8 minutes
}

fn default_early_exit_enabled() -> bool {
    true
}

fn default_early_exit_take_profit_pct() -> f64 {
    15.0
}

fn default_early_exit_stop_loss_pct() -> f64 {
    20.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarketConfig {
    /// WebSocket endpoint for market data
    pub ws_url: String,
    /// REST API endpoint for order execution
    pub rest_url: String,
    /// Market slug to trade (e.g., "btc-15m-up-down")
    pub market_slug: String,
    /// Condition ID for the market (required for orders)
    #[serde(default)]
    pub condition_id: Option<String>,
    /// Optional exchange-specific WS endpoint override.
    #[serde(default)]
    pub exchange_ws_url: Option<String>,
    /// Optional exchange-specific REST endpoint override.
    #[serde(default)]
    pub exchange_rest_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StrategyConfig {
    /// Number of shares per leg
    pub shares: u64,
    /// Minutes to watch for dump after round start
    pub window_min: u64,
    /// Percentage drop to trigger Leg1 (e.g., 0.15 = 15%)
    pub move_pct: Decimal,
    /// Raw sum target before fees (e.g., 0.95)
    pub sum_target: Decimal,
    /// Fee buffer to subtract from sum_target (e.g., 0.005 = 0.5%)
    pub fee_buffer: Decimal,
    /// Slippage buffer (e.g., 0.02 = 2%)
    pub slippage_buffer: Decimal,
    /// Minimum profit target (e.g., 0.01 = 1%)
    pub profit_buffer: Decimal,
}

impl StrategyConfig {
    /// Calculate effective sum target after all buffers
    /// sum_target_eff = sum_target - fee_buffer - slippage_buffer - profit_buffer
    pub fn effective_sum_target(&self) -> Decimal {
        self.sum_target - self.fee_buffer - self.slippage_buffer - self.profit_buffer
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionConfig {
    /// Exchange backend (`polymarket` or `kalshi`)
    #[serde(default = "default_execution_exchange")]
    pub exchange: String,
    /// Order timeout in milliseconds
    pub order_timeout_ms: u64,
    /// Maximum retry attempts for order submission
    pub max_retries: u8,
    /// Maximum spread in basis points to accept
    pub max_spread_bps: u32,
    /// Polling interval for order status in milliseconds
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
    /// Best-effort post-submit fill confirmation via polling
    #[serde(default)]
    pub confirm_fills: bool,
    /// Maximum time to wait for a terminal order status (ms) when confirm_fills is enabled
    #[serde(default = "default_confirm_fill_timeout_ms")]
    pub confirm_fill_timeout_ms: u64,
    /// Maximum quote age in seconds before rejecting trade (default: 5s)
    #[serde(default = "default_max_quote_age")]
    pub max_quote_age_secs: u64,
}

fn default_poll_interval() -> u64 {
    500
}

fn default_execution_exchange() -> String {
    "polymarket".to_string()
}

fn default_confirm_fill_timeout_ms() -> u64 {
    2000
}

fn default_max_quote_age() -> u64 {
    5 // 5 seconds max for trading decisions
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            exchange: default_execution_exchange(),
            order_timeout_ms: 5000,
            max_retries: 3,
            max_spread_bps: 500,
            poll_interval_ms: 500,
            confirm_fills: false,
            confirm_fill_timeout_ms: default_confirm_fill_timeout_ms(),
            max_quote_age_secs: default_max_quote_age(),
        }
    }
}

#[derive(Clone, Deserialize)]
pub struct KalshiConfig {
    /// Kalshi Trade API base URL.
    #[serde(default = "default_kalshi_base_url")]
    pub base_url: String,
    /// Optional API key (can also be sourced from env).
    #[serde(default)]
    pub api_key: Option<String>,
    /// Optional API secret (can also be sourced from env).
    #[serde(default)]
    pub api_secret: Option<String>,
}

fn redact_optional_secret(secret: &Option<String>) -> Option<&'static str> {
    secret.as_ref().map(|_| "[REDACTED]")
}

impl fmt::Debug for KalshiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KalshiConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &redact_optional_secret(&self.api_key))
            .field("api_secret", &redact_optional_secret(&self.api_secret))
            .finish()
    }
}

impl Default for KalshiConfig {
    fn default() -> Self {
        Self {
            base_url: default_kalshi_base_url(),
            api_key: None,
            api_secret: None,
        }
    }
}

fn default_kalshi_base_url() -> String {
    "https://api.elections.kalshi.com/trade-api/v2".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    /// Maximum single position exposure in USD
    pub max_single_exposure_usd: Decimal,
    /// Minimum seconds remaining to allow new Leg1
    pub min_remaining_seconds: u64,
    /// Number of consecutive failures before circuit breaker
    pub max_consecutive_failures: u32,
    /// Daily loss limit in USD (absolute value)
    pub daily_loss_limit_usd: Decimal,
    /// Seconds before round end to force Leg2 action
    pub leg2_force_close_seconds: u64,

    // === Fund Management ===
    /// Maximum concurrent positions (0 = unlimited)
    #[serde(default)]
    pub max_positions: u32,
    /// Maximum positions per symbol (e.g., 1 = only 1 BTC position at a time)
    /// Default: 1 to prevent one symbol from consuming all funds
    #[serde(default = "default_max_positions_per_symbol")]
    pub max_positions_per_symbol: u32,
    /// Percentage of available balance per trade (e.g., 0.10 = 10%)
    #[serde(default)]
    pub position_size_pct: Option<Decimal>,
    /// Fixed USD amount per trade (overrides position_size_pct if set)
    #[serde(default)]
    pub fixed_amount_usd: Option<Decimal>,
    /// Minimum balance to maintain (won't trade if balance below this)
    #[serde(default)]
    pub min_balance_usd: Decimal,
}

fn default_max_positions_per_symbol() -> u32 {
    1 // Default: only 1 position per symbol
}

#[derive(Clone, Deserialize)]
pub struct DatabaseConfig {
    /// PostgreSQL connection URL
    pub url: String,
    /// Maximum connections in pool
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

impl fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabaseConfig")
            .field("url", &"[REDACTED]")
            .field("max_connections", &self.max_connections)
            .finish()
    }
}

fn default_max_connections() -> u32 {
    5
}

#[derive(Debug, Clone, Deserialize)]
pub struct DryRunConfig {
    /// Enable dry run mode (no real orders)
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LoggingConfig {
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Enable JSON formatted logs
    #[serde(default)]
    pub json: bool,
}

fn default_log_level() -> String {
    "info".to_string()
}

impl AppConfig {
    /// Load configuration from files and environment
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from("config")
    }

    /// Load configuration from either a config directory or a single TOML file
    pub fn load_from<P: AsRef<Path>>(config_dir: P) -> Result<Self, ConfigError> {
        let config_path = config_dir.as_ref();

        let mut builder = Config::builder()
            // Start with default values
            .set_default("logging.level", "info")?
            .set_default("logging.json", false)?
            .set_default("execution.exchange", default_execution_exchange())?
            .set_default("execution.poll_interval_ms", 500)?
            .set_default("execution.confirm_fills", false)?
            .set_default(
                "execution.confirm_fill_timeout_ms",
                default_confirm_fill_timeout_ms(),
            )?
            .set_default("database.max_connections", 5)?
            .set_default("kalshi.base_url", default_kalshi_base_url())?
            .set_default("api_port", 8081)?;

        // Accept either a config directory (`config/`) or a single TOML file
        // (`config/default.toml`) for CLI loading.
        if config_path.is_file() {
            builder = builder.add_source(File::from(config_path).required(true));
        } else {
            builder = builder
                // Load default config file
                .add_source(File::from(config_path.join("default.toml")).required(false))
                // Load environment-specific config (e.g., config/production.toml)
                .add_source(
                    File::from(config_path.join(
                        std::env::var("PLOY_ENV").unwrap_or_else(|_| "development".to_string()),
                    ))
                    .required(false),
                );
        }

        builder = builder.add_source(
            // Override with environment variables (PLOY_MARKET__WS_URL, etc.)
            Environment::with_prefix("PLOY")
                .prefix_separator("_")
                .separator("__")
                .list_separator(",")
                .with_list_parse_key("event_edge_agent.event_ids")
                .with_list_parse_key("event_edge_agent.titles")
                .try_parsing(true),
        );

        let mut cfg: Self = builder.build()?.try_deserialize()?;
        cfg.apply_env_overrides();
        Ok(cfg)
    }

    /// Create a default configuration for CLI usage
    pub fn default_config(dry_run: bool, market_slug: &str) -> Self {
        use rust_decimal_macros::dec;

        Self {
            account: AccountConfig::default(),
            market: MarketConfig {
                ws_url: "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string(),
                rest_url: "https://clob.polymarket.com".to_string(),
                market_slug: market_slug.to_string(),
                condition_id: None,
                exchange_ws_url: None,
                exchange_rest_url: None,
            },
            strategy: StrategyConfig {
                shares: 20,
                window_min: 2,
                move_pct: dec!(0.15),
                sum_target: Decimal::ONE,
                fee_buffer: dec!(0.005),
                slippage_buffer: dec!(0.02),
                profit_buffer: dec!(0.01),
            },
            execution: ExecutionConfig {
                exchange: default_execution_exchange(),
                order_timeout_ms: 5000,
                max_retries: 3,
                max_spread_bps: 500,
                poll_interval_ms: 500,
                confirm_fills: false,
                confirm_fill_timeout_ms: default_confirm_fill_timeout_ms(),
                max_quote_age_secs: default_max_quote_age(),
            },
            risk: RiskConfig {
                max_single_exposure_usd: dec!(100),
                min_remaining_seconds: 30,
                max_consecutive_failures: 3,
                daily_loss_limit_usd: dec!(500),
                leg2_force_close_seconds: 20,
                // Fund management defaults
                max_positions: 3,                // Max 3 concurrent positions
                max_positions_per_symbol: 1,     // Only 1 position per symbol
                position_size_pct: None,         // Not using percentage-based sizing
                fixed_amount_usd: Some(dec!(1)), // $1 per trade
                min_balance_usd: dec!(2),        // Keep $2 minimum balance
            },
            database: DatabaseConfig {
                url: "postgres://localhost/ploy".to_string(),
                max_connections: 5,
            },
            dry_run: DryRunConfig { enabled: dry_run },
            kalshi: KalshiConfig::default(),
            logging: LoggingConfig::default(),
            agent_framework: AgentFrameworkConfig::default(),
            health_port: Some(8080),
            api_port: Some(8081),
            event_edge_agent: None,
            nba_comeback: None,
        }
    }

    /// Validate configuration values
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // Validate strategy params
        if self.strategy.move_pct <= Decimal::ZERO || self.strategy.move_pct >= Decimal::ONE {
            errors.push("move_pct must be between 0 and 1".to_string());
        }

        if self.strategy.sum_target <= Decimal::ZERO || self.strategy.sum_target > Decimal::ONE {
            errors.push("sum_target must be > 0 and <= 1".to_string());
        }

        let eff_target = self.strategy.effective_sum_target();
        if eff_target <= Decimal::ZERO {
            errors.push(format!(
                "Effective sum target is non-positive: {eff_target}. Check fee/slippage/profit buffers."
            ));
        }

        let exchange = self.execution.exchange.trim().to_ascii_lowercase();
        if exchange != "polymarket" && exchange != "kalshi" {
            errors.push(format!(
                "execution.exchange must be one of [polymarket, kalshi], got {}",
                self.execution.exchange
            ));
        }

        // Validate risk params
        if self.risk.max_single_exposure_usd <= Decimal::ZERO {
            errors.push("max_single_exposure_usd must be positive".to_string());
        }

        if self.risk.daily_loss_limit_usd <= Decimal::ZERO {
            errors.push("daily_loss_limit_usd must be positive".to_string());
        }

        if self.risk.leg2_force_close_seconds >= self.risk.min_remaining_seconds {
            errors.push(
                "leg2_force_close_seconds should be less than min_remaining_seconds".to_string(),
            );
        }

        let framework_mode = self.agent_framework.mode.trim().to_ascii_lowercase();
        if framework_mode != "internal" && framework_mode != "openclaw" {
            errors.push(format!(
                "agent_framework.mode must be one of [internal, openclaw], got {}",
                self.agent_framework.mode
            ));
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn apply_env_overrides(&mut self) {
        env_overrides::apply_env_overrides(self);
    }

    /// Whether built-in Rust agent loops must be disabled in this process.
    pub fn openclaw_runtime_lockdown(&self) -> bool {
        self.agent_framework.is_openclaw_mode() && self.agent_framework.hard_disable_internal_agents
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_effective_sum_target() {
        let strategy = StrategyConfig {
            shares: 20,
            window_min: 2,
            move_pct: dec!(0.15),
            sum_target: dec!(0.95),
            fee_buffer: dec!(0.005),
            slippage_buffer: dec!(0.02),
            profit_buffer: dec!(0.01),
        };

        // 0.95 - 0.005 - 0.02 - 0.01 = 0.915
        assert_eq!(strategy.effective_sum_target(), dec!(0.915));
    }

    #[test]
    fn test_parse_string_list_csv() {
        let parsed = env_overrides::parse_string_list("a,b, c ,,d");
        assert_eq!(parsed, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_parse_string_list_json_array() {
        let parsed = env_overrides::parse_string_list(r#"["id-1","id-2"]"#);
        assert_eq!(parsed, vec!["id-1", "id-2"]);
    }

    #[test]
    fn test_default_config_uses_polymarket_exchange() {
        let cfg = AppConfig::default_config(true, "test-market");
        assert_eq!(cfg.execution.exchange, "polymarket");
        assert_eq!(
            cfg.kalshi.base_url,
            "https://api.elections.kalshi.com/trade-api/v2"
        );
    }

    #[test]
    fn test_validate_rejects_unknown_execution_exchange() {
        let mut cfg = AppConfig::default_config(true, "test-market");
        cfg.execution.exchange = "unknown".to_string();
        let errors = cfg.validate().expect_err("validation should fail");
        assert!(errors
            .iter()
            .any(|e| e.contains("execution.exchange must be one of [polymarket, kalshi]")));
    }

    #[test]
    fn test_database_config_debug_redacts_url() {
        let cfg = DatabaseConfig {
            url: "postgres://user:password@localhost:5432/ploy".to_string(),
            max_connections: 7,
        };

        let rendered = format!("{:?}", cfg);

        assert!(!rendered.contains("password"));
        assert!(!rendered.contains("postgres://user:password@localhost:5432/ploy"));
        assert!(rendered.contains("7"));
    }

    #[test]
    fn test_kalshi_config_debug_redacts_credentials() {
        let cfg = KalshiConfig {
            base_url: "https://example.com".to_string(),
            api_key: Some("kalshi-key".to_string()),
            api_secret: Some("kalshi-secret".to_string()),
        };

        let rendered = format!("{:?}", cfg);

        assert!(!rendered.contains("kalshi-key"));
        assert!(!rendered.contains("kalshi-secret"));
        assert!(rendered.contains("https://example.com"));
    }
}
