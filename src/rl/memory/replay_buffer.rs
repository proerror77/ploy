//! Replay Buffer
//!
//! Experience replay buffer for off-policy learning and PPO rollouts.

use rand::seq::SliceRandom;
use rand::thread_rng;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use crate::rl::core::{ContinuousAction, DiscreteAction, RewardSignal};

/// A single transition in the environment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    /// State features before action
    pub state: Vec<f32>,
    /// Action taken (continuous representation)
    pub action: Vec<f32>,
    /// Discrete action (if applicable)
    pub discrete_action: Option<DiscreteAction>,
    /// Reward received
    pub reward: f32,
    /// Detailed reward signal
    pub reward_signal: RewardSignal,
    /// Next state features
    pub next_state: Vec<f32>,
    /// Whether episode terminated
    pub done: bool,
    /// Log probability of action (for PPO)
    pub log_prob: Option<f32>,
    /// Value estimate at state (for PPO)
    pub value: Option<f32>,
}

impl Transition {
    /// Create a new transition
    pub fn new(
        state: Vec<f32>,
        action: Vec<f32>,
        reward: f32,
        next_state: Vec<f32>,
        done: bool,
    ) -> Self {
        Self {
            state,
            action,
            discrete_action: None,
            reward,
            reward_signal: RewardSignal::from_pnl(reward),
            next_state,
            done,
            log_prob: None,
            value: None,
        }
    }

    /// Set the discrete action
    pub fn with_discrete_action(mut self, action: DiscreteAction) -> Self {
        self.discrete_action = Some(action);
        self
    }

    /// Set the reward signal
    pub fn with_reward_signal(mut self, signal: RewardSignal) -> Self {
        self.reward_signal = signal;
        self
    }

    /// Set PPO-specific fields
    pub fn with_ppo_data(mut self, log_prob: f32, value: f32) -> Self {
        self.log_prob = Some(log_prob);
        self.value = Some(value);
        self
    }
}

/// Replay buffer for experience storage
#[derive(Debug)]
pub struct ReplayBuffer {
    /// Storage for transitions
    buffer: VecDeque<Transition>,
    /// Maximum capacity
    capacity: usize,
}

impl ReplayBuffer {
    /// Create a new replay buffer with given capacity
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Add a transition to the buffer
    pub fn push(&mut self, transition: Transition) {
        if self.buffer.len() >= self.capacity {
            self.buffer.pop_front();
        }
        self.buffer.push_back(transition);
    }

    /// Sample a random batch of transitions
    pub fn sample(&self, batch_size: usize) -> Vec<Transition> {
        let mut rng = thread_rng();
        let mut indices: Vec<usize> = (0..self.buffer.len()).collect();
        indices.shuffle(&mut rng);

        indices
            .into_iter()
            .take(batch_size.min(self.buffer.len()))
            .map(|i| self.buffer[i].clone())
            .collect()
    }

    /// Get all transitions (for PPO on-policy training)
    pub fn get_all(&self) -> Vec<Transition> {
        self.buffer.iter().cloned().collect()
    }

    /// Clear all transitions
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Get current number of transitions
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Check if buffer has enough samples for training
    pub fn has_enough_samples(&self, min_samples: usize) -> bool {
        self.buffer.len() >= min_samples
    }

    /// Get buffer capacity
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Get fill ratio (0.0 to 1.0)
    pub fn fill_ratio(&self) -> f32 {
        self.buffer.len() as f32 / self.capacity as f32
    }

    /// Extract batch tensors for training
    ///
    /// Returns (states, actions, rewards, next_states, dones)
    pub fn to_batch_tensors(
        &self,
        batch: &[Transition],
    ) -> (
        Vec<Vec<f32>>,
        Vec<Vec<f32>>,
        Vec<f32>,
        Vec<Vec<f32>>,
        Vec<bool>,
    ) {
        let states: Vec<Vec<f32>> = batch.iter().map(|t| t.state.clone()).collect();
        let actions: Vec<Vec<f32>> = batch.iter().map(|t| t.action.clone()).collect();
        let rewards: Vec<f32> = batch.iter().map(|t| t.reward).collect();
        let next_states: Vec<Vec<f32>> = batch.iter().map(|t| t.next_state.clone()).collect();
        let dones: Vec<bool> = batch.iter().map(|t| t.done).collect();

        (states, actions, rewards, next_states, dones)
    }

    /// Extract PPO-specific tensors
    ///
    /// Returns (states, actions, log_probs, values, rewards, dones)
    pub fn to_ppo_tensors(
        &self,
        transitions: &[Transition],
    ) -> (
        Vec<Vec<f32>>,
        Vec<Vec<f32>>,
        Vec<f32>,
        Vec<f32>,
        Vec<f32>,
        Vec<bool>,
    ) {
        let states: Vec<Vec<f32>> = transitions.iter().map(|t| t.state.clone()).collect();
        let actions: Vec<Vec<f32>> = transitions.iter().map(|t| t.action.clone()).collect();
        let log_probs: Vec<f32> = transitions
            .iter()
            .map(|t| t.log_prob.unwrap_or(0.0))
            .collect();
        let values: Vec<f32> = transitions.iter().map(|t| t.value.unwrap_or(0.0)).collect();
        let rewards: Vec<f32> = transitions.iter().map(|t| t.reward).collect();
        let dones: Vec<bool> = transitions.iter().map(|t| t.done).collect();

        (states, actions, log_probs, values, rewards, dones)
    }
}

