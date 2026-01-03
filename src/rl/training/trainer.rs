//! Training Loop
//!
//! Orchestrates the RL training process.
//!
//! Note: The full implementation requires proper burn tensor type handling.
//! This module provides the structure and interface; the actual tensor
//! operations need refinement based on the specific burn backend.

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use crate::rl::config::TrainingConfig;
use crate::rl::core::{
    DefaultStateEncoder, RawObservation, RewardFunction, PnLRewardFunction, RewardTransition,
    StateEncoder, ContinuousAction,
};
use crate::rl::memory::{RolloutBuffer, Transition};
use crate::rl::environment::{
    TradingEnvironment, TradingEnvConfig, EnvAction,
    BacktestEnvironment, HistoricalData, generate_sample_data,
};
use crate::rl::algorithms::ppo::{PPOTrainer, PPOTrainerConfig, PPOBatch};

/// Training statistics
#[derive(Debug, Clone, Default)]
pub struct TrainingStats {
    /// Total episodes completed
    pub episodes: usize,
    /// Total steps taken
    pub steps: usize,
    /// Total updates performed
    pub updates: usize,
    /// Average episode reward
    pub avg_episode_reward: f32,
    /// Average episode length
    pub avg_episode_length: f32,
    /// Latest policy loss
    pub policy_loss: f32,
    /// Latest value loss
    pub value_loss: f32,
    /// Current exploration rate
    pub exploration_rate: f32,
}

/// Training loop for RL agents
///
/// This provides the training infrastructure. The actual neural network
/// operations are handled by the PPOTrainer which uses burn tensors.
pub struct TrainingLoop {
    /// State encoder
    encoder: Arc<DefaultStateEncoder>,
    /// Reward function
    reward_fn: Box<dyn RewardFunction>,
    /// Rollout buffer
    rollout_buffer: RolloutBuffer,
    /// Training configuration
    config: TrainingConfig,
    /// Training statistics
    stats: TrainingStats,
    /// Whether in training mode
    training: bool,
}

impl TrainingLoop {
    /// Create a new training loop
    pub fn new(config: TrainingConfig) -> Self {
        Self {
            encoder: Arc::new(DefaultStateEncoder::new()),
            reward_fn: Box::new(PnLRewardFunction::new()),
            rollout_buffer: RolloutBuffer::new(config.update_frequency),
            config,
            stats: TrainingStats::default(),
            training: true,
        }
    }

    /// Set training mode
    pub fn set_training(&mut self, training: bool) {
        self.training = training;
    }

    /// Get current stats
    pub fn stats(&self) -> &TrainingStats {
        &self.stats
    }

    /// Process a step in the environment
    ///
    /// This method encodes the observation, computes rewards, and stores
    /// the transition for training. Action selection is done via the
    /// rule-based policy or external PPO trainer.
    pub fn step(
        &mut self,
        observation: &RawObservation,
        reward_transition: &RewardTransition,
        done: bool,
        action: ContinuousAction,
    ) {
        // Encode state
        let state_vec = self.encoder.encode(observation);

        // Compute reward
        let reward_signal = self.reward_fn.compute(reward_transition);

        // Store transition if training
        if self.training {
            let transition = Transition::new(
                state_vec.clone(),
                action.to_tensor().to_vec(),
                reward_signal.total,
                state_vec,
                done,
            )
            .with_reward_signal(reward_signal);

            self.rollout_buffer.push(transition);
            self.stats.steps += 1;

            // Check if we should update
            if self.rollout_buffer.is_full() || done {
                self.update();
            }

            if done {
                self.stats.episodes += 1;
            }
        }

        // Decay exploration
        self.stats.exploration_rate = (self.stats.exploration_rate * self.config.exploration_decay)
            .max(self.config.exploration_min);
    }

    /// Perform a training update
    fn update(&mut self) {
        if self.rollout_buffer.is_empty() {
            return;
        }

        info!(
            "Training update with {} transitions",
            self.rollout_buffer.len()
        );

        // Compute advantages
        let last_value = 0.0;
        self.rollout_buffer.compute_advantages(
            0.99, // gamma
            0.95, // gae_lambda
            last_value,
        );

        // Note: Actual PPO update would happen here with the neural network
        // For now, just clear the buffer
        self.stats.updates += 1;
        self.rollout_buffer.clear();

        info!(
            "Update {} complete: steps={}",
            self.stats.updates, self.stats.steps
        );
    }

    /// Get encoded state vector for external use
    pub fn encode_state(&self, observation: &RawObservation) -> Vec<f32> {
        self.encoder.encode(observation)
    }

    /// Get rollout buffer length
    pub fn buffer_len(&self) -> usize {
        self.rollout_buffer.len()
    }
}

/// Episode result from simulated training
#[derive(Debug, Clone)]
pub struct EpisodeResult {
    /// Total reward for episode
    pub total_reward: f32,
    /// Episode length in steps
    pub length: usize,
    /// Final PnL
    pub final_pnl: f64,
    /// Number of trades
    pub num_trades: usize,
    /// Win rate
    pub win_rate: f64,
}

