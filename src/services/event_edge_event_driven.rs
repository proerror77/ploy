use crate::adapters::PolymarketClient;
use crate::config::EventEdgeAgentConfig;
use crate::error::{PloyError, Result};
use crate::strategy::event_edge::core::{EventEdgeCore, EventEdgeState};
use crate::strategy::event_models::arena_text::fetch_arena_text_snapshot;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedState {
    last_arena_updated: Option<NaiveDate>,
    last_trade_at: HashMap<String, DateTime<Utc>>,
    daily_spend_usd: Decimal,
    daily_spend_day: NaiveDate,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            last_arena_updated: None,
            last_trade_at: HashMap::new(),
            daily_spend_usd: Decimal::ZERO,
            daily_spend_day: Utc::now().date_naive(),
        }
    }
}

/// Event-driven runner with persisted state and Arena change detection.
pub struct EventEdgeEventDrivenAgent {
    core: EventEdgeCore,
    state_path: PathBuf,
}

impl EventEdgeEventDrivenAgent {
    pub async fn new(client: PolymarketClient, cfg: EventEdgeAgentConfig) -> Result<Self> {
        let state_path = default_state_path();
        let persisted = load_state(&state_path).await.unwrap_or_default();
        let core_state = EventEdgeState {
            last_trade_at: persisted.last_trade_at,
            daily_spend_usd: persisted.daily_spend_usd,
            daily_spend_day: persisted.daily_spend_day,
            last_arena_updated: persisted.last_arena_updated,
            resolved_event_ids: Vec::new(),
        };
        Ok(Self {
            core: EventEdgeCore::with_state(client, cfg, core_state),
            state_path,
        })
    }

    pub async fn run_forever(mut self) -> Result<()> {
        if !self.core.cfg.enabled {
            warn!("EventEdgeEventDrivenAgent disabled in config");
            return Ok(());
        }
        if self.core.targets_empty() {
            warn!("EventEdgeEventDrivenAgent enabled but no targets configured");
            return Ok(());
        }

        let interval = Duration::from_secs(self.core.cfg.interval_secs.max(5));
        info!(
            "EventEdgeEventDrivenAgent started (interval={}s trade={} cooldown={}s max_daily_spend=${} state={})",
            interval.as_secs(),
            self.core.cfg.trade,
            self.core.cfg.cooldown_secs,
            self.core.cfg.max_daily_spend_usd,
            self.state_path.display()
        );

        let mut tick = tokio::time::interval(interval);
        let mut backoff_secs: u64 = 0;

        loop {
            tick.tick().await;
            if backoff_secs > 0 {
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
            }

            match self.run_one_cycle().await {
                Ok(()) => backoff_secs = 0,
                Err(e) => {
                    warn!("EventEdgeEventDrivenAgent cycle error: {}", e);
                    backoff_secs = (backoff_secs.saturating_mul(2)).clamp(5, 300);
                    if backoff_secs == 0 {
                        backoff_secs = 5;
                    }
                }
            }
        }
    }

    async fn run_one_cycle(&mut self) -> Result<()> {
        self.core.reset_daily_if_needed();

        let arena = fetch_arena_text_snapshot().await?;
        if let Some(last) = arena.last_updated {
            if self.core.state.last_arena_updated == Some(last) {
                info!(
                    "EventEdgeEventDrivenAgent: Arena unchanged (last_updated={}); skipping",
                    last
                );
                return Ok(());
            }
        }

        let event_ids = self.core.resolve_event_ids().await?;
        for event_id in &event_ids {
            if let Err(e) = self.scan_and_maybe_trade(event_id, arena.clone()).await {
                warn!(
                    "EventEdgeEventDrivenAgent: scan/trade failed for {}: {}",
                    event_id, e
                );
            }
        }

        self.core.state.last_arena_updated = arena.last_updated;
        self.save_persisted_state().await?;
        Ok(())
    }

    async fn scan_and_maybe_trade(
        &mut self,
        event_id: &str,
        arena: crate::strategy::event_models::arena_text::ArenaTextSnapshot,
    ) -> Result<()> {
        let scan = self.core.scan_event(event_id, Some(arena)).await?;
        info!(
            "EventEdgeEventDrivenAgent: event={} title=\"{}\" end={} conf={:.2} arena_last_updated={:?}",
            scan.event_id, scan.event_title,
            scan.end_time.to_rfc3339(), scan.confidence, scan.arena_last_updated
        );

        if let Some(d) = self.core.pick_best_trade(&scan) {
            warn!(
                "EventEdgeEventDrivenAgent blocked direct BUY outcome=\"{}\" shares={} ask={:.2}¢ edge={:.1}pp: route through coordinator intent ingress",
                d.outcome,
                d.shares,
                d.limit_price * dec!(100),
                d.edge * dec!(100)
            );
        }
        Ok(())
    }

    async fn save_persisted_state(&self) -> Result<()> {
        let ps = PersistedState {
            last_arena_updated: self.core.state.last_arena_updated,
            last_trade_at: self.core.state.last_trade_at.clone(),
            daily_spend_usd: self.core.state.daily_spend_usd,
            daily_spend_day: self.core.state.daily_spend_day,
        };
        save_state(&self.state_path, &ps).await
    }
}

// ── Persistence helpers (unchanged from original) ────────────────────

fn default_state_path() -> PathBuf {
    if let Ok(dir) = std::env::var("PLOY_STATE_DIR") {
        return PathBuf::from(dir).join("event_edge_openclaw.json");
    }
    PathBuf::from("data/state/event_edge_openclaw.json")
}

async fn load_state(path: &Path) -> Result<PersistedState> {
    let s = match tokio::fs::read_to_string(path).await {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PersistedState::default()),
        Err(e) => return Err(PloyError::Io(e)),
    };
    serde_json::from_str(&s).map_err(PloyError::from)
}

async fn save_state(path: &Path, st: &PersistedState) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Err(PloyError::Internal("invalid state path".to_string()));
    };
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(PloyError::from)?;
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(st)?;
    tokio::fs::write(&tmp, body)
        .await
        .map_err(PloyError::from)?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(PloyError::from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn state_roundtrip() {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ploy_openclaw_state_test_{}.json",
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));

        let st0 = PersistedState::default();
        save_state(&p, &st0).await.unwrap();
        let st1 = load_state(&p).await.unwrap();
        assert_eq!(st1.last_arena_updated, st0.last_arena_updated);
        assert_eq!(st1.daily_spend_day, st0.daily_spend_day);

        let _ = tokio::fs::remove_file(&p).await;
    }
}
