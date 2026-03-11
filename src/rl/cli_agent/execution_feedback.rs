use super::*;

impl RLCryptoAgent {
    /// Handle execution report.
    pub(super) fn handle_execution(&mut self, report: &ExecutionReport) {
        match report.status {
            ExecutionStatus::Submitted | ExecutionStatus::Pending => {
                debug!(
                    "[{}] Execution accepted for processing: status={:?} order_id={:?}",
                    self.config.id, report.status, report.order_id
                );
            }
            _ if report.is_success() => {
                self.handle_successful_execution(report);
            }
            _ => {
                self.handle_failed_execution(report);
            }
        }
    }

    pub(super) fn handle_successful_execution(&mut self, report: &ExecutionReport) {
        self.consecutive_failures = 0;

        if let Some(avg_price) = report.avg_fill_price {
            if self.position.is_some() {
                if let Some(pos) = &self.position {
                    let realized =
                        (avg_price - pos.entry_price) * Decimal::from(report.filled_shares);
                    self.daily_pnl += realized;
                    info!(
                        "[{}] Position closed: realized PnL = {}",
                        self.config.id, realized
                    );
                }
                self.position = None;
                self.update_exposure();
            } else {
                let side = if self
                    .last_action
                    .map(|action| action.side_preference > 0.0)
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
                info!(
                    "[{}] Position opened: {:?} {} @ {}",
                    self.config.id, side, report.filled_shares, avg_price
                );
            }
        }

        self.decay_exploration();
    }

    pub(super) fn handle_failed_execution(&mut self, report: &ExecutionReport) {
        self.consecutive_failures += 1;
        warn!(
            "[{}] Execution failed: {:?}. Consecutive: {}",
            self.config.id, report.error_message, self.consecutive_failures
        );

        if self.consecutive_failures >= 3 {
            warn!("[{}] Too many failures, pausing agent", self.config.id);
            self.status = AgentStatus::Paused;
        }
    }
}
