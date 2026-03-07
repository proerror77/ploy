use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposableCryptoSpec {
    #[serde(default)]
    pub signal_blocks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredStrategySpec {
    pub strategy_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginSpec {
    ComposableCrypto(ComposableCryptoSpec),
    RegisteredStrategy(RegisteredStrategySpec),
}
