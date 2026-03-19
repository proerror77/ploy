use super::*;

impl RLCryptoAgent {
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
}
