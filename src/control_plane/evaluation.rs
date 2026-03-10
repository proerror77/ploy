use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{StrategyLifecycleStage, StrategyProductType};

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
