<<<<<<<< HEAD:src/rl/integration/live_runtime.rs
//! RL live runtime helper for command-scoped execution.
//!
//! This retains the former RL crypto agent behavior without keeping the
//! retired `DomainAgent` / `platform::agents` compatibility layer alive.

use chrono::{DateTime, Datelike, Timelike, Utc};
========
//! RL-powered crypto agent used by the legacy RL CLI runtime.

use chrono::{DateTime, Utc};
>>>>>>>> origin/hotfix/staggered-arb-release-20260306:src/rl/cli_agent.rs
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::agent_runtime::AgentRiskParams;
use crate::domain::Side;
use crate::error::Result;
#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
<<<<<<<< HEAD:src/rl/integration/live_runtime.rs
use crate::platform::{
    AgentRiskParams, AgentStatus, CryptoEvent, Domain, ExecutionReport, OrderIntent,
    OrderPriority, QuoteUpdateEvent,
};
========
use crate::platform::OrderPriority;
>>>>>>>> origin/hotfix/staggered-arb-release-20260306:src/rl/cli_agent.rs
use crate::rl::config::RLConfig;
#[cfg(feature = "onnx")]
use crate::rl::core::{DefaultStateEncoder, TOTAL_FEATURES};
use crate::rl::core::{
    ContinuousAction, DiscreteAction, PnLRewardFunction, RawObservation, RewardFunction,
};
#[cfg(feature = "onnx")]
use crate::rl::{CONTINUOUS_ACTION_DIM, NUM_DISCRETE_ACTIONS};
use crate::rl::memory::ReplayBuffer;
use crate::rl::{CryptoEvent, DomainEvent, ExecutionReport};
use crate::{AgentStatus, Domain, OrderIntent};

mod policy;

fn default_policy_output() -> String {
    "continuous".to_string()
}

/// Runtime configuration for the standalone RL command path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RLCryptoRuntimeConfig {
    pub id: String,
    pub name: String,
    pub coins: Vec<String>,
    pub up_token_id: String,
    pub down_token_id: String,
    pub binance_symbol: String,
    pub market_slug: String,
    pub default_shares: u64,
    pub risk_params: AgentRiskParams,
    pub rl_config: RLConfig,
    pub online_learning: bool,
    pub exploration_rate: f32,
    #[serde(default)]
    pub policy_model_path: Option<String>,
    #[serde(default = "default_policy_output")]
    pub policy_output: String,
    #[serde(default)]
    pub policy_model_version: Option<String>,
}

impl Default for RLCryptoRuntimeConfig {
    fn default() -> Self {
        Self {
            id: "rl-crypto-runtime-1".to_string(),
            name: "RL Crypto Runtime".to_string(),
            coins: vec!["BTC".to_string()],
            up_token_id: String::new(),
            down_token_id: String::new(),
            binance_symbol: "BTCUSDT".to_string(),
            market_slug: String::new(),
            default_shares: 100,
            risk_params: AgentRiskParams::default(),
            rl_config: RLConfig::default(),
            online_learning: true,
            exploration_rate: 0.1,
            policy_model_path: None,
            policy_output: default_policy_output(),
            policy_model_version: None,
        }
    }
}

#[derive(Debug, Clone)]
struct InternalPosition {
    token_id: String,
    side: Side,
    shares: u64,
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    unrealized_pnl: Decimal,
}

