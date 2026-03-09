//! Trading Agents — pull-based agent implementations
//!
//! Legacy pull-based agent runtime.
//!
//! Crypto and governance runtimes still live here. Sports and politics now run
//! through the canonical managed strategy runtime; their modules remain only as
//! config compatibility shims for bootstrap deserialization.
//! Agents communicate with the Coordinator via `AgentContext`.

pub mod context;
pub mod crypto;
pub mod crypto_lob_ml;
pub mod crypto_rl_policy;
pub mod governance_context;
pub mod openclaw;
pub mod politics;
pub mod sports;
pub mod traits;

pub use context::AgentContext;
pub use crypto::{CryptoEntryMode, CryptoTradingAgent, CryptoTradingConfig};
pub use crypto_lob_ml::{
    CryptoLobMlAgent, CryptoLobMlConfig, CryptoLobMlEntrySidePolicy, CryptoLobMlExitMode,
};
pub use crypto_rl_policy::{CryptoRlPolicyAgent, CryptoRlPolicyConfig};
pub use governance_context::GovernanceContext;
pub use openclaw::{OpenClawAgent, OpenClawConfig};
pub use politics::PoliticsTradingConfig;
pub use sports::SportsTradingConfig;
pub use traits::{AgentConfig, GovernanceAgent, TradingAgent};
