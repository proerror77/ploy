use rust_decimal::Decimal;
use serde::Deserialize;

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

#[derive(Debug, Clone, Deserialize)]
pub struct EventEdgeAgentConfig {
    /// Enable the agent inside `ploy run`
    #[serde(default)]
    pub enabled: bool,
    /// Agent framework to use:
    /// - "deterministic" (default): internal loop with fixed rules
    /// - "event_driven": event-driven + persisted-state loop (Arena `last_updated` gating)
    /// - "claude_agent_sdk": tool-using agent via `claude-agent-sdk-rs` (Claude Code CLI)
    #[serde(default = "default_event_edge_framework")]
    pub framework: String,
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
        let valid_frameworks = ["deterministic", "event_driven", "claude_agent_sdk"];
        if !valid_frameworks.contains(&self.framework.as_str()) {
            errors.push(format!(
                "framework must be one of {:?}, got \"{}\"",
                valid_frameworks, self.framework
            ));
        }
        errors
    }
}

impl Default for EventEdgeAgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            framework: default_event_edge_framework(),
            event_ids: Vec::new(),
            titles: Vec::new(),
            interval_secs: default_event_edge_interval_secs(),
            min_edge: default_event_edge_min_edge(),
            max_entry: default_event_edge_max_entry(),
            shares: default_event_edge_shares(),
            trade: false,
            cooldown_secs: default_event_edge_cooldown_secs(),
            max_daily_spend_usd: default_event_edge_max_daily_spend_usd(),
            model: None,
            claude_max_turns: default_event_edge_claude_max_turns(),
        }
    }
}

fn default_event_edge_framework() -> String {
    "deterministic".to_string()
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

fn default_event_edge_claude_max_turns() -> u32 {
    20
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

/// Event registry discovery service configuration
#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryConfig {
    /// Enable the background discovery scanner
    #[serde(default)]
    pub enabled: bool,
    /// Scan interval in seconds (default: 300 = 5 minutes)
    #[serde(default = "default_discovery_scan_interval")]
    pub scan_interval_secs: u64,
    /// Sports keywords to scan (e.g. ["NBA", "NFL"])
    #[serde(default = "default_discovery_sports_keywords")]
    pub sports_keywords: Vec<String>,
    /// General keywords to scan
    #[serde(default)]
    pub general_keywords: Vec<String>,
}

fn default_discovery_scan_interval() -> u64 {
    300
}

fn default_discovery_sports_keywords() -> Vec<String> {
    vec!["NBA".to_string(), "NFL".to_string()]
}
