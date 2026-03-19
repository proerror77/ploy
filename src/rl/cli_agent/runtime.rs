use super::*;

use chrono::{Datelike, Timelike};

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
            }
        }
    }

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
        usize::from(self.position.is_some())
    }

    pub fn total_exposure(&self) -> Decimal {
        self.total_exposure
    }

    pub fn daily_pnl(&self) -> Decimal {
        self.daily_pnl
    }
}
