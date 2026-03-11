use super::*;

impl RLCryptoAgent {
    /// Decay exploration rate.
    pub(super) fn decay_exploration(&mut self) {
        let decay = self.config.rl_config.training.exploration_decay;
        let min = self.config.rl_config.training.exploration_min;
        self.exploration_rate = (self.exploration_rate * decay).max(min);
    }

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
}