<<<<<<<< HEAD:src/rl/integration/live_runtime.rs
/// Standalone RL live runtime used by CLI/research command paths.
pub struct RLCryptoRuntime {
    config: RLCryptoRuntimeConfig,
========
/// RL-Powered Crypto Agent
///
/// Uses reinforcement learning to make trading decisions for crypto markets.
/// The legacy RL CLI drives it directly instead of routing through a shared agent runtime.
pub struct RLCryptoAgent {
    config: RLCryptoAgentConfig,
>>>>>>>> origin/hotfix/staggered-arb-release-20260306:src/rl/cli_agent.rs
    status: AgentStatus,
    #[cfg(feature = "onnx")]
    encoder: Arc<DefaultStateEncoder>,
    reward_fn: Box<dyn RewardFunction + Send + Sync>,
    replay_buffer: Arc<RwLock<ReplayBuffer>>,
    current_obs: RawObservation,
    prev_obs: Option<RawObservation>,
    position: Option<InternalPosition>,
    daily_pnl: Decimal,
    total_exposure: Decimal,
    step_count: u64,
    last_action: Option<ContinuousAction>,
    last_action_source: Option<String>,
    exploration_rate: f32,
    consecutive_failures: u32,
    #[cfg(feature = "onnx")]
    policy_model: Option<OnnxModel>,
}

impl RLCryptoRuntime {
    pub fn new(config: RLCryptoRuntimeConfig) -> Self {
        info!("Creating RLCryptoRuntime: {} ({})", config.name, config.id);

        let buffer_size = config.rl_config.training.buffer_size;
        let exploration = config.exploration_rate;

        #[cfg(feature = "onnx")]
        let policy_model: Option<OnnxModel> = match config.policy_model_path.as_deref() {
            Some(path) if !path.trim().is_empty() => {
                match OnnxModel::load_for_vec_input(path, TOTAL_FEATURES) {
                    Ok(m) => {
                        info!(
                            runtime = %config.id,
                            policy_path = %path,
                            input_dim = m.input_dim(),
                            output_dim = m.output_dim(),
                            policy_output = %config.policy_output,
                            "loaded RL policy ONNX model"
                        );
                        Some(m)
                    }
                    Err(e) => {
                        warn!(
                            runtime = %config.id,
                            policy_path = %path,
                            error = %e,
                            "failed to load RL policy ONNX model; falling back to rule-based policy"
                        );
                        None
                    }
                }
            }
            _ => None,
        };

        #[cfg(not(feature = "onnx"))]
        if let Some(path) = config.policy_model_path.as_deref() {
            if !path.trim().is_empty() {
                warn!(
                    runtime = %config.id,
                    policy_path = %path,
                    "policy_model_path is set but binary is built without --features onnx; using rule-based policy"
                );
            }
        }

        Self {
            config,
            status: AgentStatus::Initializing,
            #[cfg(feature = "onnx")]
            encoder: Arc::new(DefaultStateEncoder::new()),
            reward_fn: Box::new(PnLRewardFunction::new()),
            replay_buffer: Arc::new(RwLock::new(ReplayBuffer::new(buffer_size))),
            current_obs: RawObservation::new(),
            prev_obs: None,
            position: None,
            daily_pnl: Decimal::ZERO,
            total_exposure: Decimal::ZERO,
            step_count: 0,
            last_action: None,
            last_action_source: None,
            exploration_rate: exploration,
            consecutive_failures: 0,
            #[cfg(feature = "onnx")]
            policy_model,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(RLCryptoRuntimeConfig::default())
    }

<<<<<<<< HEAD:src/rl/integration/live_runtime.rs
    pub fn id(&self) -> &str {
        &self.config.id
========
    /// Update observation from crypto event
    fn update_from_crypto_event(&mut self, event: &CryptoEvent) {
        // Update spot price
        self.current_obs.spot_price = Some(event.spot_price);

        // Update price history
        if self.current_obs.price_history.len() >= 15 {
            self.current_obs.price_history.remove(0);
        }
        self.current_obs.price_history.push(event.spot_price);

        // Update momentum features
        if let Some(momentum) = event.momentum {
            self.current_obs.momentum_1s = Some(Decimal::try_from(momentum[0]).unwrap_or_default());
            self.current_obs.momentum_5s = Some(Decimal::try_from(momentum[1]).unwrap_or_default());
            self.current_obs.momentum_15s =
                Some(Decimal::try_from(momentum[2]).unwrap_or_default());
            self.current_obs.momentum_60s =
                Some(Decimal::try_from(momentum[3]).unwrap_or_default());
        }

        // Update quotes
        if let Some(quotes) = &event.quotes {
            self.current_obs.up_bid = Some(quotes.up_bid);
            self.current_obs.up_ask = Some(quotes.up_ask);
            self.current_obs.down_bid = Some(quotes.down_bid);
            self.current_obs.down_ask = Some(quotes.down_ask);
            self.current_obs.calculate_spreads();
            self.current_obs.calculate_sum_of_asks();
        }

        // Update time features
        let now = Utc::now();
        self.current_obs
            .update_time_features(now.hour(), now.weekday().num_days_from_monday());

        // Update position features
        self.update_position_features();
>>>>>>>> origin/hotfix/staggered-arb-release-20260306:src/rl/cli_agent.rs
    }

    pub fn status(&self) -> AgentStatus {
        self.status
    }

<<<<<<<< HEAD:src/rl/integration/live_runtime.rs
    pub fn position_count(&self) -> usize {
        usize::from(self.position.is_some())
    }

    pub fn total_exposure(&self) -> Decimal {
        self.total_exposure
    }

    pub fn daily_pnl(&self) -> Decimal {
        self.daily_pnl
    }

    pub async fn start(&mut self) -> Result<()> {
        info!("[{}] Starting RL runtime...", self.config.id);
        self.status = AgentStatus::Running;
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        info!("[{}] Stopping RL runtime...", self.config.id);
        self.status = AgentStatus::Stopped;
        Ok(())
    }

    pub fn pause(&mut self) {
        self.status = AgentStatus::Paused;
    }

    pub fn resume(&mut self) {
        self.consecutive_failures = 0;
        self.status = AgentStatus::Running;
    }

    pub fn on_crypto_event(&mut self, event: &CryptoEvent) -> Vec<OrderIntent> {
        if !self.status.can_trade() {
            return vec![];
        }

========
    /// Decay exploration rate
    fn decay_exploration(&mut self) {
        let decay = self.config.rl_config.training.exploration_decay;
        let min = self.config.rl_config.training.exploration_min;
        self.exploration_rate = (self.exploration_rate * decay).max(min);
    }

    /// Process crypto event and generate intents
    fn process_crypto_event(&mut self, event: &CryptoEvent) -> Vec<OrderIntent> {
        // Check if this is a coin we're monitoring
>>>>>>>> origin/hotfix/staggered-arb-release-20260306:src/rl/cli_agent.rs
        let coin = event.symbol.replace("USDT", "");
        if !self.config.coins.iter().any(|c| c == &coin) {
            return vec![];
        }

        if !self.config.market_slug.is_empty() {
            if let Some(slug) = &event.round_slug {
                if slug != &self.config.market_slug {
                    return vec![];
                }
            }
        }

        self.prev_obs = Some(self.current_obs.clone());
        self.update_from_crypto_event(event);
        self.step_count += 1;

        let action = self.select_action();
        let intents = self.action_to_intents(action);

        if self.online_learning() {
            let reward_signal = self.reward_fn.compute(&self.compute_reward_transition());
            debug!(
                "[{}] step={} reward={:.4} exploration={:.4}",
                self.config.id, self.step_count, reward_signal.total, self.exploration_rate
            );
            let _ = &self.replay_buffer;
        }

        if !intents.is_empty() {
            debug!(
                "[{}] Step {}: generated {} intents",
                self.config.id,
                self.step_count,
                intents.len()
            );
        }

        intents
    }

    pub fn on_quote_update(&mut self, update: &QuoteUpdateEvent) {
        if update.domain != Domain::Crypto {
            return;
        }

        match update.side {
            Side::Up => {
                self.current_obs.up_bid = Some(update.bid);
                self.current_obs.up_ask = Some(update.ask);
            }
            Side::Down => {
                self.current_obs.down_bid = Some(update.bid);
                self.current_obs.down_ask = Some(update.ask);
            }
        }
        self.current_obs.calculate_spreads();
        self.current_obs.calculate_sum_of_asks();
        self.update_position_prices();
        self.update_position_features();
    }

    pub fn on_tick(&mut self, now: DateTime<Utc>) {
        self.current_obs
            .update_time_features(now.hour(), now.weekday().num_days_from_monday());
        self.update_position_prices();
        self.update_position_features();
    }

    pub fn on_execution(&mut self, report: &ExecutionReport) {
        if matches!(
            report.status,
            crate::platform::ExecutionStatus::Pending | crate::platform::ExecutionStatus::Submitted
        ) {
            return;
        }

        if report.is_success() {
            self.consecutive_failures = 0;

            if let Some(avg_price) = report.avg_fill_price {
                if self.position.is_some() {
                    if let Some(pos) = &self.position {
                        let realized =
                            (avg_price - pos.entry_price) * Decimal::from(report.filled_shares);
                        self.daily_pnl += realized;
                    }
                    self.position = None;
                    self.update_exposure();
                } else {
                    let side = if self
                        .last_action
                        .map(|a| a.side_preference > 0.0)
                        .unwrap_or(true)
                    {
                        Side::Up
                    } else {
                        Side::Down
                    };

                    let token_id = match side {
                        Side::Up => self.config.up_token_id.clone(),
                        Side::Down => self.config.down_token_id.clone(),
                    };

                    self.position = Some(InternalPosition {
                        token_id,
                        side,
                        shares: report.filled_shares,
                        entry_price: avg_price,
                        entry_time: Utc::now(),
                        unrealized_pnl: Decimal::ZERO,
                    });
                    self.update_exposure();
                }
            }

            self.decay_exploration();
        } else {
            self.consecutive_failures += 1;
            warn!(
                "[{}] execution failed: {:?} (consecutive={})",
                self.config.id, report.error_message, self.consecutive_failures
            );
            if self.consecutive_failures >= 3 {
                self.status = AgentStatus::Paused;
            }
        }
    }

    fn online_learning(&self) -> bool {
        self.config.online_learning
    }

    fn update_from_crypto_event(&mut self, event: &CryptoEvent) {
        self.current_obs.spot_price = Some(event.spot_price);
        if self.current_obs.price_history.len() >= 15 {
            self.current_obs.price_history.remove(0);
        }
        self.current_obs.price_history.push(event.spot_price);

        if let Some(momentum) = event.momentum {
            self.current_obs.momentum_1s = Some(Decimal::try_from(momentum[0]).unwrap_or_default());
            self.current_obs.momentum_5s = Some(Decimal::try_from(momentum[1]).unwrap_or_default());
            self.current_obs.momentum_15s =
                Some(Decimal::try_from(momentum[2]).unwrap_or_default());
            self.current_obs.momentum_60s =
                Some(Decimal::try_from(momentum[3]).unwrap_or_default());
        }

        if let Some(quotes) = &event.quotes {
            self.current_obs.up_bid = Some(quotes.up_bid);
            self.current_obs.up_ask = Some(quotes.up_ask);
            self.current_obs.down_bid = Some(quotes.down_bid);
            self.current_obs.down_ask = Some(quotes.down_ask);
            self.current_obs.calculate_spreads();
            self.current_obs.calculate_sum_of_asks();
        }

        let now = Utc::now();
        self.current_obs
            .update_time_features(now.hour(), now.weekday().num_days_from_monday());
        self.update_position_features();
    }

    fn update_position_features(&mut self) {
        if let Some(pos) = &self.position {
            self.current_obs.has_position = true;
            self.current_obs.position_side = Some(pos.side);
            self.current_obs.position_shares = pos.shares;
            self.current_obs.entry_price = Some(pos.entry_price);
            self.current_obs.unrealized_pnl = Some(pos.unrealized_pnl);
            self.current_obs.position_duration_secs =
                Some((Utc::now() - pos.entry_time).num_seconds());
        } else {
            self.current_obs.has_position = false;
            self.current_obs.position_side = None;
            self.current_obs.position_shares = 0;
            self.current_obs.entry_price = None;
            self.current_obs.unrealized_pnl = None;
            self.current_obs.position_duration_secs = None;
        }
    }

    fn update_position_prices(&mut self) {
        if let Some(pos) = &mut self.position {
            let current_price = match pos.side {
                Side::Up => self.current_obs.up_bid,
                Side::Down => self.current_obs.down_bid,
            };
            if let Some(price) = current_price {
                pos.unrealized_pnl = (price - pos.entry_price) * Decimal::from(pos.shares);
            }
        }
    }

    fn update_exposure(&mut self) {
        self.total_exposure = self
            .position
            .as_ref()
            .map(|p| p.entry_price * Decimal::from(p.shares))
            .unwrap_or(Decimal::ZERO);
    }

    fn compute_reward_transition(&self) -> crate::rl::RewardTransition {
        let mut transition = crate::rl::RewardTransition::default();
        if let (Some(prev), Some(curr)) = (
            self.prev_obs.as_ref().and_then(|o| o.unrealized_pnl),
            self.current_obs.unrealized_pnl,
        ) {
            transition.unrealized_pnl_delta = Some(curr - prev);
        }
        if self.current_obs.has_position
            && !self
                .prev_obs
                .as_ref()
                .map(|o| o.has_position)
                .unwrap_or(false)
        {
            transition.sum_of_asks_at_entry = self.current_obs.sum_of_asks;
        }
        transition.risk_exposure = self
            .current_obs
            .exposure_pct
            .to_string()
            .parse()
            .unwrap_or(0.0);
        transition
    }

    fn select_action(&mut self) -> ContinuousAction {
        let mut action = self.rule_based_policy();
        let mut source = "rule_based";

        #[cfg(feature = "onnx")]
        if let Some(model) = &self.policy_model {
            let state_vec = self.encoder.encode(&self.current_obs);
            match model.predict(&state_vec) {
                Ok(out) => match self.action_from_policy_output(&out) {
                    Some(a) => {
                        action = a;
                        source = "onnx";
                    }
                    None => {
                        warn!(
                            runtime = %self.config.id,
                            output_dim = out.len(),
                            policy_output = %self.config.policy_output,
                            "RL ONNX policy output could not be interpreted; keeping rule-based policy"
                        );
                    }
                },
                Err(e) => {
                    warn!(
                        runtime = %self.config.id,
                        error = %e,
                        "RL ONNX policy inference failed; keeping rule-based policy"
                    );
                }
            }
        }

<<<<<<<< HEAD:src/rl/integration/live_runtime.rs
        if rand::random::<f32>() < self.exploration_rate {
            action = ContinuousAction::new(
                rand::random::<f32>() * 2.0 - 1.0,
                rand::random::<f32>() * 2.0 - 1.0,
                rand::random::<f32>(),
                0.0,
                0.0,
            );
            source = "explore";
        }

        self.last_action = Some(action);
        self.last_action_source = Some(source.to_string());
        action
    }

    #[cfg(feature = "onnx")]
    fn map_urgency(raw: f32) -> f32 {
        if !raw.is_finite() {
            return 0.5;
        }
        if (0.0..=1.0).contains(&raw) {
            return raw;
        }
        if (-1.0..=1.0).contains(&raw) {
            return (raw + 1.0) * 0.5;
        }
        1.0 / (1.0 + (-raw).exp())
    }

    #[cfg(feature = "onnx")]
    fn action_from_discrete(action: DiscreteAction) -> ContinuousAction {
        match action {
            DiscreteAction::Hold => ContinuousAction::default(),
            DiscreteAction::BuyUp => ContinuousAction::new(0.8, 1.0, 0.5, 0.0, 0.0),
            DiscreteAction::BuyDown => ContinuousAction::new(0.8, -1.0, 0.5, 0.0, 0.0),
            DiscreteAction::SellPosition => ContinuousAction::new(-0.8, 0.0, 0.8, 0.0, 0.0),
            DiscreteAction::EnterHedge => ContinuousAction::new(0.8, 0.0, 0.6, 0.0, 0.0),
        }
    }

    #[cfg(feature = "onnx")]
    fn argmax(values: &[f32]) -> Option<usize> {
        if values.is_empty() {
            return None;
        }
        let mut best_idx = 0usize;
        let mut best_val = values[0];
        for (i, &v) in values.iter().enumerate().skip(1) {
            if v > best_val {
                best_val = v;
                best_idx = i;
            }
        }
        Some(best_idx)
    }

    #[cfg(feature = "onnx")]
    fn softmax(values: &[f32]) -> Vec<f32> {
        if values.is_empty() {
            return Vec::new();
        }
        let max = values
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .fold(f32::NEG_INFINITY, f32::max);
        if !max.is_finite() {
            return vec![0.0; values.len()];
        }
        let mut exps = Vec::with_capacity(values.len());
        let mut sum = 0.0f32;
        for &v in values {
            let x = if v.is_finite() { (v - max).exp() } else { 0.0 };
            exps.push(x);
            sum += x;
        }
        if sum <= 0.0 {
            return vec![0.0; values.len()];
        }
        exps.into_iter().map(|v| v / sum).collect()
    }

    #[cfg(feature = "onnx")]
    fn action_from_policy_output(&self, output: &[f32]) -> Option<ContinuousAction> {
        match self.config.policy_output.trim().to_ascii_lowercase().as_str() {
            "continuous" => {
                if output.len() < CONTINUOUS_ACTION_DIM {
                    return None;
                }
                let v = &output[..CONTINUOUS_ACTION_DIM];
                Some(ContinuousAction::new(v[0], v[1], Self::map_urgency(v[2]), v[3], v[4]))
            }
            "continuous_mean_logstd" | "mean_logstd" => {
                if output.len() < CONTINUOUS_ACTION_DIM * 2 {
                    return None;
                }
                let mean = &output[..CONTINUOUS_ACTION_DIM];
                Some(ContinuousAction::new(
                    mean[0].tanh(),
                    mean[1].tanh(),
                    Self::map_urgency(mean[2]),
                    mean[3].tanh(),
                    mean[4].tanh(),
                ))
            }
            "discrete_logits" | "discrete" => {
                if output.len() < NUM_DISCRETE_ACTIONS {
                    return None;
                }
                let idx = Self::argmax(&Self::softmax(&output[..NUM_DISCRETE_ACTIONS]))?;
                Some(Self::action_from_discrete(DiscreteAction::from_index(idx)?))
            }
            "discrete_probs" => {
                if output.len() < NUM_DISCRETE_ACTIONS {
                    return None;
                }
                let idx = Self::argmax(&output[..NUM_DISCRETE_ACTIONS])?;
                Some(Self::action_from_discrete(DiscreteAction::from_index(idx)?))
            }
            _ => None,
        }
    }

    fn rule_based_policy(&self) -> ContinuousAction {
        if let Some(sum) = self.current_obs.sum_of_asks {
            let sum_f32: f32 = sum.to_string().parse().unwrap_or(1.0);
            if sum_f32 < 0.96 && !self.current_obs.has_position {
                let side_pref = match self.current_obs.momentum_1s {
                    Some(m) if m > Decimal::ZERO => 0.5,
                    Some(m) if m < Decimal::ZERO => -0.5,
                    _ => 0.0,
                };
                return ContinuousAction::new(0.7, side_pref, 0.5, 0.0, 0.0);
            }
            if sum_f32 > 1.0 && self.current_obs.has_position {
                return ContinuousAction::new(-0.8, 0.0, 0.7, 0.0, 0.0);
            }
            if let Some(pnl) = self.current_obs.unrealized_pnl {
                let pnl_f32: f32 = pnl.to_string().parse().unwrap_or(0.0);
                if pnl_f32 < -0.05 && self.current_obs.has_position {
                    return ContinuousAction::new(-1.0, 0.0, 1.0, 0.0, 0.0);
                }
            }
        }
        ContinuousAction::default()
    }

    fn deployment_id(&self) -> String {
        let market_slug = self.config.market_slug.trim().to_ascii_lowercase();
        if market_slug.is_empty() {
            "crypto.pm.rl_crypto".to_string()
        } else {
            format!("crypto.pm.rl_crypto.{}", market_slug)
        }
    }

    fn action_to_intents(&self, action: ContinuousAction) -> Vec<OrderIntent> {
        let discrete = action.to_discrete();
        let policy_source = self.last_action_source.as_deref().unwrap_or("unknown");
        let policy_version = self.config.policy_model_version.as_deref().unwrap_or("");
        let deployment_id = self.deployment_id();
        let mut intents = Vec::new();

        match discrete {
            DiscreteAction::Hold => {}
            DiscreteAction::BuyUp => {
                if let Some(ask) = self.current_obs.up_ask {
                    intents.push(
                        OrderIntent::new(
                            &self.config.id,
                            Domain::Crypto,
                            &self.config.market_slug,
                            &self.config.up_token_id,
                            Side::Up,
                            true,
                            self.calculate_shares(&action),
                            ask,
                        )
                        .with_priority(if action.is_aggressive() {
                            OrderPriority::High
                        } else {
                            OrderPriority::Normal
                        })
                        .with_metadata("strategy", "rl_crypto")
                        .with_deployment_id(deployment_id.as_str())
                        .with_metadata("action", "buy_up")
                        .with_metadata("step", &self.step_count.to_string())
                        .with_metadata("policy_source", policy_source)
                        .with_metadata("policy_model_version", policy_version),
                    );
                }
            }
            DiscreteAction::BuyDown => {
                if let Some(ask) = self.current_obs.down_ask {
                    intents.push(
                        OrderIntent::new(
                            &self.config.id,
                            Domain::Crypto,
                            &self.config.market_slug,
                            &self.config.down_token_id,
                            Side::Down,
                            true,
                            self.calculate_shares(&action),
                            ask,
                        )
                        .with_priority(if action.is_aggressive() {
                            OrderPriority::High
                        } else {
                            OrderPriority::Normal
                        })
                        .with_metadata("strategy", "rl_crypto")
                        .with_deployment_id(deployment_id.as_str())
                        .with_metadata("action", "buy_down")
                        .with_metadata("step", &self.step_count.to_string())
                        .with_metadata("policy_source", policy_source)
                        .with_metadata("policy_model_version", policy_version),
                    );
                }
            }
            DiscreteAction::SellPosition => {
                if let Some(pos) = &self.position {
                    let bid = match pos.side {
                        Side::Up => self.current_obs.up_bid,
                        Side::Down => self.current_obs.down_bid,
                    };
                    if let Some(bid) = bid {
                        intents.push(
                            OrderIntent::new(
                                &self.config.id,
                                Domain::Crypto,
                                &self.config.market_slug,
                                &pos.token_id,
                                pos.side,
                                false,
                                pos.shares,
                                bid,
                            )
                            .with_priority(OrderPriority::High)
                            .with_metadata("strategy", "rl_crypto")
                            .with_deployment_id(deployment_id.as_str())
                            .with_metadata("action", "sell")
                            .with_metadata("exit_reason", "rl_signal")
                            .with_metadata("policy_source", policy_source)
                            .with_metadata("policy_model_version", policy_version),
                        );
                    }
                }
            }
            DiscreteAction::EnterHedge => {
                if let Some(pos) = &self.position {
                    let (other_side, other_token, other_ask) = match pos.side {
                        Side::Up => (
                            Side::Down,
                            &self.config.down_token_id,
                            self.current_obs.down_ask,
                        ),
                        Side::Down => (Side::Up, &self.config.up_token_id, self.current_obs.up_ask),
                    };
                    if let Some(ask) = other_ask {
                        let total_cost = pos.entry_price + ask;
                        if total_cost < dec!(1.0) {
                            intents.push(
                                OrderIntent::new(
                                    &self.config.id,
                                    Domain::Crypto,
                                    &self.config.market_slug,
                                    other_token,
                                    other_side,
                                    true,
                                    pos.shares,
                                    ask,
                                )
                                .with_priority(OrderPriority::High)
                                .with_metadata("strategy", "rl_crypto")
                                .with_deployment_id(deployment_id.as_str())
                                .with_metadata("action", "hedge")
                                .with_metadata(
                                    "locked_profit",
                                    &(dec!(1.0) - total_cost).to_string(),
                                )
                                .with_metadata("policy_source", policy_source)
                                .with_metadata("policy_model_version", policy_version),
                            );
                        }
                    }
                }
========
use chrono::Datelike;
use chrono::Timelike;

impl RLCryptoAgent {
    pub fn id(&self) -> &str {
        &self.config.id
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn domain(&self) -> Domain {
        Domain::Crypto
    }

    pub fn status(&self) -> AgentStatus {
        self.status
    }

    pub fn risk_params(&self) -> &AgentRiskParams {
        &self.config.risk_params
    }

    pub async fn on_event(&mut self, event: DomainEvent) -> Result<Vec<OrderIntent>> {
        if !self.status.can_trade() {
            return Ok(vec![]);
        }

        match event {
            DomainEvent::Crypto(crypto_event) => Ok(self.process_crypto_event(&crypto_event)),
            DomainEvent::Tick(now) => {
                self.current_obs
                    .update_time_features(now.hour(), now.weekday().num_days_from_monday());
                self.update_position_prices();
                self.update_position_features();
                Ok(vec![])
>>>>>>>> origin/hotfix/staggered-arb-release-20260306:src/rl/cli_agent.rs
            }
        }

        intents
    }

<<<<<<<< HEAD:src/rl/integration/live_runtime.rs
    fn calculate_shares(&self, action: &ContinuousAction) -> u64 {
        let base = self.config.default_shares;
        let multiplier = action.position_size_pct();
        ((base as f32) * multiplier).max(1.0) as u64
    }

    fn decay_exploration(&mut self) {
        let decay = self.config.rl_config.training.exploration_decay;
        let min = self.config.rl_config.training.exploration_min;
        self.exploration_rate = (self.exploration_rate * decay).max(min);
========
    pub async fn on_execution(&mut self, report: ExecutionReport) {
        self.handle_execution(&report);
    }

    pub async fn start(&mut self) -> Result<()> {
        info!("[{}] Starting RL Crypto Agent...", self.config.id);
        self.status = AgentStatus::Running;
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        info!("[{}] Stopping RL Crypto Agent...", self.config.id);
        self.status = AgentStatus::Stopped;
        Ok(())
    }

    pub fn pause(&mut self) {
        info!("[{}] Pausing...", self.config.id);
        self.status = AgentStatus::Paused;
    }

    pub fn resume(&mut self) {
        info!("[{}] Resuming...", self.config.id);
        self.consecutive_failures = 0;
        self.status = AgentStatus::Running;
    }

    pub fn position_count(&self) -> usize {
        if self.position.is_some() {
            1
        } else {
            0
        }
    }

    pub fn total_exposure(&self) -> Decimal {
        self.total_exposure
    }

    pub fn daily_pnl(&self) -> Decimal {
        self.daily_pnl
>>>>>>>> origin/hotfix/staggered-arb-release-20260306:src/rl/cli_agent.rs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
<<<<<<<< HEAD:src/rl/integration/live_runtime.rs
    use crate::platform::{CryptoEvent, ExecutionStatus, QuoteData};
========
    use crate::rl::{ExecutionStatus, QuoteData};
>>>>>>>> origin/hotfix/staggered-arb-release-20260306:src/rl/cli_agent.rs

    fn make_crypto_event(
        symbol: &str,
        spot: Decimal,
        up_ask: Decimal,
        down_ask: Decimal,
    ) -> CryptoEvent {
        CryptoEvent {
            symbol: symbol.to_string(),
            spot_price: spot,
            round_slug: None,
            quotes: Some(QuoteData {
                up_bid: up_ask - dec!(0.01),
                up_ask,
                down_bid: down_ask - dec!(0.01),
                down_ask,
                timestamp: Utc::now(),
            }),
            momentum: Some([0.002, 0.001, 0.0005, 0.0001]),
        }
    }

    #[tokio::test]
    async fn rl_runtime_lifecycle() {
        let mut runtime = RLCryptoRuntime::with_defaults();
        assert_eq!(runtime.status(), AgentStatus::Initializing);
        runtime.start().await.unwrap();
        assert_eq!(runtime.status(), AgentStatus::Running);
        runtime.pause();
        assert_eq!(runtime.status(), AgentStatus::Paused);
        runtime.resume();
        assert_eq!(runtime.status(), AgentStatus::Running);
        runtime.stop().await.unwrap();
        assert_eq!(runtime.status(), AgentStatus::Stopped);
    }

    #[tokio::test]
    async fn rl_runtime_generates_intent_on_good_sum() {
        let mut runtime = RLCryptoRuntime::new(RLCryptoRuntimeConfig {
            coins: vec!["BTC".to_string()],
            exploration_rate: 0.0,
            ..Default::default()
        });
        runtime.start().await.unwrap();

        let intents = runtime.on_crypto_event(&make_crypto_event(
            "BTCUSDT",
            dec!(50000),
            dec!(0.47),
            dec!(0.48),
        ));

        assert!(!intents.is_empty());
        assert!(intents[0].is_buy);
        assert_eq!(intents[0].domain, Domain::Crypto);
    }

    #[tokio::test]
    async fn rl_runtime_suppresses_high_sum_signal() {
        let mut runtime = RLCryptoRuntime::new(RLCryptoRuntimeConfig {
            coins: vec!["BTC".to_string()],
            exploration_rate: 0.0,
            ..Default::default()
        });
        runtime.start().await.unwrap();

        let intents = runtime.on_crypto_event(&make_crypto_event(
            "BTCUSDT",
            dec!(50000),
            dec!(0.50),
            dec!(0.50),
        ));

        assert!(intents.is_empty());
    }

    #[tokio::test]
    async fn rl_runtime_tracks_position_after_fill() {
        let mut runtime = RLCryptoRuntime::new(RLCryptoRuntimeConfig {
            up_token_id: "up-token".to_string(),
            down_token_id: "down-token".to_string(),
            ..Default::default()
        });
        runtime.start().await.unwrap();

        let report = ExecutionReport {
            intent_id: uuid::Uuid::new_v4(),
            agent_id: runtime.id().to_string(),
            order_id: Some("order-1".to_string()),
            status: ExecutionStatus::Filled,
            filled_shares: 100,
            avg_fill_price: Some(dec!(0.50)),
            fees: Decimal::ZERO,
            error_message: None,
            executed_at: Utc::now(),
            latency_ms: 50,
        };

        runtime.on_execution(&report);

        assert_eq!(runtime.position_count(), 1);
        assert!(runtime.total_exposure() > Decimal::ZERO);
    }

    #[tokio::test]
    async fn rl_runtime_ignores_submitted_without_fill() {
        let mut runtime = RLCryptoRuntime::new(RLCryptoRuntimeConfig {
            up_token_id: "up-token".to_string(),
            down_token_id: "down-token".to_string(),
            ..Default::default()
        });
        runtime.start().await.unwrap();

        let report = ExecutionReport {
            intent_id: uuid::Uuid::new_v4(),
            agent_id: runtime.id().to_string(),
            order_id: Some("order-1".to_string()),
            status: ExecutionStatus::Submitted,
            filled_shares: 0,
            avg_fill_price: Some(dec!(0.50)),
            fees: Decimal::ZERO,
            error_message: None,
            executed_at: Utc::now(),
            latency_ms: 50,
        };

        runtime.on_execution(&report);

        assert_eq!(runtime.position_count(), 0);
        assert_eq!(runtime.total_exposure(), Decimal::ZERO);
        assert_eq!(runtime.status(), AgentStatus::Running);
    }
}