/// Train agent using simulated environment
pub fn train_simulated(
    trainer: &mut PPOTrainer,
    env_config: TradingEnvConfig,
    num_episodes: usize,
    verbose: bool,
) -> Vec<EpisodeResult> {
    let mut env = TradingEnvironment::new(env_config);
    let mut results = Vec::with_capacity(num_episodes);

    for episode in 0..num_episodes {
        let mut obs = env.reset();
        let mut total_reward = 0.0f32;
        let mut done = false;

        // Collect experience for this episode
        let mut states = Vec::new();
        let mut actions = Vec::new();
        let mut rewards = Vec::new();
        let mut values = Vec::new();
        let mut log_probs = Vec::new();
        let mut dones = Vec::new();

        while !done {
            // Get action from trainer
            let (action_vec, log_prob) = trainer.get_action(&obs);
            let value = trainer.get_value(&obs);

            // Convert action to discrete
            let action_idx = action_vec.iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(i, _)| i)
                .unwrap_or(0);
            let action = EnvAction::from(action_idx);

            // Store transition
            states.push(obs.clone());
            actions.push(action_vec.clone());
            log_probs.push(log_prob);
            values.push(value);

            // Take step
            let result = env.step(action);
            total_reward += result.reward;
            rewards.push(result.reward);
            dones.push(result.done || result.truncated);
            obs = result.observation;
            done = result.done || result.truncated;
        }

        // Compute GAE and train
        let last_value = if done { 0.0 } else { trainer.get_value(&obs) };
        let (advantages, returns) = trainer.compute_gae(&rewards, &values, &dones, last_value);

        // Create batch
        let batch = PPOBatch {
            states,
            actions,
            old_log_probs: log_probs,
            returns,
            advantages,
            old_values: values,
        };

        // Train step
        let _ppo_output = trainer.train_step(batch);

        // Decay exploration
        trainer.decay_exploration();

        // Record result
        let episode_result = EpisodeResult {
            total_reward,
            length: env.step_count(),
            final_pnl: env.episode_pnl(),
            num_trades: env.num_trades(),
            win_rate: env.win_rate(),
        };

        if verbose {
            info!(
                "Episode {}/{}: reward={:.2}, pnl={:.2}, trades={}, win_rate={:.1}%, eps={:.3}",
                episode + 1,
                num_episodes,
                total_reward,
                episode_result.final_pnl,
                episode_result.num_trades,
                episode_result.win_rate * 100.0,
                trainer.exploration_rate()
            );
        }

        results.push(episode_result);
    }

    results
}

/// Calculate training summary statistics
pub fn summarize_results(results: &[EpisodeResult]) -> TrainingSummary {
    if results.is_empty() {
        return TrainingSummary::default();
    }

    let n = results.len() as f64;

    let avg_reward: f64 = results.iter().map(|r| r.total_reward as f64).sum::<f64>() / n;
    let avg_pnl: f64 = results.iter().map(|r| r.final_pnl).sum::<f64>() / n;
    let avg_length: f64 = results.iter().map(|r| r.length as f64).sum::<f64>() / n;
    let avg_trades: f64 = results.iter().map(|r| r.num_trades as f64).sum::<f64>() / n;
    let avg_win_rate: f64 = results.iter().map(|r| r.win_rate).sum::<f64>() / n;

    // Calculate profit factor (sum of wins / sum of losses)
    let total_wins: f64 = results.iter().filter(|r| r.final_pnl > 0.0).map(|r| r.final_pnl).sum();
    let total_losses: f64 = results.iter().filter(|r| r.final_pnl < 0.0).map(|r| -r.final_pnl).sum();
    let profit_factor = if total_losses > 0.0 { total_wins / total_losses } else { f64::INFINITY };

    TrainingSummary {
        num_episodes: results.len(),
        avg_reward: avg_reward as f32,
        avg_pnl,
        avg_episode_length: avg_length as f32,
        avg_trades: avg_trades as f32,
        avg_win_rate: avg_win_rate as f32,
        profit_factor,
    }
}

/// Training summary statistics
#[derive(Debug, Clone, Default)]
pub struct TrainingSummary {
    /// Number of episodes
    pub num_episodes: usize,
    /// Average reward per episode
    pub avg_reward: f32,
    /// Average PnL per episode
    pub avg_pnl: f64,
    /// Average episode length
    pub avg_episode_length: f32,
    /// Average trades per episode
    pub avg_trades: f32,
    /// Average win rate
    pub avg_win_rate: f32,
    /// Profit factor (wins / losses)
    pub profit_factor: f64,
}

/// Backtest result for a single episode
#[derive(Debug, Clone)]
pub struct BacktestResult {
    /// Round identifier
    pub round_slug: String,
    /// Total reward
    pub total_reward: f32,
    /// Episode length (ticks processed)
    pub length: usize,
    /// Final PnL
    pub final_pnl: f64,
    /// Number of trades
    pub num_trades: usize,
    /// Win rate
    pub win_rate: f64,
    /// Final capital
    pub final_capital: f64,
}

