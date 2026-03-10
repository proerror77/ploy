use crate::control_plane::TradeIntent;

use super::CoordinatorHandle;

impl CoordinatorHandle {
    /// Submit a strategy trade intent through coordinator ingress.
    pub async fn submit_trade_intent(&self, intent: TradeIntent) -> crate::error::Result<()> {
        self.submit_order(intent.into()).await
    }
}
