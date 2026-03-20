use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformConfig {
    pub listen_addr: String,
}

impl Default for PlatformConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:8081".to_string(),
        }
    }
}
