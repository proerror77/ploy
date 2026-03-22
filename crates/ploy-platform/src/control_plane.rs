use crate::accounts::AccountClaimRegistry;
use crate::audit::AuditLog;
use crate::deployments::DeploymentRegistry;
use crate::health::{snapshot, HealthSnapshot};
use crate::system::SystemService;

#[derive(Debug, Default)]
pub struct ControlPlane {
    pub accounts: AccountClaimRegistry,
    pub deployments: DeploymentRegistry,
    pub audit: AuditLog,
    pub system: SystemService,
}

impl ControlPlane {
    pub fn health(&self) -> HealthSnapshot {
        snapshot(self.deployments.summaries().len())
    }
}
