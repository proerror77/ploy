use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformConfig {
    pub listen_addr: String,
    pub registry_file: PathBuf,
    pub runtime_root: PathBuf,
    pub status_file: PathBuf,
    pub deployment_status_file: PathBuf,
    pub trading_state_file: PathBuf,
    pub tick_interval_ms: u64,
}

impl Default for PlatformConfig {
    fn default() -> Self {
        let runtime_root = PathBuf::from("run/platform");
        Self {
            listen_addr: "127.0.0.1:8081".to_string(),
            registry_file: PathBuf::from("data/state/deployments.json"),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            runtime_root,
            tick_interval_ms: 1_000,
        }
    }
}

impl PlatformConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(value) = std::env::var("PLOY_LISTEN_ADDR") {
            config.listen_addr = value;
        }
        if let Ok(value) = std::env::var("PLOY_DEPLOYMENTS_FILE") {
            config.registry_file = PathBuf::from(value);
        }
        if let Ok(value) = std::env::var("PLOY_RUNTIME_ROOT") {
            config.runtime_root = PathBuf::from(value);
        }
        if let Ok(value) = std::env::var("PLOY_SYSTEM_STATUS_FILE") {
            config.status_file = PathBuf::from(value);
        } else {
            config.status_file = config.runtime_root.join("system-status.json");
        }
        if let Ok(value) = std::env::var("PLOY_DEPLOYMENT_STATUS_FILE") {
            config.deployment_status_file = PathBuf::from(value);
        } else {
            config.deployment_status_file = config.runtime_root.join("deployments.json");
        }
        if let Ok(value) = std::env::var("PLOY_TRADING_STATE_FILE") {
            config.trading_state_file = PathBuf::from(value);
        } else {
            config.trading_state_file = config.runtime_root.join("trading-state.json");
        }
        if let Ok(value) = std::env::var("PLOY_TICK_INTERVAL_MS") {
            if let Ok(parsed) = value.parse() {
                config.tick_interval_ms = parsed;
            }
        }

        config
    }
}

#[cfg(test)]
mod tests {
    use super::PlatformConfig;

    #[test]
    fn default_paths_match_workspace_contract() {
        let config = PlatformConfig::default();
        assert_eq!(
            config.registry_file.to_string_lossy(),
            "data/state/deployments.json"
        );
        assert_eq!(config.runtime_root.to_string_lossy(), "run/platform");
        assert_eq!(
            config.status_file.to_string_lossy(),
            "run/platform/system-status.json"
        );
        assert_eq!(
            config.deployment_status_file.to_string_lossy(),
            "run/platform/deployments.json"
        );
        assert_eq!(
            config.trading_state_file.to_string_lossy(),
            "run/platform/trading-state.json"
        );
        assert_eq!(config.tick_interval_ms, 1_000);
    }
}
