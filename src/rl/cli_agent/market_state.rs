use super::*;
use chrono::Datelike;
use chrono::Timelike;

impl RLCryptoAgent {
    /// Update observation from crypto event.
    pub(super) fn update_from_crypto_event(&mut self, event: &CryptoEvent) {
        self.current_obs.spot_price = Some(event.spot_price);

        if self.current_obs.price_history.len() >= 15 {
            self.current_obs.price_history.remove(0);
        }
        self.current_obs.price_history.push(event.spot_price);

        if let Some(momentum) = event.momentum {
            self.current_obs.momentum_1s = Some(Decimal::try_from(momentum[0]).unwrap_or_default());
            self.current_obs.momentum_5s = Some(Decimal::try_from(momentum[1]).unwrap_or_default());
            self.current_obs.momentum_15s =
                Some(Decimal::try_from(momentum[2]).unwrap_or_default());
            self.current_obs.momentum_60s =
                Some(Decimal::try_from(momentum[3]).unwrap_or_default());
        }

        if let Some(quotes) = &event.quotes {
            self.current_obs.up_bid = Some(quotes.up_bid);
            self.current_obs.up_ask = Some(quotes.up_ask);
            self.current_obs.down_bid = Some(quotes.down_bid);
            self.current_obs.down_ask = Some(quotes.down_ask);
            self.current_obs.calculate_spreads();
            self.current_obs.calculate_sum_of_asks();
        }

        let now = Utc::now();
        self.current_obs
            .update_time_features(now.hour(), now.weekday().num_days_from_monday());
        self.update_position_features();
    }

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

    /// Decay exploration rate.
    pub(super) fn decay_exploration(&mut self) {
        let decay = self.config.rl_config.training.exploration_decay;
        let min = self.config.rl_config.training.exploration_min;
        self.exploration_rate = (self.exploration_rate * decay).max(min);
    }

    /// Process crypto event and generate intents.
    pub(super) fn process_crypto_event(&mut self, event: &CryptoEvent) -> Vec<OrderIntent> {
        let coin = event.symbol.replace("USDT", "");
        if !self.config.coins.iter().any(|c| c == &coin) {
            return vec![];
        }

        if !self.config.market_slug.is_empty() {
            if let Some(slug) = &event.round_slug {
                if slug != &self.config.market_slug {
                    return vec![];
                }
            }
        }

        self.prev_obs = Some(self.current_obs.clone());
        self.update_from_crypto_event(event);
        self.step_count += 1;

        let action = self.select_action();
        let intents = self.action_to_intents(action);

        if !intents.is_empty() {
            debug!(
                "[{}] Step {}: Generated {} intents, action={:?}",
                self.config.id,
                self.step_count,
                intents.len(),
                action.to_discrete()
            );
        }

        intents
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
