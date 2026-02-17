//! Lightweight ML utilities (deploy-safe inference).
//!
//! This module is intentionally dependency-light so it can run 24/7 on small
//! EC2 instances without GPU/toolchain complexity.

pub mod dense;
#[cfg(feature = "onnx")]
pub mod onnx;

pub use dense::{Activation, DenseLayer, DenseNetwork};
#[cfg(feature = "onnx")]
pub use onnx::OnnxModel;
