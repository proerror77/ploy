use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::platform::Domain;

/// Timeframe for deployment / intent routing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Timeframe {
    #[serde(rename = "5m")]
    M5,
    #[serde(rename = "15m")]
    M15,
    Other(String),
}

impl Timeframe {
    pub fn as_str(&self) -> &str {
        match self {
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::Other(v) => v.as_str(),
        }
    }
}

/// Execution-mode scope for a deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentExecutionMode {
    /// Deployment can run in both dry-run and live mode.
    Any,
    /// Deployment is only eligible when runtime is dry-run.
    DryRunOnly,
    /// Deployment is only eligible when runtime is live.
    LiveOnly,
}

impl Default for DeploymentExecutionMode {
    fn default() -> Self {
        Self::Any
    }
}

/// Market selection policy for a deployment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum MarketSelector {
    /// Fixed target market (symbol/series/slug pinned by config).
    Static {
        symbol: Option<String>,
        series_id: Option<String>,
        market_slug: Option<String>,
    },
    /// Dynamic discovery from PM universe with entry filters.
    Dynamic {
        domain: Domain,
        query: Option<String>,
        min_liquidity_usd: Option<Decimal>,
        max_spread_bps: Option<u32>,
        min_time_remaining_secs: Option<u64>,
        max_time_remaining_secs: Option<u64>,
    },
}

/// Runtime deployment unit: strategy x market scope x risk/allocator profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyLifecycleStage {
    Backtest,
    Paper,
    Shadow,
    Live,
}

impl StrategyLifecycleStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Backtest => "backtest",
            Self::Paper => "paper",
            Self::Shadow => "shadow",
            Self::Live => "live",
        }
    }

    pub fn allows_live_ingress(&self) -> bool {
        matches!(self, Self::Live)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyProductType {
    BinaryOption,
    MultiOutcome,
    Scalar,
}

impl StrategyProductType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BinaryOption => "binary_option",
            Self::MultiOutcome => "multi_outcome",
            Self::Scalar => "scalar",
        }
    }
}

fn default_strategy_version() -> String {
    "v1".to_string()
}

fn default_lifecycle_stage() -> StrategyLifecycleStage {
    StrategyLifecycleStage::Live
}

fn default_strategy_product_type() -> StrategyProductType {
    StrategyProductType::BinaryOption
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyDeployment {
    pub id: String,
    pub strategy: String,
    #[serde(default = "default_strategy_version")]
    pub strategy_version: String,
    pub domain: Domain,
    pub market_selector: MarketSelector,
    pub timeframe: Timeframe,
    pub enabled: bool,
    pub allocator_profile: String,
    pub risk_profile: String,
    pub priority: i32,
    pub cooldown_secs: u64,
    /// Optional account scope allow-list.
    /// Empty list means "all accounts".
    #[serde(default)]
    pub account_ids: Vec<String>,
    /// Optional runtime execution-mode scope.
    #[serde(default)]
    pub execution_mode: DeploymentExecutionMode,
    #[serde(default = "default_lifecycle_stage")]
    pub lifecycle_stage: StrategyLifecycleStage,
    #[serde(default = "default_strategy_product_type")]
    pub product_type: StrategyProductType,
    #[serde(default)]
    pub last_evaluated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_evaluation_score: Option<f64>,
}

impl StrategyDeployment {
    pub fn normalize_account_ids_in_place(&mut self) {
        let mut normalized: Vec<String> = self
            .account_ids
            .iter()
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
            .collect();
        normalized.sort_by_key(|v| v.to_ascii_lowercase());
        normalized.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        self.account_ids = normalized;
    }

    pub fn matches_account(&self, account_id: &str) -> bool {
        let runtime_account = account_id.trim();
        if runtime_account.is_empty() || self.account_ids.is_empty() {
            return true;
        }
        self.account_ids
            .iter()
            .any(|v| v.eq_ignore_ascii_case(runtime_account))
    }

    pub fn matches_execution_mode(&self, dry_run: bool) -> bool {
        match self.execution_mode {
            DeploymentExecutionMode::Any => true,
            DeploymentExecutionMode::DryRunOnly => dry_run,
            DeploymentExecutionMode::LiveOnly => !dry_run,
        }
    }

    pub fn is_enabled_for_runtime(&self, account_id: &str, dry_run: bool) -> bool {
        self.enabled && self.matches_account(account_id) && self.matches_execution_mode(dry_run)
    }
}
