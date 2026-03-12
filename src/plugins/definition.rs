use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::platform::Domain;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    ComposableCrypto,
    RegisteredStrategy,
}

impl PluginKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ComposableCrypto => "composable_crypto",
            Self::RegisteredStrategy => "registered_strategy",
        }
    }
}

impl std::fmt::Display for PluginKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PluginKind {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "composable_crypto" => Ok(Self::ComposableCrypto),
            "registered_strategy" => Ok(Self::RegisteredStrategy),
            other => Err(format!(
                "unknown plugin kind: {other}; expected composable_crypto or registered_strategy"
            )),
        }
    }
}

impl<'de> Deserialize<'de> for PluginKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDefinition {
    pub plugin_id: String,
    pub kind: PluginKind,
    pub version: String,
    pub domain: Domain,
}
