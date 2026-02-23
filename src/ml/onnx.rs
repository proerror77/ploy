//! ONNX inference wrapper (pure Rust via `tract-onnx`).
//!
//! This is used for deployable DL/RL inference without Python in production.

use crate::error::{PloyError, Result};

use tract_onnx::prelude::*;

#[derive(Clone)]
pub struct OnnxModel {
    plan: TypedRunnableModel<TypedModel>,
    input_shape: Vec<usize>,
    output_dim: usize,
}

impl std::fmt::Debug for OnnxModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxModel")
            .field("input_shape", &self.input_shape)
            .field("output_dim", &self.output_dim)
            .finish()
    }
}

impl OnnxModel {
    /// Load an ONNX model and specialize it to a fixed `f32` tensor input.
    ///
    /// `input_shape` must include the batch dimension (typically `1`).
    pub fn load_for_tensor_input(path: &str, input_shape: &[usize]) -> Result<Self> {
        if input_shape.is_empty() {
            return Err(PloyError::Validation(
                "input_shape must have at least 1 dimension".to_string(),
            ));
        }
        if input_shape.iter().any(|d| *d == 0) {
            return Err(PloyError::Validation(
                "input_shape dimensions must all be > 0".to_string(),
            ));
        }

        let model = tract_onnx::onnx()
            .model_for_path(path)
            .map_err(|e| PloyError::Internal(format!("onnx load failed: {e}")))?;

        let mut shape = tvec!();
        for d in input_shape {
            shape.push(*d);
        }

        let model = model
            .with_input_fact(0, InferenceFact::dt_shape(f32::datum_type(), shape))
            .map_err(|e| PloyError::Internal(format!("onnx input fact failed: {e}")))?;

        let plan = model
            .into_optimized()
            .map_err(|e| PloyError::Internal(format!("onnx optimize failed: {e}")))?
            .into_runnable()
            .map_err(|e| PloyError::Internal(format!("onnx runnable failed: {e}")))?;

        // Infer output_dim by running a dummy forward pass.
        let dummy = tract_ndarray::ArrayD::<f32>::zeros(tract_ndarray::IxDyn(input_shape))
            .into_tvalue();
        let outputs = plan
            .run(tvec!(dummy))
            .map_err(|e| PloyError::Internal(format!("onnx run failed: {e}")))?;
        if outputs.is_empty() {
            return Err(PloyError::Internal("onnx produced no outputs".to_string()));
        }
        let out0 = &outputs[0];
        let arr = out0
            .to_array_view::<f32>()
            .map_err(|e| PloyError::Internal(format!("onnx output decode failed: {e}")))?;
        let output_dim = arr.len();
        if output_dim == 0 {
            return Err(PloyError::Internal(
                "onnx output has zero elements".to_string(),
            ));
        }

        Ok(Self {
            plan,
            input_shape: input_shape.to_vec(),
            output_dim,
        })
    }

    /// Load an ONNX model and specialize it to a fixed `[1, input_dim]` f32 input.
    pub fn load_for_vec_input(path: &str, input_dim: usize) -> Result<Self> {
        if input_dim == 0 {
            return Err(PloyError::Validation("input_dim must be > 0".to_string()));
        }
        Self::load_for_tensor_input(path, &[1, input_dim])
    }

    pub fn input_dim(&self) -> usize {
        self.input_shape.last().copied().unwrap_or(0)
    }

    pub fn output_dim(&self) -> usize {
        self.output_dim
    }

    pub fn input_shape(&self) -> &[usize] {
        &self.input_shape
    }

    pub fn input_elem_count(&self) -> usize {
        self.input_shape.iter().product()
    }

    /// Run inference on a single feature vector.
    pub fn predict(&self, input: &[f32]) -> Result<Vec<f32>> {
        let expected = self.input_elem_count();
        if input.len() != expected {
            return Err(PloyError::Validation(format!(
                "onnx input dim mismatch: got {}, expected {} (shape={:?})",
                input.len(),
                expected,
                self.input_shape
            )));
        }

        let tensor = tract_ndarray::ArrayD::<f32>::from_shape_vec(
            tract_ndarray::IxDyn(&self.input_shape),
            input.to_vec(),
        )
        .map_err(|e| PloyError::Internal(format!("onnx input reshape failed: {e}")))?
        .into_tvalue();

        let outputs = self
            .plan
            .run(tvec!(tensor))
            .map_err(|e| PloyError::Internal(format!("onnx run failed: {e}")))?;
        if outputs.is_empty() {
            return Err(PloyError::Internal("onnx produced no outputs".to_string()));
        }

        let arr = outputs[0]
            .to_array_view::<f32>()
            .map_err(|e| PloyError::Internal(format!("onnx output decode failed: {e}")))?;

        Ok(arr.iter().copied().collect())
    }

    pub fn predict_scalar(&self, input: &[f32]) -> Result<f32> {
        let out = self.predict(input)?;
        if out.len() != 1 {
            return Err(PloyError::Validation(format!(
                "onnx predict_scalar expects output_dim=1, got {}",
                out.len()
            )));
        }
        Ok(out[0])
    }
}
