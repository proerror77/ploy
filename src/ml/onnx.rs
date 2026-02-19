//! ONNX inference wrapper (pure Rust via `tract-onnx`).
//!
//! This is used for deployable DL/RL inference without Python in production.

use crate::error::{PloyError, Result};

use tract_onnx::prelude::*;

#[derive(Clone)]
pub struct OnnxModel {
    plan: TypedRunnableModel<TypedModel>,
    input_dim: usize,
    output_dim: usize,
}

impl std::fmt::Debug for OnnxModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxModel")
            .field("input_dim", &self.input_dim)
            .field("output_dim", &self.output_dim)
            .finish()
    }
}

impl OnnxModel {
    /// Load an ONNX model and specialize it to a fixed `[1, input_dim]` f32 input.
    pub fn load_for_vec_input(path: &str, input_dim: usize) -> Result<Self> {
        if input_dim == 0 {
            return Err(PloyError::Validation("input_dim must be > 0".to_string()));
        }

        let model = tract_onnx::onnx()
            .model_for_path(path)
            .map_err(|e| PloyError::Internal(format!("onnx load failed: {e}")))?;

        let model = model
            .with_input_fact(
                0,
                InferenceFact::dt_shape(f32::datum_type(), tvec!(1, input_dim)),
            )
            .map_err(|e| PloyError::Internal(format!("onnx input fact failed: {e}")))?;

        let plan = model
            .into_optimized()
            .map_err(|e| PloyError::Internal(format!("onnx optimize failed: {e}")))?
            .into_runnable()
            .map_err(|e| PloyError::Internal(format!("onnx runnable failed: {e}")))?;

        // Infer output_dim by running a dummy forward pass.
        let dummy = tract_ndarray::Array2::<f32>::zeros((1, input_dim)).into_tvalue();
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
            input_dim,
            output_dim,
        })
    }

    pub fn input_dim(&self) -> usize {
        self.input_dim
    }

    pub fn output_dim(&self) -> usize {
        self.output_dim
    }

    /// Run inference on a single feature vector.
    pub fn predict(&self, input: &[f32]) -> Result<Vec<f32>> {
        if input.len() != self.input_dim {
            return Err(PloyError::Validation(format!(
                "onnx input dim mismatch: got {}, expected {}",
                input.len(),
                self.input_dim
            )));
        }

        let tensor =
            tract_ndarray::Array2::<f32>::from_shape_vec((1, self.input_dim), input.to_vec())
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
