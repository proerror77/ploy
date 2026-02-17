//! Dense neural network inference (CPU-only).
//!
//! Supports small MLPs loaded from JSON for:
//! - binary classification (e.g., p_up prediction)
//! - policy/value inference (RL) via vector outputs
//!
//! Design goals:
//! - Stable, deterministic, dependency-light.
//! - Explicit shape validation (fail fast, caller can fallback).

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::{PloyError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activation {
    Linear,
    Relu,
    Tanh,
    Sigmoid,
}

impl Default for Activation {
    fn default() -> Self {
        Self::Linear
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenseLayer {
    /// Weights shape: [out_dim][in_dim]
    pub weights: Vec<Vec<f64>>,
    /// Bias shape: [out_dim]
    pub bias: Vec<f64>,
    #[serde(default)]
    pub activation: Activation,
}

impl DenseLayer {
    fn in_dim(&self) -> usize {
        self.weights.first().map(|r| r.len()).unwrap_or(0)
    }

    fn out_dim(&self) -> usize {
        self.weights.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenseNetwork {
    /// Expected input dimension.
    pub input_dim: usize,

    /// Optional z-score normalization.
    #[serde(default)]
    pub input_mean: Option<Vec<f64>>,
    #[serde(default)]
    pub input_std: Option<Vec<f64>>,

    pub layers: Vec<DenseLayer>,

    /// Optional free-form metadata (versioning, training info, etc).
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl DenseNetwork {
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = std::fs::read_to_string(&path)?;
        let model: Self = serde_json::from_str(&content)?;
        model.validate().map_err(PloyError::Validation)?;
        Ok(model)
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.input_dim == 0 {
            return Err("input_dim must be > 0".to_string());
        }
        if self.layers.is_empty() {
            return Err("layers must not be empty".to_string());
        }
        if let (Some(mean), Some(std)) = (&self.input_mean, &self.input_std) {
            if mean.len() != self.input_dim {
                return Err(format!(
                    "input_mean length {} != input_dim {}",
                    mean.len(),
                    self.input_dim
                ));
            }
            if std.len() != self.input_dim {
                return Err(format!(
                    "input_std length {} != input_dim {}",
                    std.len(),
                    self.input_dim
                ));
            }
            if std.iter().any(|v| !v.is_finite() || *v <= 0.0) {
                return Err("input_std must be finite and > 0".to_string());
            }
        } else if self.input_mean.is_some() || self.input_std.is_some() {
            return Err("input_mean and input_std must be provided together".to_string());
        }

        let mut expected_in = self.input_dim;
        for (idx, layer) in self.layers.iter().enumerate() {
            if layer.out_dim() == 0 {
                return Err(format!("layer[{idx}] out_dim must be > 0"));
            }
            if layer.bias.len() != layer.out_dim() {
                return Err(format!(
                    "layer[{idx}] bias len {} != out_dim {}",
                    layer.bias.len(),
                    layer.out_dim()
                ));
            }
            for (r, row) in layer.weights.iter().enumerate() {
                if row.len() != expected_in {
                    return Err(format!(
                        "layer[{idx}] weights row {r} len {} != expected in_dim {expected_in}",
                        row.len()
                    ));
                }
                if row.iter().any(|v| !v.is_finite()) {
                    return Err(format!("layer[{idx}] weights contain non-finite values"));
                }
            }
            if layer.bias.iter().any(|v| !v.is_finite()) {
                return Err(format!("layer[{idx}] bias contain non-finite values"));
            }
            expected_in = layer.out_dim();
        }
        Ok(())
    }

    pub fn output_dim(&self) -> usize {
        self.layers.last().map(|l| l.out_dim()).unwrap_or(0)
    }

    pub fn forward(&self, input: &[f64]) -> Result<Vec<f64>> {
        if input.len() != self.input_dim {
            return Err(PloyError::Validation(format!(
                "DenseNetwork input dim mismatch: got {}, expected {}",
                input.len(),
                self.input_dim
            )));
        }

        let mut x: Vec<f64> = input.to_vec();

        if let (Some(mean), Some(std)) = (&self.input_mean, &self.input_std) {
            for i in 0..x.len() {
                let denom = std[i].max(1e-12);
                x[i] = (x[i] - mean[i]) / denom;
            }
        }

        for layer in &self.layers {
            let out_dim = layer.out_dim();
            let in_dim = layer.in_dim();

            let mut y = vec![0.0_f64; out_dim];
            for o in 0..out_dim {
                let mut sum = layer.bias[o];
                // weights[o] is the o-th row (len = in_dim)
                let row = &layer.weights[o];
                debug_assert_eq!(row.len(), in_dim);
                for i in 0..in_dim {
                    sum += row[i] * x[i];
                }
                y[o] = apply_activation(sum, layer.activation);
            }
            x = y;
        }

        Ok(x)
    }

    pub fn forward_scalar(&self, input: &[f64]) -> Result<f64> {
        let out = self.forward(input)?;
        if out.len() != 1 {
            return Err(PloyError::Validation(format!(
                "DenseNetwork forward_scalar expects output_dim=1, got {}",
                out.len()
            )));
        }
        Ok(out[0])
    }
}

fn apply_activation(x: f64, act: Activation) -> f64 {
    match act {
        Activation::Linear => x,
        Activation::Relu => x.max(0.0),
        Activation::Tanh => x.tanh(),
        Activation::Sigmoid => sigmoid(x),
    }
}

fn sigmoid(x: f64) -> f64 {
    // Numerically-stable sigmoid.
    if x >= 0.0 {
        let z = (-x).exp();
        1.0 / (1.0 + z)
    } else {
        let z = x.exp();
        z / (1.0 + z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_scalar_sigmoid() {
        let net = DenseNetwork {
            input_dim: 2,
            input_mean: None,
            input_std: None,
            layers: vec![DenseLayer {
                weights: vec![vec![1.0, 2.0]],
                bias: vec![0.0],
                activation: Activation::Sigmoid,
            }],
            metadata: serde_json::json!({}),
        };
        net.validate().unwrap();

        let p0 = net.forward_scalar(&[0.0, 0.0]).unwrap();
        assert!((p0 - 0.5).abs() < 1e-12);

        let p1 = net.forward_scalar(&[1.0, 0.0]).unwrap();
        assert!(p1 > 0.5);
    }

    #[test]
    fn validates_shapes() {
        let bad = DenseNetwork {
            input_dim: 3,
            input_mean: None,
            input_std: None,
            layers: vec![DenseLayer {
                weights: vec![vec![1.0, 2.0]], // in_dim mismatch
                bias: vec![0.0],
                activation: Activation::Linear,
            }],
            metadata: serde_json::json!({}),
        };
        assert!(bad.validate().is_err());
    }
}
