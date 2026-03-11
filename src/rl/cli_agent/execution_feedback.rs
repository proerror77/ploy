use super::*;
use crate::rl::ExecutionStatus;

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
}
