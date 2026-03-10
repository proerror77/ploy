use config::{Config, ConfigError, Environment, File};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::fmt;
use std::path::Path;

mod agent_configs;

pub use agent_configs::{
    AgentFrameworkConfig, DiscoveryConfig, EventEdgeAgentConfig, NbaComebackConfig,
};

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
    /// Optional event registry discovery service
    #[serde(default)]
    pub event_registry: Option<DiscoveryConfig>,
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
        if let Some(v) = env_bool(&["PLOY_DRY_RUN__ENABLED", "PLOY__DRY_RUN__ENABLED"]) {
            self.dry_run.enabled = v;
        }

        if let Some(v) = env_string(&["PLOY_ACCOUNT__ID", "PLOY__ACCOUNT__ID", "PLOY_ACCOUNT_ID"]) {
            if !v.trim().is_empty() {
                self.account.id = v;
            }
        }

        if let Some(v) = env_string(&[
            "PLOY_ACCOUNT__WALLET_ADDRESS",
            "PLOY__ACCOUNT__WALLET_ADDRESS",
            "PLOY_ACCOUNT_WALLET_ADDRESS",
        ]) {
            if !v.trim().is_empty() {
                self.account.wallet_address = Some(v);
            }
        }

        if let Some(v) = env_string(&[
            "PLOY_ACCOUNT__LABEL",
            "PLOY__ACCOUNT__LABEL",
            "PLOY_ACCOUNT_LABEL",
        ]) {
            if !v.trim().is_empty() {
                self.account.label = Some(v);
            }
        }

        if let Some(v) = env_string(&["PLOY_MARKET__MARKET_SLUG", "PLOY__MARKET__MARKET_SLUG"]) {
            self.market.market_slug = v;
        }

        if let Some(v) = env_string(&[
            "PLOY_EXECUTION__EXCHANGE",
            "PLOY__EXECUTION__EXCHANGE",
            "PLOY_EXECUTION_EXCHANGE",
        ]) {
            let normalized = v.trim().to_ascii_lowercase();
            if matches!(normalized.as_str(), "polymarket" | "kalshi") {
                self.execution.exchange = normalized;
            }
        }

        if let Some(v) = env_string_raw(&[
            "PLOY_KALSHI__BASE_URL",
            "PLOY__KALSHI__BASE_URL",
            "PLOY_KALSHI_BASE_URL",
            "KALSHI_BASE_URL",
        ]) {
            if !v.trim().is_empty() {
                self.kalshi.base_url = v;
            }
        }

        if let Some(v) = env_string_raw(&[
            "PLOY_KALSHI__API_KEY",
            "PLOY__KALSHI__API_KEY",
            "PLOY_KALSHI_API_KEY",
            "KALSHI_API_KEY",
            "KALSHI_ACCESS_KEY",
        ]) {
            if !v.trim().is_empty() {
                self.kalshi.api_key = Some(v);
            }
        }

        if let Some(v) = env_string_raw(&[
            "PLOY_KALSHI__API_SECRET",
            "PLOY__KALSHI__API_SECRET",
            "PLOY_KALSHI_API_SECRET",
            "KALSHI_API_SECRET",
            "KALSHI_ACCESS_SECRET",
        ]) {
            if !v.trim().is_empty() {
                self.kalshi.api_secret = Some(v);
            }
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

        if let Some(v) = env_string(&[
            "PLOY_AGENT_FRAMEWORK__MODE",
            "PLOY__AGENT_FRAMEWORK__MODE",
            "PLOY_AGENT_FRAMEWORK_MODE",
        ]) {
            let normalized = v.trim().to_ascii_lowercase();
            if matches!(normalized.as_str(), "internal" | "openclaw") {
                self.agent_framework.mode = normalized;
            }
        }

        if let Some(v) = env_bool(&[
            "PLOY_AGENT_FRAMEWORK__HARD_DISABLE_INTERNAL_AGENTS",
            "PLOY__AGENT_FRAMEWORK__HARD_DISABLE_INTERNAL_AGENTS",
            "PLOY_AGENT_FRAMEWORK_HARD_DISABLE_INTERNAL_AGENTS",
            "PLOY_OPENCLAW_ONLY",
        ]) {
            self.agent_framework.hard_disable_internal_agents = v;
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

    /// Whether built-in Rust agent loops must be disabled in this process.
    pub fn openclaw_runtime_lockdown(&self) -> bool {
        self.agent_framework.is_openclaw_mode() && self.agent_framework.hard_disable_internal_agents
    }
}

fn env_string(keys: &[&str]) -> Option<String> {
    env_string_raw(keys).map(|s| s.to_ascii_lowercase())
}

fn env_string_raw(keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Ok(v) = std::env::var(key) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
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
