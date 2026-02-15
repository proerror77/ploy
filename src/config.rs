use config::{Config, ConfigError, Environment, File};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::Path;

/// Main configuration structure
#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub market: MarketConfig,
    pub strategy: StrategyConfig,
    pub execution: ExecutionConfig,
    pub risk: RiskConfig,
    pub database: DatabaseConfig,
    pub dry_run: DryRunConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
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
    /// Optional event registry discovery service
    #[serde(default)]
    pub event_registry: Option<DiscoveryConfig>,
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

fn default_confirm_fill_timeout_ms() -> u64 {
    2000
}

fn default_max_quote_age() -> u64 {
    5 // 5 seconds max for trading decisions
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
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

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    /// PostgreSQL connection URL
    pub url: String,
    /// Maximum connections in pool
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
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
            .set_default("execution.poll_interval_ms", 500)?
            .set_default("execution.confirm_fills", false)?
            .set_default(
                "execution.confirm_fill_timeout_ms",
                default_confirm_fill_timeout_ms(),
            )?
            .set_default("database.max_connections", 5)?
            .set_default("api_port", 8081)?;

        // Accept either a config directory (`config/`) or a single TOML file
        // (`config/default.toml`) for CLI compatibility.
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
            market: MarketConfig {
                ws_url: "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string(),
                rest_url: "https://clob.polymarket.com".to_string(),
                market_slug: market_slug.to_string(),
                condition_id: None,
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
            logging: LoggingConfig::default(),
            health_port: Some(8080),
            api_port: Some(8081),
            event_edge_agent: None,
            nba_comeback: None,
            event_registry: None,
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

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn apply_env_overrides(&mut self) {
        if let Some(v) = env_bool(&["PLOY_DRY_RUN__ENABLED", "PLOY__DRY_RUN__ENABLED"]) {
            self.dry_run.enabled = v;
        }

        if let Some(v) = env_string(&["PLOY_MARKET__MARKET_SLUG", "PLOY__MARKET__MARKET_SLUG"]) {
            self.market.market_slug = v;
        }

        if let Some(v) = env_u16(&["PLOY_API_PORT", "PLOY__API_PORT"]) {
            self.api_port = Some(v);
        }

        if let Some(v) = env_string(&[
            "PLOY_DATABASE__URL",
            "PLOY__DATABASE__URL",
            "PLOY_DATABASE_URL",
            "DATABASE_URL",
        ]) {
            self.database.url = v;
        }

        if let Some(v) = env_string(&[
            "PLOY_DATABASE__MAX_CONNECTIONS",
            "PLOY__DATABASE__MAX_CONNECTIONS",
            "PLOY_DATABASE_MAX_CONNECTIONS",
        ])
        .and_then(|raw| raw.parse::<u32>().ok())
        {
            self.database.max_connections = v;
        }

        let ee_enabled = env_bool(&[
            "PLOY_EVENT_EDGE_AGENT__ENABLED",
            "PLOY__EVENT_EDGE_AGENT__ENABLED",
        ]);
        let ee_trade = env_bool(&[
            "PLOY_EVENT_EDGE_AGENT__TRADE",
            "PLOY__EVENT_EDGE_AGENT__TRADE",
        ]);
        let ee_event_ids = env_list(&[
            "PLOY_EVENT_EDGE_AGENT__EVENT_IDS",
            "PLOY__EVENT_EDGE_AGENT__EVENT_IDS",
            "PLOY_EVENT_EDGE_AGENT_EVENT_IDS",
        ]);
        let ee_titles = env_list(&[
            "PLOY_EVENT_EDGE_AGENT__TITLES",
            "PLOY__EVENT_EDGE_AGENT__TITLES",
            "PLOY_EVENT_EDGE_AGENT_TITLES",
        ]);
        if ee_enabled.is_some() || ee_trade.is_some() {
            let ee = self
                .event_edge_agent
                .get_or_insert_with(EventEdgeAgentConfig::default);
            if let Some(v) = ee_enabled {
                ee.enabled = v;
            }
            if let Some(v) = ee_trade {
                ee.trade = v;
            }
        }
        if ee_event_ids.is_some() || ee_titles.is_some() {
            let ee = self
                .event_edge_agent
                .get_or_insert_with(EventEdgeAgentConfig::default);
            if let Some(v) = ee_event_ids {
                ee.event_ids = v;
            }
            if let Some(v) = ee_titles {
                ee.titles = v;
            }
        }
    }
}

fn env_string(keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Ok(v) = std::env::var(key) {
            return Some(v);
        }
    }
    None
}

fn env_u16(keys: &[&str]) -> Option<u16> {
    env_string(keys).and_then(|v| v.parse::<u16>().ok())
}

fn env_bool(keys: &[&str]) -> Option<bool> {
    env_string(keys).and_then(|v| parse_bool_like(&v))
}

fn env_list(keys: &[&str]) -> Option<Vec<String>> {
    env_string(keys).map(|raw| parse_string_list(&raw))
}

fn parse_string_list(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    if trimmed.starts_with('[') {
        if let Ok(values) = serde_json::from_str::<Vec<String>>(trimmed) {
            return values
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }

    trimmed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn parse_bool_like(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
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
        let parsed = parse_string_list("a,b, c ,,d");
        assert_eq!(parsed, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_parse_string_list_json_array() {
        let parsed = parse_string_list(r#"["id-1","id-2"]"#);
        assert_eq!(parsed, vec!["id-1", "id-2"]);
    }
}
