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
    /// sum_target_eff = 1 - fee_buffer - slippage_buffer - profit_buffer
    pub fn effective_sum_target(&self) -> Decimal {
        Decimal::ONE - self.fee_buffer - self.slippage_buffer - self.profit_buffer
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
}

fn default_poll_interval() -> u64 {
    500
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            order_timeout_ms: 5000,
            max_retries: 3,
            max_spread_bps: 500,
            poll_interval_ms: 500,
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

    /// Load configuration from a specific directory
    pub fn load_from<P: AsRef<Path>>(config_dir: P) -> Result<Self, ConfigError> {
        let config_dir = config_dir.as_ref();

        let builder = Config::builder()
            // Start with default values
            .set_default("logging.level", "info")?
            .set_default("logging.json", false)?
            .set_default("execution.poll_interval_ms", 500)?
            .set_default("database.max_connections", 5)?
            // Load default config file
            .add_source(File::from(config_dir.join("default.toml")).required(false))
            // Load environment-specific config (e.g., config/production.toml)
            .add_source(
                File::from(config_dir.join(
                    std::env::var("PLOY_ENV").unwrap_or_else(|_| "development".to_string()),
                ))
                .required(false),
            )
            // Override with environment variables (PLOY_MARKET__WS_URL, etc.)
            .add_source(
                Environment::with_prefix("PLOY")
                    .separator("__")
                    .try_parsing(true),
            );

        builder.build()?.try_deserialize()
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
                sum_target: dec!(0.95),
                fee_buffer: dec!(0.005),
                slippage_buffer: dec!(0.02),
                profit_buffer: dec!(0.01),
            },
            execution: ExecutionConfig {
                order_timeout_ms: 5000,
                max_retries: 3,
                max_spread_bps: 500,
                poll_interval_ms: 500,
            },
            risk: RiskConfig {
                max_single_exposure_usd: dec!(100),
                min_remaining_seconds: 30,
                max_consecutive_failures: 3,
                daily_loss_limit_usd: dec!(500),
                leg2_force_close_seconds: 20,
                // Fund management defaults
                max_positions: 3,              // Max 3 concurrent positions
                max_positions_per_symbol: 1,   // Only 1 position per symbol
                position_size_pct: None,       // Not using percentage-based sizing
                fixed_amount_usd: Some(dec!(1)), // $1 per trade
                min_balance_usd: dec!(2),      // Keep $2 minimum balance
            },
            database: DatabaseConfig {
                url: "postgres://localhost/ploy".to_string(),
                max_connections: 5,
            },
            dry_run: DryRunConfig { enabled: dry_run },
            logging: LoggingConfig::default(),
            health_port: Some(8080),
        }
    }

    /// Validate configuration values
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // Validate strategy params
        if self.strategy.move_pct <= Decimal::ZERO || self.strategy.move_pct >= Decimal::ONE {
            errors.push("move_pct must be between 0 and 1".to_string());
        }

        if self.strategy.sum_target <= Decimal::ZERO || self.strategy.sum_target >= Decimal::ONE {
            errors.push("sum_target must be between 0 and 1".to_string());
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

        // 1 - 0.005 - 0.02 - 0.01 = 0.965
        assert_eq!(strategy.effective_sum_target(), dec!(0.965));
    }
}
