use crate::error::Result;

#[cfg(feature = "claimer_daemon")]
pub use crate::strategy::claimer::{AutoClaimer, ClaimResult, ClaimerConfig, RedeemablePosition};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountClaimerHandle;

impl AccountClaimerHandle {
    pub async fn ensure_daemon(self) -> Result<()> {
        #[cfg(feature = "claimer_daemon")]
        {
            crate::strategy::claimer::ensure_account_claimer_daemon().await
        }

        #[cfg(not(feature = "claimer_daemon"))]
        {
            Ok(())
        }
    }
}

pub async fn ensure_account_claimer_daemon() -> Result<()> {
    AccountClaimerHandle.ensure_daemon().await
}
