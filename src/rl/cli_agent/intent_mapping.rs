use super::*;
use rust_decimal_macros::dec;

impl RLCryptoAgent {
    fn deployment_id(&self) -> String {
        let market_slug = self.config.market_slug.trim().to_ascii_lowercase();
        if market_slug.is_empty() {
            "crypto.pm.rl_crypto".to_string()
        } else {
            format!("crypto.pm.rl_crypto.{}", market_slug)
        }
    }

    pub(super) fn action_to_intents(&self, action: ContinuousAction) -> Vec<OrderIntent> {
        let discrete = action.to_discrete();
        let mut intents = Vec::new();
        let policy_source = self.last_action_source.as_deref().unwrap_or("unknown");
        let policy_version = self.config.policy_model_version.as_deref().unwrap_or("");
        let deployment_id = self.deployment_id();

        match discrete {
            DiscreteAction::Hold => {}
            DiscreteAction::BuyUp => {
                if let Some(ask) = self.current_obs.up_ask {
                    let shares = self.calculate_shares(&action);
                    let intent = OrderIntent::new(
                        &self.config.id,
                        Domain::Crypto,
                        &self.config.market_slug,
                        &self.config.up_token_id,
                        Side::Up,
                        true,
                        shares,
                        ask,
                    )
                    .with_priority(if action.is_aggressive() {
                        OrderPriority::High
                    } else {
                        OrderPriority::Normal
                    })
                    .with_metadata("strategy", "rl_crypto")
                    .with_deployment_id(deployment_id.as_str())
                    .with_metadata("action", "buy_up")
                    .with_metadata("step", &self.step_count.to_string())
                    .with_metadata("policy_source", policy_source)
                    .with_metadata("policy_model_version", policy_version);

                    intents.push(intent);
                }
            }
            DiscreteAction::BuyDown => {
                if let Some(ask) = self.current_obs.down_ask {
                    let shares = self.calculate_shares(&action);
                    let intent = OrderIntent::new(
                        &self.config.id,
                        Domain::Crypto,
                        &self.config.market_slug,
                        &self.config.down_token_id,
                        Side::Down,
                        true,
                        shares,
                        ask,
                    )
                    .with_priority(if action.is_aggressive() {
                        OrderPriority::High
                    } else {
                        OrderPriority::Normal
                    })
                    .with_metadata("strategy", "rl_crypto")
                    .with_deployment_id(deployment_id.as_str())
                    .with_metadata("action", "buy_down")
                    .with_metadata("step", &self.step_count.to_string())
                    .with_metadata("policy_source", policy_source)
                    .with_metadata("policy_model_version", policy_version);

                    intents.push(intent);
                }
            }
            DiscreteAction::SellPosition => {
                if let Some(pos) = &self.position {
                    let bid = match pos.side {
                        Side::Up => self.current_obs.up_bid,
                        Side::Down => self.current_obs.down_bid,
                    };

                    if let Some(bid) = bid {
                        let intent = OrderIntent::new(
                            &self.config.id,
                            Domain::Crypto,
                            &self.config.market_slug,
                            &pos.token_id,
                            pos.side,
                            false,
                            pos.shares,
                            bid,
                        )
                        .with_priority(OrderPriority::High)
                        .with_metadata("strategy", "rl_crypto")
                        .with_deployment_id(deployment_id.as_str())
                        .with_metadata("action", "sell")
                        .with_metadata("exit_reason", "rl_signal")
                        .with_metadata("policy_source", policy_source)
                        .with_metadata("policy_model_version", policy_version);

                        intents.push(intent);
                    }
                }
            }
            DiscreteAction::EnterHedge => {
                if let Some(pos) = &self.position {
                    let (other_side, other_token, other_ask) = match pos.side {
                        Side::Up => (
                            Side::Down,
                            &self.config.down_token_id,
                            self.current_obs.down_ask,
                        ),
                        Side::Down => (Side::Up, &self.config.up_token_id, self.current_obs.up_ask),
                    };

                    if let Some(ask) = other_ask {
                        let total_cost = pos.entry_price + ask;
                        if total_cost < dec!(1.0) {
                            let intent = OrderIntent::new(
                                &self.config.id,
                                Domain::Crypto,
                                &self.config.market_slug,
                                other_token,
                                other_side,
                                true,
                                pos.shares,
                                ask,
                            )
                            .with_priority(OrderPriority::High)
                            .with_metadata("strategy", "rl_crypto")
                            .with_deployment_id(deployment_id.as_str())
                            .with_metadata("action", "hedge")
                            .with_metadata("locked_profit", &(dec!(1.0) - total_cost).to_string())
                            .with_metadata("policy_source", policy_source)
                            .with_metadata("policy_model_version", policy_version);

                            intents.push(intent);
                        }
                    }
                }
            }
        }

        intents
    }

    fn calculate_shares(&self, action: &ContinuousAction) -> u64 {
        let base = self.config.default_shares;
        let multiplier = action.position_size_pct();
        ((base as f32) * multiplier).max(1.0) as u64
    }
}
