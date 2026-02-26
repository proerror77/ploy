//! Per-agent rolling performance tracker
//!
//! Consumes GlobalState snapshots and maintains rolling metrics:
//! Sharpe ratio, win rate, max drawdown, and a composite score.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::coordinator::GlobalState;

use super::config::AllocatorConfig;

/// Rolling performance metrics for one agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPerformance {
    pub agent_id: String,
    /// Rolling PnL over the performance window
    pub rolling_pnl: Decimal,
    /// Annualized Sharpe ratio (approximate)
    pub rolling_sharpe: f64,
    /// Win rate (fraction of positive PnL deltas)
    pub win_rate: f64,
    /// Maximum drawdown observed in the window
    pub max_drawdown: Decimal,
    /// Composite score (0.0-1.0), higher = better
    pub score: f64,
    /// Last evaluation timestamp
    pub evaluated_at: DateTime<Utc>,
}

/// Single observation in the performance ring buffer
#[derive(Debug, Clone)]
struct PnlSample {
    timestamp: DateTime<Utc>,
    daily_pnl: Decimal,
}

/// Tracks performance for all agents
pub struct PerformanceTracker {
    config: AllocatorConfig,
    window_secs: u64,
    /// Ring buffer per agent: (timestamp, daily_pnl, exposure)
    history: HashMap<String, VecDeque<PnlSample>>,
    /// Latest computed performance per agent
    latest: HashMap<String, AgentPerformance>,
}

impl PerformanceTracker {
    pub fn new(config: AllocatorConfig, window_secs: u64) -> Self {
        Self {
            config,
            window_secs,
            history: HashMap::new(),
            latest: HashMap::new(),
        }
    }

    /// Ingest a GlobalState snapshot and update all agent performance metrics
    pub fn update(&mut self, state: &GlobalState) {
        let now = Utc::now();
        let cutoff = now - chrono::Duration::seconds(self.window_secs as i64);

        for (agent_id, snapshot) in &state.agents {
            // Skip the meta-agent itself
            if agent_id == "openclaw" {
                continue;
            }

            let buffer = self
                .history
                .entry(agent_id.clone())
                .or_insert_with(VecDeque::new);

            // Add new sample
            buffer.push_back(PnlSample {
                timestamp: now,
                daily_pnl: snapshot.daily_pnl,
            });

            // Prune old samples
            while buffer.front().map_or(false, |s| s.timestamp < cutoff) {
                buffer.pop_front();
            }

            // Compute metrics
            let perf = compute_performance(&self.config, self.window_secs, agent_id, buffer);
            self.latest.insert(agent_id.clone(), perf);
        }

        // Remove agents no longer present
        self.history.retain(|id, _| state.agents.contains_key(id));
        self.latest.retain(|id, _| state.agents.contains_key(id));
    }

    /// Get latest performance for all tracked agents
    pub fn all(&self) -> &HashMap<String, AgentPerformance> {
        &self.latest
    }

    /// Get performance for a specific agent
    pub fn get(&self, agent_id: &str) -> Option<&AgentPerformance> {
        self.latest.get(agent_id)
    }
}

fn compute_performance(
    config: &AllocatorConfig,
    window_secs: u64,
    agent_id: &str,
    buffer: &VecDeque<PnlSample>,
) -> AgentPerformance {
    let now = Utc::now();

    if buffer.len() < 2 {
        return AgentPerformance {
            agent_id: agent_id.to_string(),
            rolling_pnl: buffer.back().map(|s| s.daily_pnl).unwrap_or(Decimal::ZERO),
            rolling_sharpe: 0.0,
            win_rate: 0.5,
            max_drawdown: Decimal::ZERO,
            score: 0.5,
            evaluated_at: now,
        };
    }

    // PnL deltas between consecutive samples
    let mut deltas: Vec<f64> = Vec::with_capacity(buffer.len() - 1);
    let mut wins = 0u64;
    let mut peak_pnl = f64::NEG_INFINITY;
    let mut max_dd = 0.0f64;

    for i in 1..buffer.len() {
        let prev = dec_to_f64(buffer[i - 1].daily_pnl);
        let curr = dec_to_f64(buffer[i].daily_pnl);
        let delta = curr - prev;
        deltas.push(delta);

        if delta > 0.0 {
            wins += 1;
        }

        // Track drawdown
        if curr > peak_pnl {
            peak_pnl = curr;
        }
        let dd = peak_pnl - curr;
        if dd > max_dd {
            max_dd = dd;
        }
    }

    let rolling_pnl = buffer.back().map(|s| s.daily_pnl).unwrap_or(Decimal::ZERO);
    let win_rate = if deltas.is_empty() {
        0.5
    } else {
        wins as f64 / deltas.len() as f64
    };

    // Sharpe: mean(deltas) / std(deltas) * sqrt(observations_per_day)
    let mean = deltas.iter().sum::<f64>() / deltas.len() as f64;
    let variance = deltas.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / deltas.len() as f64;
    let std_dev = variance.sqrt();

    // Observations per day approximation
    let obs_per_day = if window_secs > 0 {
        86400.0 / window_secs as f64 * deltas.len() as f64
    } else {
        1.0
    };
    let sharpe = if std_dev > 1e-12 {
        (mean / std_dev) * obs_per_day.sqrt()
    } else {
        0.0
    };

    // Normalize sharpe to 0-1 range (clamp -3..+3 → 0..1)
    let sharpe_norm = ((sharpe + 3.0) / 6.0).clamp(0.0, 1.0);

    // Drawdown ratio: max_dd / peak (clamped 0-1)
    // Only meaningful when peak_pnl is positive (agent has been profitable)
    let drawdown_ratio = if peak_pnl > 1e-12 {
        (max_dd / peak_pnl).clamp(0.0, 1.0)
    } else {
        // Agent has never been profitable in this window — treat as zero drawdown
        // rather than inflating the ratio from a negative denominator
        0.0
    };

    // Composite score
    let score = config.sharpe_weight * sharpe_norm
        + config.win_rate_weight * win_rate
        + config.drawdown_weight * (1.0 - drawdown_ratio);

    AgentPerformance {
        agent_id: agent_id.to_string(),
        rolling_pnl,
        rolling_sharpe: sharpe,
        win_rate,
        max_drawdown: Decimal::from_f64_retain(max_dd).unwrap_or(Decimal::ZERO),
        score: score.clamp(0.0, 1.0),
        evaluated_at: now,
    }
}

fn dec_to_f64(d: Decimal) -> f64 {
    d.to_string().parse::<f64>().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dec_to_f64_works() {
        assert!((dec_to_f64(Decimal::new(150, 2)) - 1.5).abs() < 1e-9);
        assert!((dec_to_f64(Decimal::ZERO) - 0.0).abs() < 1e-9);
    }
}
