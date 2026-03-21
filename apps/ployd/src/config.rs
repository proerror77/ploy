use secrecy::SecretString;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct PlatformConfig {
    pub listen_addr: String,
    pub admin_token: Option<SecretString>,
    pub sidecar_token: Option<SecretString>,
    pub auth_cookie_secret: SecretString,
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
            admin_token: None,
            sidecar_token: None,
            auth_cookie_secret: generate_cookie_secret(),
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
        if let Ok(value) = std::env::var("PLOY_ADMIN_TOKEN") {
            if !value.trim().is_empty() {
                config.admin_token = Some(SecretString::from(value));
            }
        } else if let Ok(value) = std::env::var("PLOY_API_ADMIN_TOKEN") {
            if !value.trim().is_empty() {
                config.admin_token = Some(SecretString::from(value));
            }
        }
        if let Ok(value) = std::env::var("PLOY_SIDECAR_AUTH_TOKEN") {
            if !value.trim().is_empty() {
                config.sidecar_token = Some(SecretString::from(value));
            }
        }
        if let Ok(value) = std::env::var("PLOY_API_AUTH_COOKIE_SECRET") {
            if !value.trim().is_empty() {
                config.auth_cookie_secret = SecretString::from(value);
            }
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

fn generate_cookie_secret() -> SecretString {
    use rand::RngCore;

    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    SecretString::from(bytes_to_hex(&bytes))
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::PlatformConfig;

    #[test]
    fn default_paths_match_workspace_contract() {
        let config = PlatformConfig::default();
        assert!(config.admin_token.is_none());
        assert!(config.sidecar_token.is_none());
        assert!(!config.auth_cookie_secret.expose_secret().is_empty());
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
