use super::*;

impl RLCryptoAgent {
    /// Select action using RL policy
    pub(super) fn select_action(&mut self) -> ContinuousAction {
        let mut action = self.rule_based_policy();
        let mut source = "rule_based";

        #[cfg(feature = "onnx")]
        if let Some(model) = &self.policy_model {
            let state_vec = self.encoder.encode(&self.current_obs);
            match model.predict(&state_vec) {
                Ok(out) => match self.action_from_policy_output(&out) {
                    Some(candidate) => {
                        action = candidate;
                        source = "onnx";
                    }
                    None => {
                        warn!(
                            agent = %self.config.id,
                            output_dim = out.len(),
                            policy_output = %self.config.policy_output,
                            "RL ONNX policy output could not be interpreted; keeping rule-based policy"
                        );
                    }
                },
                Err(error) => {
                    warn!(
                        agent = %self.config.id,
                        error = %error,
                        "RL ONNX policy inference failed; keeping rule-based policy"
                    );
                }
            }
        }

        if rand::random::<f32>() < self.exploration_rate {
            action = ContinuousAction::new(
                rand::random::<f32>() * 2.0 - 1.0,
                rand::random::<f32>() * 2.0 - 1.0,
                rand::random::<f32>(),
                0.0,
                0.0,
            );
            source = "explore";
        }

        self.last_action = Some(action);
        self.last_action_source = Some(source.to_string());
        action
    }

    fn map_urgency(raw: f32) -> f32 {
        if !raw.is_finite() {
            return 0.5;
        }
        if (0.0..=1.0).contains(&raw) {
            return raw;
        }
        if (-1.0..=1.0).contains(&raw) {
            return (raw + 1.0) * 0.5;
        }
        1.0 / (1.0 + (-raw).exp())
    }

    fn action_from_discrete(action: DiscreteAction) -> ContinuousAction {
        match action {
            DiscreteAction::Hold => ContinuousAction::default(),
            DiscreteAction::BuyUp => ContinuousAction::new(0.8, 1.0, 0.5, 0.0, 0.0),
            DiscreteAction::BuyDown => ContinuousAction::new(0.8, -1.0, 0.5, 0.0, 0.0),
            DiscreteAction::SellPosition => ContinuousAction::new(-0.8, 0.0, 0.8, 0.0, 0.0),
            DiscreteAction::EnterHedge => ContinuousAction::new(0.8, 0.0, 0.6, 0.0, 0.0),
        }
    }

    fn argmax(values: &[f32]) -> Option<usize> {
        if values.is_empty() {
            return None;
        }
        let mut best_idx = 0usize;
        let mut best_val = values[0];
        for (index, &value) in values.iter().enumerate().skip(1) {
            if value > best_val {
                best_val = value;
                best_idx = index;
            }
        }
        Some(best_idx)
    }

    fn softmax(values: &[f32]) -> Vec<f32> {
        if values.is_empty() {
            return Vec::new();
        }
        let mut max = f32::NEG_INFINITY;
        for &value in values {
            if value.is_finite() && value > max {
                max = value;
            }
        }
        if !max.is_finite() {
            return vec![0.0; values.len()];
        }
        let mut exps = Vec::with_capacity(values.len());
        let mut sum = 0.0f32;
        for &value in values {
            let exp_value = if value.is_finite() {
                (value - max).exp()
            } else {
                0.0
            };
            exps.push(exp_value);
            sum += exp_value;
        }
        if sum <= 0.0 {
            return vec![0.0; values.len()];
        }
        for value in &mut exps {
            *value /= sum;
        }
        exps
    }

    fn action_from_policy_output(&self, output: &[f32]) -> Option<ContinuousAction> {
        let kind = self.config.policy_output.trim().to_ascii_lowercase();

        match kind.as_str() {
            "continuous" => {
                if output.len() < CONTINUOUS_ACTION_DIM {
                    return None;
                }
                let values = &output[..CONTINUOUS_ACTION_DIM];
                let urgency = Self::map_urgency(values[2]);
                Some(ContinuousAction::new(
                    values[0], values[1], urgency, values[3], values[4],
                ))
            }
            "continuous_mean_logstd" | "mean_logstd" => {
                if output.len() < CONTINUOUS_ACTION_DIM * 2 {
                    return None;
                }
                let mean = &output[..CONTINUOUS_ACTION_DIM];
                let urgency = Self::map_urgency(mean[2]);
                Some(ContinuousAction::new(
                    mean[0].tanh(),
                    mean[1].tanh(),
                    urgency,
                    mean[3].tanh(),
                    mean[4].tanh(),
                ))
            }
            "discrete_logits" | "discrete" => {
                if output.len() < NUM_DISCRETE_ACTIONS {
                    return None;
                }
                let probs = Self::softmax(&output[..NUM_DISCRETE_ACTIONS]);
                let idx = Self::argmax(&probs)?;
                let action = DiscreteAction::from_index(idx)?;
                Some(Self::action_from_discrete(action))
            }
            "discrete_probs" => {
                if output.len() < NUM_DISCRETE_ACTIONS {
                    return None;
                }
                let idx = Self::argmax(&output[..NUM_DISCRETE_ACTIONS])?;
                let action = DiscreteAction::from_index(idx)?;
                Some(Self::action_from_discrete(action))
            }
            _ => None,
        }
    }

    fn rule_based_policy(&self) -> ContinuousAction {
        if let Some(sum) = self.current_obs.sum_of_asks {
            let sum_f32: f32 = sum.to_string().parse().unwrap_or(1.0);

            if sum_f32 < 0.96 && !self.current_obs.has_position {
                let side_pref = match self.current_obs.momentum_1s {
                    Some(momentum) if momentum > Decimal::ZERO => 0.5,
                    Some(momentum) if momentum < Decimal::ZERO => -0.5,
                    _ => 0.0,
                };

                return ContinuousAction::new(0.7, side_pref, 0.5, 0.0, 0.0);
            }

            if sum_f32 > 1.0 && self.current_obs.has_position {
                return ContinuousAction::new(-0.8, 0.0, 0.7, 0.0, 0.0);
            }

            if let Some(pnl) = self.current_obs.unrealized_pnl {
                let pnl_f32: f32 = pnl.to_string().parse().unwrap_or(0.0);
                if pnl_f32 < -0.05 && self.current_obs.has_position {
                    return ContinuousAction::new(-1.0, 0.0, 1.0, 0.0, 0.0);
                }
            }
        }

        ContinuousAction::default()
    }

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
