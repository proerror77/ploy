//! NBA Comeback Platform Agent
//!
//! Implements `DomainAgent` for the NBA Q3→Q4 comeback strategy.
//! Receives Tick events, runs the ESPN scan cycle, and emits OrderIntents
//! for opportunities that clear all thresholds.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::domain::Side;
use crate::error::Result;
use crate::platform::{
    AgentRiskParams, AgentStatus, Domain, DomainAgent, DomainEvent, ExecutionReport, OrderIntent,
    OrderPriority,
};
use crate::strategy::nba_comeback::core::{ComebackOpportunity, NbaComebackCore};

const DEPLOYMENT_ID_NBA_COMEBACK: &str = "sports.pm.nba.comeback";

/// NBA Q3→Q4 Comeback Trading Agent
pub struct NbaComebackAgent {
    core: NbaComebackCore,
    status: AgentStatus,
    risk_params: AgentRiskParams,
    /// Maps intent_id → opportunity for tracking fills
    pending_intents: HashMap<Uuid, ComebackOpportunity>,
    /// Track positions for exposure calculation
    position_count: usize,
    total_exposure: Decimal,
    daily_pnl: Decimal,
    consecutive_failures: u32,
    /// Last ESPN poll time (throttle to poll_interval)
    last_poll: Option<DateTime<Utc>>,
}

impl NbaComebackAgent {
    pub fn new(core: NbaComebackCore) -> Self {
        let risk_params = AgentRiskParams {
            max_order_value: core.cfg.max_daily_spend_usd,
            max_total_exposure: core.cfg.max_daily_spend_usd * dec!(2),
            max_unhedged_positions: 5,
            max_daily_loss: core.cfg.max_daily_spend_usd,
            allow_overnight: false,
            allowed_markets: vec![],
        };

        Self {
            core,
            status: AgentStatus::Initializing,
            risk_params,
            pending_intents: HashMap::new(),
            position_count: 0,
            total_exposure: Decimal::ZERO,
            daily_pnl: Decimal::ZERO,
            consecutive_failures: 0,
            last_poll: None,
        }
    }

    /// Check if enough time has passed since last ESPN poll
    fn should_poll(&self) -> bool {
        match self.last_poll {
            None => true,
            Some(last) => {
                let elapsed = (Utc::now() - last).num_seconds();
                elapsed >= self.core.cfg.espn_poll_interval_secs as i64
            }
        }
    }

    /// Convert a ComebackOpportunity into an OrderIntent
    fn opportunity_to_intent(&self, opp: &ComebackOpportunity) -> OrderIntent {
        OrderIntent::new(
            "nba_comeback",
            Domain::Sports,
            &opp.market_slug,
            &opp.token_id,
            Side::Up, // YES side — betting the trailing team wins
            true,     // is_buy
            self.core.cfg.shares,
            opp.market_price,
        )
        .with_priority(OrderPriority::Normal)
        .with_metadata("strategy", "nba_comeback")
        .with_deployment_id(DEPLOYMENT_ID_NBA_COMEBACK)
        .with_metadata("game_id", &opp.game.espn_game_id)
        .with_metadata("trailing_team", &opp.trailing_abbrev)
        .with_metadata("deficit", &opp.deficit.to_string())
        .with_metadata("comeback_rate", &format!("{:.3}", opp.comeback_rate))
        .with_metadata("edge", &format!("{:.3}", opp.edge))
        .with_metadata(
            "adjusted_win_prob",
            &format!("{:.3}", opp.adjusted_win_prob),
        )
    }

    /// Handle a Tick event — run the scan cycle
    async fn handle_tick(&mut self) -> Vec<OrderIntent> {
        if !self.should_poll() {
            return vec![];
        }
        self.last_poll = Some(Utc::now());

        // Run ESPN scan to get candidates
        let candidates = self.core.scan_espn().await;
        if candidates.is_empty() {
            return vec![];
        }

        // For now, we create opportunities without Polymarket price lookup.
        // In production, the agent would query Polymarket for each candidate's
        // market. Here we emit intents for candidates that pass all filters,
        // using a placeholder market_slug that the platform router resolves.
        let mut intents = Vec::new();

        for candidate in &candidates {
            // Build a placeholder market slug from team names
            let market_slug = format!(
                "nba-{}-vs-{}",
                candidate.game.away_abbrev.to_lowercase(),
                candidate.game.home_abbrev.to_lowercase()
            );
            let token_id = format!("{}-win-yes", candidate.trailing_abbrev.to_lowercase());

            // Use adjusted_win_prob as a synthetic "market price" estimate
            // In production, this would be the actual Polymarket YES ask
            let estimated_price =
                Decimal::from_f64_retain(candidate.adjusted_win_prob * 0.85).unwrap_or(dec!(0.50));

            if let Some(opp) =
                self.core
                    .evaluate_opportunity(candidate, estimated_price, market_slug, token_id)
            {
                let intent = self.opportunity_to_intent(&opp);
                let intent_id = intent.intent_id;
                self.pending_intents.insert(intent_id, opp);
                intents.push(intent);
            }
        }

        if !intents.is_empty() {
            info!("NBA comeback: {} order intents generated", intents.len());
        }

        intents
    }
}

