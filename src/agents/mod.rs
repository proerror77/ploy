//! Trading Agents — pull-based compatibility implementations
//!
//! New live strategy work must go through the canonical `Strategy` runtime.
//! These modules remain available for transitional compatibility and niche
//! governance/runtime-adapter use only.

pub mod context;
pub mod crypto;
pub mod crypto_lob_ml;
pub mod crypto_rl_policy;
pub mod governance_context;
pub mod openclaw;
pub mod politics;
pub mod sports;
pub mod traits;

pub use crypto::CryptoTradingConfig;
pub use crypto_lob_ml::{CryptoLobMlConfig, CryptoLobMlEntrySidePolicy, CryptoLobMlExitMode};
pub use crypto_rl_policy::CryptoRlPolicyConfig;
pub use openclaw::{OpenClawAgent, OpenClawConfig};
pub use politics::PoliticsTradingConfig;
pub use sports::SportsTradingConfig;