/// Run backtest on historical data
pub fn run_backtest(
    trainer: &mut PPOTrainer,
    data: HistoricalData,
    env_config: TradingEnvConfig,
    verbose: bool,
) -> BacktestResult {
    let round_slug = data.round_slug.clone();
    let total_ticks = data.up_ticks.len() + data.down_ticks.len();

    let mut env = BacktestEnvironment::new(data, env_config);
    let mut obs = env.reset();
    let mut total_reward = 0.0f32;

    if verbose {
        info!("Starting backtest on '{}' with {} ticks", round_slug, total_ticks);
    }

    while env.remaining_ticks() > 0 {
        // Get action from trainer
        let (action_vec, _log_prob) = trainer.get_action(&obs);

        // Convert to discrete action
        let action_idx = action_vec.iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let action = EnvAction::from(action_idx);

        // Step environment
        let result = env.step(action);
        total_reward += result.reward;
        obs = result.observation;

        if result.done {
            break;
        }
    }

    let stats = env.final_stats();

    if verbose {
        info!(
            "Backtest '{}' complete: pnl={:.2}, trades={}, win_rate={:.1}%, capital={:.2}",
            round_slug,
            stats.total_pnl,
            stats.num_trades,
            stats.win_rate * 100.0,
            stats.capital
        );
    }

    BacktestResult {
        round_slug,
        total_reward,
        length: env.total_ticks() - env.remaining_ticks(),
        final_pnl: stats.total_pnl,
        num_trades: stats.num_trades,
        win_rate: stats.win_rate,
        final_capital: stats.capital,
    }
}

/// Run multiple backtests on sample data for training/evaluation
pub fn train_backtest(
    trainer: &mut PPOTrainer,
    env_config: TradingEnvConfig,
    num_episodes: usize,
    duration_mins: u64,
    volatility: f64,
    verbose: bool,
) -> Vec<BacktestResult> {
    let mut results = Vec::with_capacity(num_episodes);

    for episode in 0..num_episodes {
        // Generate fresh sample data for each episode
        let data = generate_sample_data(duration_mins, volatility);

        // Run backtest
        let result = run_backtest(trainer, data, env_config.clone(), false);

        if verbose && (episode + 1) % 10 == 0 {
            info!(
                "Episode {}/{}: pnl={:.2}, trades={}, win_rate={:.1}%, eps={:.3}",
                episode + 1,
                num_episodes,
                result.final_pnl,
                result.num_trades,
                result.win_rate * 100.0,
                trainer.exploration_rate()
            );
        }

        // Decay exploration after each episode
        trainer.decay_exploration();

        results.push(result);
    }

    results
}

/// Summarize backtest results
pub fn summarize_backtest_results(results: &[BacktestResult]) -> BacktestSummary {
    if results.is_empty() {
        return BacktestSummary::default();
    }

    let n = results.len() as f64;

    let avg_pnl: f64 = results.iter().map(|r| r.final_pnl).sum::<f64>() / n;
    let avg_trades: f64 = results.iter().map(|r| r.num_trades as f64).sum::<f64>() / n;
    let avg_win_rate: f64 = results.iter().map(|r| r.win_rate).sum::<f64>() / n;
    let avg_reward: f64 = results.iter().map(|r| r.total_reward as f64).sum::<f64>() / n;

    // Profit factor
    let total_wins: f64 = results.iter().filter(|r| r.final_pnl > 0.0).map(|r| r.final_pnl).sum();
    let total_losses: f64 = results.iter().filter(|r| r.final_pnl < 0.0).map(|r| -r.final_pnl).sum();
    let profit_factor = if total_losses > 0.0 { total_wins / total_losses } else { f64::INFINITY };

    // Win rate by episode
    let winning_episodes = results.iter().filter(|r| r.final_pnl > 0.0).count();
    let episode_win_rate = winning_episodes as f64 / n;

    // Max drawdown
    let mut peak = results[0].final_capital;
    let mut max_drawdown = 0.0f64;
    for r in results {
        if r.final_capital > peak {
            peak = r.final_capital;
        }
        let drawdown = (peak - r.final_capital) / peak;
        if drawdown > max_drawdown {
            max_drawdown = drawdown;
        }
    }

    BacktestSummary {
        num_episodes: results.len(),
        avg_pnl,
        total_pnl: results.iter().map(|r| r.final_pnl).sum(),
        avg_trades,
        avg_win_rate,
        avg_reward,
        profit_factor,
        episode_win_rate,
        max_drawdown,
    }
}

/// Summary statistics from backtesting
#[derive(Debug, Clone, Default)]
pub struct BacktestSummary {
    /// Number of episodes
    pub num_episodes: usize,
    /// Average PnL per episode
    pub avg_pnl: f64,
    /// Total cumulative PnL
    pub total_pnl: f64,
    /// Average trades per episode
    pub avg_trades: f64,
    /// Average win rate (per trade)
    pub avg_win_rate: f64,
    /// Average reward
    pub avg_reward: f64,
    /// Profit factor
    pub profit_factor: f64,
    /// Episode win rate
    pub episode_win_rate: f64,
    /// Maximum drawdown
    pub max_drawdown: f64,
}
