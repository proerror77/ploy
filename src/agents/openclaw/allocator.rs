//! Dynamic capital allocation engine
//!
//! Decides per-agent capital allocation based on:
//! - Current market regime (from RegimeDetector)
//! - Agent performance scores (from PerformanceTracker)
//! - Configured allocation policies

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::config::AllocatorConfig;
use super::performance::AgentPerformance;
use super::regime::MarketRegime;

/// A single allocation decision for one agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationDecision {
    pub agent_id: String,
    /// Recommended entry mode (e.g., "arb_only", "directional", "vol_straddle")
    pub entry_mode: String,
    /// Kelly fraction to use for position sizing
    pub kelly_fraction: f64,
    /// Maximum fraction of total capital this agent may use
    pub max_allocation_pct: f64,
    /// Whether to pause this agent
    pub should_pause: bool,
    /// Whether to resume this agent (if currently paused)
    pub should_resume: bool,
    /// Reason for the decision
    pub reason: String,
}

/// Governance metadata key-value pairs to push to CoordinatorHandle
#[derive(Debug, Clone)]
pub struct GovernanceUpdate {
    pub metadata: HashMap<String, String>,
    pub agents_to_pause: Vec<String>,
    pub agents_to_resume: Vec<String>,
}

/// Regime-to-policy mapping
#[derive(Debug, Clone)]
struct RegimePolicy {
    crypto_mode: &'static str,
    kelly_fraction: f64,
    max_intent_pct: f64,
}

fn regime_policy(regime: MarketRegime) -> RegimePolicy {
    match regime {
        MarketRegime::HighVol => RegimePolicy {
            crypto_mode: "vol_straddle",
            kelly_fraction: 0.15,
            max_intent_pct: 0.50,
        },
        MarketRegime::LowVol => RegimePolicy {
            crypto_mode: "arb_only",
            kelly_fraction: 0.30,
            max_intent_pct: 1.00,
        },
        MarketRegime::Trending => RegimePolicy {
            crypto_mode: "directional",
            kelly_fraction: 0.25,
            max_intent_pct: 1.00,
        },
        MarketRegime::Ranging => RegimePolicy {
            crypto_mode: "arb_only",
            kelly_fraction: 0.20,
            max_intent_pct: 0.75,
        },
    }
}

/// Stateful allocator — tracks pause cooldowns
pub struct DynamicAllocator {
    config: AllocatorConfig,
    /// Agent pause timestamps for cooldown enforcement
    pause_timestamps: HashMap<String, DateTime<Utc>>,
}

impl DynamicAllocator {
    pub fn new(config: AllocatorConfig) -> Self {
        Self {
            config,
            pause_timestamps: HashMap::new(),
        }
    }

    /// Decide allocation for all agents based on regime + performance.
    /// Returns a GovernanceUpdate with metadata changes and pause/resume actions.
    pub fn decide(
        &mut self,
        regime: MarketRegime,
        performances: &HashMap<String, AgentPerformance>,
        currently_paused: &[String],
    ) -> GovernanceUpdate {
        let policy = regime_policy(regime);
        let now = Utc::now();

        let mut metadata = HashMap::new();
        let mut agents_to_pause: Vec<String> = Vec::new();
        let mut agents_to_resume: Vec<String> = Vec::new();

        // Set global regime metadata
        metadata.insert("openclaw.regime".to_string(), regime.to_string());
        metadata.insert(
            "openclaw.crypto.entry_mode".to_string(),
            policy.crypto_mode.to_string(),
        );
        metadata.insert(
            "openclaw.kelly_fraction".to_string(),
            format!("{:.2}", policy.kelly_fraction),
        );
        metadata.insert(
            "openclaw.max_intent_pct".to_string(),
            format!("{:.2}", policy.max_intent_pct),
        );

        // Score-based pause/resume decisions
        let mut scored: Vec<(&String, f64)> = performances
            .iter()
            .map(|(id, perf)| (id, perf.score))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Count active (non-paused) agents
        let active_count = performances
            .keys()
            .filter(|id| !currently_paused.contains(id))
            .count();

        for (agent_id, score) in &scored {
            let is_paused = currently_paused.contains(agent_id);

            // Underperforming agents: consider pausing
            if *score < self.config.realloc_threshold && !is_paused {
                // Safety: never pause ALL agents
                let would_remain = active_count - agents_to_pause.len();
                if would_remain > 1 {
                    agents_to_pause.push(agent_id.to_string());
                    self.pause_timestamps.insert(agent_id.to_string(), now);
                    info!(
                        agent_id,
                        score,
                        regime = %regime,
                        "openclaw: pausing underperforming agent"
                    );
                } else {
                    warn!(
                        agent_id,
                        score, "openclaw: would pause but min-1-running guard prevents it"
                    );
                }
            }

            // Paused agents: consider resuming after cooldown
            if is_paused && *score >= self.config.realloc_threshold {
                let cooldown_expired = self
                    .pause_timestamps
                    .get(*agent_id)
                    .map(|paused_at| {
                        (now - *paused_at).num_seconds() as u64 >= self.config.pause_cooldown_secs
                    })
                    .unwrap_or(true);

                if cooldown_expired {
                    agents_to_resume.push(agent_id.to_string());
                    self.pause_timestamps.remove(*agent_id);
                    info!(
                        agent_id,
                        score,
                        regime = %regime,
                        "openclaw: resuming agent after cooldown"
                    );
                }
            }

            // Per-agent allocation metadata
            let agent_max = (score * policy.max_intent_pct).min(self.config.max_single_allocation);
            metadata.insert(
                format!("openclaw.agent.{}.max_alloc_pct", agent_id),
                format!("{:.2}", agent_max),
            );
        }

        GovernanceUpdate {
            metadata,
            agents_to_pause,
            agents_to_resume,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    #[test]
    fn regime_policy_mapping() {
        let p = regime_policy(MarketRegime::HighVol);
        assert_eq!(p.crypto_mode, "vol_straddle");
        assert!((p.kelly_fraction - 0.15).abs() < 1e-9);

        let p = regime_policy(MarketRegime::LowVol);
        assert_eq!(p.crypto_mode, "arb_only");
        assert!((p.kelly_fraction - 0.30).abs() < 1e-9);
    }

    #[test]
    fn min_one_running_guard() {
        let cfg = AllocatorConfig::default();
        let mut alloc = DynamicAllocator::new(cfg);

        // Single agent with bad score
        let mut perfs = HashMap::new();
        perfs.insert(
            "crypto".to_string(),
            AgentPerformance {
                agent_id: "crypto".to_string(),
                rolling_pnl: Decimal::ZERO,
                rolling_sharpe: -1.0,
                win_rate: 0.0,
                max_drawdown: Decimal::from(100),
                score: 0.01, // Very bad
                evaluated_at: Utc::now(),
            },
        );

        let update = alloc.decide(MarketRegime::Ranging, &perfs, &[]);
        // Should NOT pause the only agent
        assert!(
            update.agents_to_pause.is_empty(),
            "must not pause the only running agent"
        );
    }
}
