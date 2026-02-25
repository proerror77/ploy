//! Cross-agent conflict detection + resolution
//!
//! Detects situations where two agents hold opposing positions on the same market
//! (e.g., crypto agent bought UP while politics agent sold DOWN on an overlapping market).
//! Resolves by pausing the lower-scoring agent.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::coordinator::GlobalState;
use crate::domain::Side;

use super::performance::AgentPerformance;

/// Detected conflict between two agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConflict {
    pub agent_a: String,
    pub agent_b: String,
    pub market_slug: String,
    /// Brief description of the conflict
    pub description: String,
    pub detected_at: DateTime<Utc>,
}

/// Conflict resolution action
#[derive(Debug, Clone)]
pub struct ConflictResolution {
    /// Agent to pause
    pub pause_agent_id: String,
    /// Reason for the pause
    pub reason: String,
}

pub struct ConflictDetector;

impl ConflictDetector {
    /// Detect conflicts: agents holding opposing sides on the same market
    pub fn detect(state: &GlobalState) -> Vec<AgentConflict> {
        let mut conflicts = Vec::new();

        // Build a map: market_slug → [(agent_id, side)]
        let mut market_agents: HashMap<String, Vec<(String, Side)>> = HashMap::new();

        for pos in &state.positions {
            let key = pos.market_slug.clone();
            market_agents
                .entry(key)
                .or_default()
                .push((pos.agent_id.clone(), pos.side));
        }

        // Check for opposing positions on the same market
        for (market, agents) in &market_agents {
            if agents.len() < 2 {
                continue;
            }

            // Compare each pair
            for i in 0..agents.len() {
                for j in (i + 1)..agents.len() {
                    let (agent_a, side_a) = &agents[i];
                    let (agent_b, side_b) = &agents[j];

                    // Opposing sides on the same market (Up vs Down)
                    if agents[i].1 != agents[j].1 {
                        conflicts.push(AgentConflict {
                            agent_a: agent_a.clone(),
                            agent_b: agent_b.clone(),
                            market_slug: market.clone(),
                            description: format!(
                                "{} ({:?}) vs {} ({:?}) on {}",
                                agent_a, side_a, agent_b, side_b, market
                            ),
                            detected_at: Utc::now(),
                        });
                    }
                }
            }
        }

        conflicts
    }

    /// Resolve conflicts by pausing the lower-scoring agent
    pub fn resolve(
        conflicts: &[AgentConflict],
        performances: &HashMap<String, AgentPerformance>,
    ) -> Vec<ConflictResolution> {
        let mut resolutions = Vec::new();
        let mut already_pausing: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for conflict in conflicts {
            let score_a = performances
                .get(&conflict.agent_a)
                .map(|p| p.score)
                .unwrap_or(0.5);
            let score_b = performances
                .get(&conflict.agent_b)
                .map(|p| p.score)
                .unwrap_or(0.5);

            let (loser, winner) = if score_a < score_b {
                (&conflict.agent_a, &conflict.agent_b)
            } else {
                (&conflict.agent_b, &conflict.agent_a)
            };

            if already_pausing.contains(loser) {
                continue;
            }

            info!(
                market = %conflict.market_slug,
                winner = %winner,
                loser = %loser,
                "openclaw: resolving conflict by pausing lower-scoring agent"
            );

            already_pausing.insert(loser.clone());
            resolutions.push(ConflictResolution {
                pause_agent_id: loser.clone(),
                reason: format!(
                    "conflict on {} — {} (score {:.2}) paused in favor of {} (score {:.2})",
                    conflict.market_slug,
                    loser,
                    if loser == &conflict.agent_a {
                        score_a
                    } else {
                        score_b
                    },
                    winner,
                    if winner == &conflict.agent_a {
                        score_a
                    } else {
                        score_b
                    },
                ),
            });
        }

        resolutions
    }
}
