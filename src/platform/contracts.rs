//! Unified control-plane contracts for strategy deployment and execution flow.
//!
//! These types are intentionally transport-friendly (`serde`) and map cleanly
//! to the in-process `OrderIntent` used by the coordinator.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::domain::{OrderRequest, OrderStatus, Side};

use super::types::{Domain, OrderIntent, OrderPriority};

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

/// Evidence stage for strategy evaluation artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyEvaluationStage {
    Backtest,
    Paper,
    Live,
}

impl StrategyEvaluationStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Backtest => "backtest",
            Self::Paper => "paper",
            Self::Live => "live",
        }
    }
}

/// Quantitative summary for one evaluation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyEvaluationMetrics {
    pub sample_size: u64,
    pub win_rate: Option<f64>,
    pub pnl_usd: Option<f64>,
    pub max_drawdown_pct: Option<f64>,
    pub sharpe: Option<f64>,
    pub fill_rate: Option<f64>,
    pub avg_slippage_bps: Option<f64>,
}

/// Traceable strategy evaluation evidence used by control-plane governance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyEvaluationEvidence {
    pub evaluation_id: String,
    pub deployment_id: String,
    pub strategy: String,
    pub strategy_version: String,
    pub product_type: StrategyProductType,
    pub lifecycle_stage: StrategyLifecycleStage,
    pub stage: StrategyEvaluationStage,
    pub evaluated_at: DateTime<Utc>,
    pub evaluator: String,
    pub dataset_hash: String,
    pub model_hash: Option<String>,
    pub config_hash: Option<String>,
    pub run_id: Option<String>,
    pub artifact_uri: Option<String>,
    pub metrics: StrategyEvaluationMetrics,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Unified strategy output contract (agent -> coordinator).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeIntent {
    pub intent_id: Uuid,
    pub deployment_id: String,
    pub agent_id: String,
    pub domain: Domain,
    pub market_slug: String,
    pub token_id: String,
    /// Binary outcome side: YES/NO mapped to UP/DOWN internally.
    pub side: Side,
    /// `true` = buy/open, `false` = sell/close.
    pub is_buy: bool,
    pub size: u64,
    pub price_limit: Decimal,
    pub confidence: Option<Decimal>,
    pub edge: Option<Decimal>,
    pub event_time: Option<DateTime<Utc>>,
    pub reason: Option<String>,
    /// Optional priority hint (`critical|high|normal|low`).
    pub priority: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl TradeIntent {
    /// Convert control-plane intent to the coordinator queue type.
    pub fn into_order_intent(mut self) -> OrderIntent {
        if self
            .metadata
            .get("deployment_id")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            self.metadata
                .insert("deployment_id".to_string(), self.deployment_id.clone());
        }
        if self
            .metadata
            .get("intent_reason")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            if let Some(reason) = self.reason.clone() {
                self.metadata.insert("intent_reason".to_string(), reason);
            }
        }
        if self
            .metadata
            .get("signal_confidence")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            if let Some(confidence) = self.confidence {
                self.metadata.insert(
                    "signal_confidence".to_string(),
                    confidence.normalize().to_string(),
                );
            }
        }
        if self
            .metadata
            .get("signal_edge")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            if let Some(edge) = self.edge {
                self.metadata
                    .insert("signal_edge".to_string(), edge.normalize().to_string());
            }
        }
        if self
            .metadata
            .get("event_time")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            if let Some(ts) = self.event_time {
                self.metadata
                    .insert("event_time".to_string(), ts.to_rfc3339());
            }
        }

        let mut intent = OrderIntent::new(
            self.agent_id,
            self.domain,
            self.market_slug,
            self.token_id,
            self.side,
            self.is_buy,
            self.size,
            self.price_limit,
        );
        intent.priority = match self
            .priority
            .as_deref()
            .unwrap_or("normal")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "critical" => OrderPriority::Critical,
            "high" => OrderPriority::High,
            "low" => OrderPriority::Low,
            _ => OrderPriority::Normal,
        };
        intent.intent_id = self.intent_id;
        intent.metadata = self.metadata;
        intent
    }
}