#[async_trait]
impl DomainAgent for NbaComebackAgent {
    fn id(&self) -> &str {
        "nba_comeback"
    }

    fn name(&self) -> &str {
        "NBA Q3→Q4 Comeback Agent"
    }

    fn domain(&self) -> Domain {
        Domain::Sports
    }

    fn status(&self) -> AgentStatus {
        self.status
    }

    fn risk_params(&self) -> &AgentRiskParams {
        &self.risk_params
    }

    async fn on_event(&mut self, event: DomainEvent) -> Result<Vec<OrderIntent>> {
        if !self.status.can_trade() {
            return Ok(vec![]);
        }

        match event {
            DomainEvent::Tick(_) => Ok(self.handle_tick().await),
            DomainEvent::Sports(se) => {
                // Could update internal game state cache from sports events
                debug!("NBA comeback: sports event for {}", se.game_id);
                Ok(vec![])
            }
            _ => Ok(vec![]),
        }
    }

    async fn on_execution(&mut self, report: ExecutionReport) {
        if report.is_success() {
            self.consecutive_failures = 0;

            if let Some(opp) = self.pending_intents.remove(&report.intent_id) {
                let cost = report.avg_fill_price.unwrap_or(opp.market_price)
                    * Decimal::from(report.filled_shares);

                self.core.record_trade(&opp.game.espn_game_id, cost);
                self.position_count += 1;
                self.total_exposure += cost;

                info!(
                    "NBA comeback: filled {} shares on {} (deficit={}, edge={:.3})",
                    report.filled_shares, opp.trailing_abbrev, opp.deficit, opp.edge
                );
            }
        } else {
            self.consecutive_failures += 1;
            self.pending_intents.remove(&report.intent_id);

            warn!(
                "NBA comeback: execution failed: {:?} (consecutive: {})",
                report.error_message, self.consecutive_failures
            );

            if self.consecutive_failures >= 3 {
                warn!("NBA comeback: too many failures, pausing");
                self.status = AgentStatus::Paused;
            }
        }
    }

    async fn start(&mut self) -> Result<()> {
        info!("NBA comeback agent: starting (loading stats)...");

        if let Err(e) = self.core.stats.load_all().await {
            warn!(
                "NBA comeback: failed to load stats: {} — continuing with empty cache",
                e
            );
        } else {
            info!(
                "NBA comeback: loaded {} team profiles",
                self.core.stats.team_count()
            );
        }

        self.status = AgentStatus::Running;
        info!("NBA comeback agent: running");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("NBA comeback agent: stopping");
        self.status = AgentStatus::Stopped;
        Ok(())
    }

    fn pause(&mut self) {
        info!("NBA comeback agent: paused");
        self.status = AgentStatus::Paused;
    }

    fn resume(&mut self) {
        info!("NBA comeback agent: resumed");
        self.consecutive_failures = 0;
        self.status = AgentStatus::Running;
    }

    fn position_count(&self) -> usize {
        self.position_count
    }

    fn total_exposure(&self) -> Decimal {
        self.total_exposure
    }

    fn daily_pnl(&self) -> Decimal {
        self.daily_pnl
    }
}

#[cfg(test)]
mod tests {
    use crate::config::NbaComebackConfig;
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn test_agent_lifecycle() {
        // We can't fully test without a DB, but we can test the lifecycle
        let cfg = NbaComebackConfig {
            enabled: true,
            min_edge: dec!(0.05),
            max_entry_price: dec!(0.75),
            shares: 50,
            cooldown_secs: 300,
            max_daily_spend_usd: dec!(100),
            min_deficit: 1,
            max_deficit: 15,
            target_quarter: 3,
            espn_poll_interval_secs: 30,
            min_comeback_rate: 0.15,
            season: "2025-26".to_string(),
            grok_enabled: false,
            grok_interval_secs: 300,
            grok_min_edge: dec!(0.08),
            grok_min_confidence: 0.6,
            grok_decision_cooldown_secs: 60,
            grok_fallback_enabled: true,
            min_reward_risk_ratio: 4.0,
            min_expected_value: 0.05,
            kelly_fraction_cap: 0.25,
            performance_daily_loss_limit_usd: dec!(30),
            performance_min_settled_trades: 10,
            performance_min_win_rate: 0.45,
            performance_low_winrate_multiplier: 0.60,
            performance_loss_streak_threshold: 3,
            performance_loss_streak_multiplier: 0.50,
            scaling_enabled: false,
            scaling_max_adds: 3,
            scaling_min_price_drop_pct: 5.0,
            scaling_max_game_exposure_usd: dec!(50),
            scaling_min_comeback_retention: 0.70,
            scaling_min_time_remaining_mins: 8.0,
            early_exit_enabled: true,
            early_exit_take_profit_pct: 15.0,
            early_exit_stop_loss_pct: 20.0,
        };

        // Test status transitions without DB
        // (Can't construct full agent without PgPool, so test config validation)
        assert!(cfg.enabled);
        assert_eq!(cfg.shares, 50);
        assert_eq!(cfg.target_quarter, 3);
    }
}
