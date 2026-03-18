use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::agent_runtime::AgentRiskParams;
use crate::coordinator::OrderPriority;
use crate::domain::Side;
use crate::error::Result;
#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
#[cfg(feature = "onnx")]
use crate::rl::core::TOTAL_FEATURES;
use crate::rl::core::{
    ContinuousAction, DefaultStateEncoder, DiscreteAction, PnLRewardFunction, RawObservation,
    RewardFunction, CONTINUOUS_ACTION_DIM, NUM_DISCRETE_ACTIONS,
};
use crate::rl::memory::ReplayBuffer;
use crate::rl::{CryptoEvent, DomainEvent, ExecutionReport};
use crate::{AgentStatus, Domain, OrderIntent};

mod agent_core;
mod config;
mod execution_feedback;
mod execution_outcomes;
mod intent_mapping;
mod market_state;
mod policy;
mod policy_output;
mod position_state;
mod runtime;
#[cfg(test)]
mod tests;

use agent_core::InternalPosition;
pub use agent_core::RLCryptoAgent;
pub use config::RLCryptoAgentConfig;
