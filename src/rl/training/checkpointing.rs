//! Model Checkpointing
//!
//! Save and load model weights for persistence.

use std::fs;
use std::path::{Path, PathBuf};

use burn::prelude::*;
use burn::record::{FullPrecisionSettings, NamedMpkFileRecorder};
use tracing::{info, warn};

/// Checkpointer for saving and loading models
pub struct Checkpointer {
    /// Directory for checkpoints
    checkpoint_dir: PathBuf,
    /// Maximum checkpoints to keep
    max_checkpoints: usize,
}

impl Checkpointer {
    /// Create a new checkpointer
    pub fn new<P: AsRef<Path>>(checkpoint_dir: P, max_checkpoints: usize) -> Self {
        let checkpoint_dir = checkpoint_dir.as_ref().to_path_buf();

        // Create directory if it doesn't exist
        if !checkpoint_dir.exists() {
            if let Err(e) = fs::create_dir_all(&checkpoint_dir) {
                warn!("Failed to create checkpoint directory: {}", e);
            }
        }

        Self {
            checkpoint_dir,
            max_checkpoints,
        }
    }

    /// Get checkpoint path for a given name
    pub fn checkpoint_path(&self, name: &str) -> PathBuf {
        self.checkpoint_dir.join(format!("{}.mpk", name))
    }

    /// Save a model
    pub fn save<B, M>(&self, model: &M, name: &str) -> Result<PathBuf, String>
    where
        B: Backend,
        M: Module<B>,
    {
        let path = self.checkpoint_path(name);

        let recorder = NamedMpkFileRecorder::<FullPrecisionSettings>::new();
        model
            .clone()
            .save_file(&path, &recorder)
            .map_err(|e| format!("Failed to save checkpoint: {}", e))?;

        info!("Saved checkpoint to {:?}", path);

        // Cleanup old checkpoints
        self.cleanup_old_checkpoints();

        Ok(path)
    }

    /// Load a model
    pub fn load<B, M>(&self, name: &str, _device: &B::Device) -> Result<M, String>
    where
        B: Backend,
        M: Module<B>,
    {
        let path = self.checkpoint_path(name);

        if !path.exists() {
            return Err(format!("Checkpoint not found: {:?}", path));
        }

        // Note: Module::load_file requires the record type to match
        // This is a simplified version - actual implementation may need adjustment
        Err("Load not yet implemented - requires record type".to_string())
    }

    /// List available checkpoints
    pub fn list_checkpoints(&self) -> Vec<String> {
        let mut checkpoints = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.checkpoint_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(".mpk") {
                        checkpoints.push(name.trim_end_matches(".mpk").to_string());
                    }
                }
            }
        }

        checkpoints.sort();
        checkpoints
    }

    /// Get latest checkpoint name
    pub fn latest_checkpoint(&self) -> Option<String> {
        self.list_checkpoints().into_iter().last()
    }

    /// Cleanup old checkpoints keeping only max_checkpoints
    fn cleanup_old_checkpoints(&self) {
        let checkpoints = self.list_checkpoints();

        if checkpoints.len() <= self.max_checkpoints {
            return;
        }

        let to_remove = checkpoints.len() - self.max_checkpoints;
        for name in checkpoints.into_iter().take(to_remove) {
            let path = self.checkpoint_path(&name);
            if let Err(e) = fs::remove_file(&path) {
                warn!("Failed to remove old checkpoint {:?}: {}", path, e);
            } else {
                info!("Removed old checkpoint: {}", name);
            }
        }
    }

    /// Check if a checkpoint exists
    pub fn exists(&self, name: &str) -> bool {
        self.checkpoint_path(name).exists()
    }
}

impl Default for Checkpointer {
    fn default() -> Self {
        Self::new("./checkpoints", 5)
    }
}

/// Generate a checkpoint name with timestamp
pub fn timestamped_name(prefix: &str) -> String {
    let now = chrono::Utc::now();
    format!("{}_{}", prefix, now.format("%Y%m%d_%H%M%S"))
}

/// Generate a checkpoint name with episode number
pub fn episode_name(prefix: &str, episode: usize) -> String {
    format!("{}_ep{:06}", prefix, episode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    #[test]
    fn test_checkpoint_path() {
        let checkpointer = Checkpointer::new(temp_dir().join("test_ckpt"), 5);
        let path = checkpointer.checkpoint_path("model_v1");

        assert!(path.to_string_lossy().contains("model_v1.mpk"));
    }

    #[test]
    fn test_timestamped_name() {
        let name = timestamped_name("ppo");
        assert!(name.starts_with("ppo_"));
        assert!(name.len() > 10);
    }

    #[test]
    fn test_episode_name() {
        let name = episode_name("ppo", 100);
        assert_eq!(name, "ppo_ep000100");
    }
}