impl Default for ReplayBuffer {
    fn default() -> Self {
        Self::new(10_000)
    }
}

/// Rollout buffer for on-policy algorithms (PPO)
///
/// Stores complete episodes/rollouts for training.
#[derive(Debug)]
pub struct RolloutBuffer {
    /// Transitions in current rollout
    transitions: Vec<Transition>,
    /// Computed advantages
    advantages: Vec<f32>,
    /// Computed returns
    returns: Vec<f32>,
    /// Maximum rollout length
    max_length: usize,
}

impl RolloutBuffer {
    /// Create a new rollout buffer
    pub fn new(max_length: usize) -> Self {
        Self {
            transitions: Vec::with_capacity(max_length),
            advantages: Vec::new(),
            returns: Vec::new(),
            max_length,
        }
    }

    /// Add a transition
    pub fn push(&mut self, transition: Transition) {
        if self.transitions.len() < self.max_length {
            self.transitions.push(transition);
        }
    }

    /// Check if rollout is full
    pub fn is_full(&self) -> bool {
        self.transitions.len() >= self.max_length
    }

    /// Get rollout length
    pub fn len(&self) -> usize {
        self.transitions.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty()
    }

    /// Compute advantages and returns using GAE
    pub fn compute_advantages(&mut self, gamma: f32, gae_lambda: f32, last_value: f32) {
        let n = self.transitions.len();
        self.advantages = vec![0.0; n];
        self.returns = vec![0.0; n];

        let mut gae = 0.0;
        let mut next_value = last_value;

        for t in (0..n).rev() {
            let transition = &self.transitions[t];
            let mask = if transition.done { 0.0 } else { 1.0 };
            let value = transition.value.unwrap_or(0.0);

            let delta = transition.reward + gamma * next_value * mask - value;
            gae = delta + gamma * gae_lambda * mask * gae;

            self.advantages[t] = gae;
            self.returns[t] = gae + value;
            next_value = value;
        }

        // Normalize advantages
        if n > 1 {
            let mean: f32 = self.advantages.iter().sum::<f32>() / n as f32;
            let var: f32 = self
                .advantages
                .iter()
                .map(|a| (a - mean).powi(2))
                .sum::<f32>()
                / n as f32;
            let std = var.sqrt().max(1e-8);

            for adv in &mut self.advantages {
                *adv = (*adv - mean) / std;
            }
        }
    }

    /// Get transitions with computed advantages
    pub fn get_batch(&self) -> Vec<(Transition, f32, f32)> {
        self.transitions
            .iter()
            .zip(self.advantages.iter())
            .zip(self.returns.iter())
            .map(|((t, a), r)| (t.clone(), *a, *r))
            .collect()
    }

    /// Sample mini-batches for training
    pub fn sample_minibatches(&self, batch_size: usize) -> Vec<Vec<(Transition, f32, f32)>> {
        let mut indices: Vec<usize> = (0..self.transitions.len()).collect();
        indices.shuffle(&mut thread_rng());

        indices
            .chunks(batch_size)
            .map(|chunk| {
                chunk
                    .iter()
                    .map(|&i| {
                        (
                            self.transitions[i].clone(),
                            self.advantages[i],
                            self.returns[i],
                        )
                    })
                    .collect()
            })
            .collect()
    }

    /// Clear the buffer
    pub fn clear(&mut self) {
        self.transitions.clear();
        self.advantages.clear();
        self.returns.clear();
    }
}

impl Default for RolloutBuffer {
    fn default() -> Self {
        Self::new(2048)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_transition(reward: f32, done: bool) -> Transition {
        Transition::new(vec![0.0; 42], vec![0.0; 5], reward, vec![0.0; 42], done)
    }

    #[test]
    fn test_replay_buffer_push() {
        let mut buffer = ReplayBuffer::new(10);

        for i in 0..15 {
            buffer.push(make_transition(i as f32, false));
        }

        // Should only keep last 10
        assert_eq!(buffer.len(), 10);
    }

    #[test]
    fn test_replay_buffer_sample() {
        let mut buffer = ReplayBuffer::new(100);

        for i in 0..50 {
            buffer.push(make_transition(i as f32, false));
        }

        let batch = buffer.sample(10);
        assert_eq!(batch.len(), 10);
    }

    #[test]
    fn test_rollout_buffer() {
        let mut buffer = RolloutBuffer::new(100);

        for i in 0..10 {
            let mut t = make_transition(1.0, i == 9);
            t.value = Some(0.5);
            buffer.push(t);
        }

        buffer.compute_advantages(0.99, 0.95, 0.0);

        assert_eq!(buffer.len(), 10);
        assert_eq!(buffer.advantages.len(), 10);
        assert_eq!(buffer.returns.len(), 10);
    }
}