/// Risk gate outcome for a `TradeIntent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskDecision {
    pub status: RiskDecisionStatus,
    pub reason_code: Option<String>,
    pub message: Option<String>,
    pub suggested_max_size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskDecisionStatus {
    Allow,
    Deny,
    Throttle,
}

/// Normalized command sent to the execution gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderCommand {
    pub intent_id: Uuid,
    pub deployment_id: String,
    pub idempotency_key: String,
    pub request: OrderRequest,
}

/// Normalized execution report emitted by the gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderExecutionReport {
    pub intent_id: Uuid,
    pub deployment_id: String,
    pub order_id: Option<String>,
    pub status: OrderStatus,
    pub filled_shares: u64,
    pub avg_fill_price: Option<Decimal>,
    pub error_message: Option<String>,
    pub executed_at: DateTime<Utc>,
    pub latency_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn trade_intent_into_order_intent_maps_priority_and_metadata() {
        let intent = TradeIntent {
            intent_id: Uuid::new_v4(),
            deployment_id: "deploy.crypto.15m".to_string(),
            agent_id: "openclaw-agent".to_string(),
            domain: Domain::Crypto,
            market_slug: "btc-updown-15m".to_string(),
            token_id: "token-yes".to_string(),
            side: Side::Up,
            is_buy: true,
            size: 10,
            price_limit: dec!(0.42),
            confidence: Some(dec!(0.73)),
            edge: Some(dec!(0.05)),
            event_time: None,
            reason: Some("signal_edge".to_string()),
            priority: Some("high".to_string()),
            metadata: HashMap::new(),
        };

        let mapped = intent.into_order_intent();
        assert_eq!(mapped.priority, OrderPriority::High);
        assert_eq!(mapped.deployment_id(), Some("deploy.crypto.15m"));
        assert_eq!(
            mapped.metadata.get("intent_reason").map(String::as_str),
            Some("signal_edge")
        );
    }

    #[test]
    fn deployment_runtime_scope_matching() {
        let mut deployment = StrategyDeployment {
            id: "dep.crypto.5m".to_string(),
            strategy: "momentum".to_string(),
            strategy_version: "v1".to_string(),
            domain: Domain::Crypto,
            market_selector: MarketSelector::Dynamic {
                domain: Domain::Crypto,
                query: Some("BTC 5m".to_string()),
                min_liquidity_usd: None,
                max_spread_bps: None,
                min_time_remaining_secs: None,
                max_time_remaining_secs: None,
            },
            timeframe: Timeframe::M5,
            enabled: true,
            allocator_profile: "default".to_string(),
            risk_profile: "default".to_string(),
            priority: 10,
            cooldown_secs: 30,
            account_ids: vec![" acct-a ".to_string(), "ACCT-A".to_string()],
            execution_mode: DeploymentExecutionMode::LiveOnly,
            lifecycle_stage: StrategyLifecycleStage::Live,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: None,
            last_evaluation_score: None,
        };

        deployment.normalize_account_ids_in_place();
        assert_eq!(deployment.account_ids.len(), 1);
        assert!(deployment.matches_account("acct-a"));
        assert!(!deployment.matches_account("acct-b"));
        assert!(deployment.matches_execution_mode(false));
        assert!(!deployment.matches_execution_mode(true));
        assert!(deployment.is_enabled_for_runtime("acct-a", false));
        assert!(!deployment.is_enabled_for_runtime("acct-a", true));
    }

    #[test]
    fn trade_intent_into_order_intent_normalizes_blank_deployment_metadata() {
        let mut intent = TradeIntent {
            intent_id: Uuid::new_v4(),
            deployment_id: "deploy.crypto.15m".to_string(),
            agent_id: "openclaw-agent".to_string(),
            domain: Domain::Crypto,
            market_slug: "btc-updown-15m".to_string(),
            token_id: "token-yes".to_string(),
            side: Side::Up,
            is_buy: true,
            size: 10,
            price_limit: dec!(0.42),
            confidence: None,
            edge: None,
            event_time: None,
            reason: None,
            priority: None,
            metadata: HashMap::new(),
        };
        intent
            .metadata
            .insert("deployment_id".to_string(), "   ".to_string());

        let mapped = intent.into_order_intent();
        assert_eq!(mapped.deployment_id(), Some("deploy.crypto.15m"));
    }
}
