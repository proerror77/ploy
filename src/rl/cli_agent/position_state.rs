use super::*;

impl RLCryptoAgent {
    /// Update position-related observation features.
    pub(super) fn update_position_features(&mut self) {
        if let Some(pos) = &self.position {
            self.current_obs.has_position = true;
            self.current_obs.position_side = Some(pos.side);
            self.current_obs.position_shares = pos.shares;
            self.current_obs.entry_price = Some(pos.entry_price);
            self.current_obs.unrealized_pnl = Some(pos.unrealized_pnl);
            self.current_obs.position_duration_secs =
                Some((Utc::now() - pos.entry_time).num_seconds());
        } else {
            self.current_obs.has_position = false;
            self.current_obs.position_side = None;
            self.current_obs.position_shares = 0;
            self.current_obs.entry_price = None;
            self.current_obs.unrealized_pnl = None;
            self.current_obs.position_duration_secs = None;
        }
    }

    /// Update exposure calculation.
    pub(super) fn update_exposure(&mut self) {
        self.total_exposure = self
            .position
            .as_ref()
            .map(|p| p.entry_price * Decimal::from(p.shares))
            .unwrap_or(Decimal::ZERO);
    }

    /// Update position prices.
    pub(super) fn update_position_prices(&mut self) {
        if let Some(pos) = &mut self.position {
            let current_price = match pos.side {
                Side::Up => self.current_obs.up_bid,
                Side::Down => self.current_obs.down_bid,
            };

            if let Some(price) = current_price {
                pos.unrealized_pnl = (price - pos.entry_price) * Decimal::from(pos.shares);
            }
        }
    }
}
