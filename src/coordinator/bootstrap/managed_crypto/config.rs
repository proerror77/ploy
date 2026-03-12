use crate::strategy::crypto_lob_ml::CryptoLobMlConfig;
#[cfg(feature = "rl")]
use crate::strategy::crypto_rl_policy::CryptoRlPolicyConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedCryptoRuntimeConfig {
    #[serde(default)]
    pub enable_lob_ml: bool,
    #[serde(default)]
    pub lob_ml: CryptoLobMlConfig,
    #[cfg(feature = "rl")]
    #[serde(default)]
    pub enable_rl_policy: bool,
    #[cfg(feature = "rl")]
    #[serde(default)]
    pub rl_policy: CryptoRlPolicyConfig,
}

impl Default for ManagedCryptoRuntimeConfig {
    fn default() -> Self {
        Self {
            enable_lob_ml: false,
            lob_ml: CryptoLobMlConfig::default(),
            #[cfg(feature = "rl")]
            enable_rl_policy: false,
            #[cfg(feature = "rl")]
            rl_policy: CryptoRlPolicyConfig::default(),
        }
    }
}
