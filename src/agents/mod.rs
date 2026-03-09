//! Trading Agents — pull-based agent implementations
//!
//! Legacy pull-based agent runtime.
//!
//! Crypto and governance runtimes still live here. Sports and politics now run
//! through the canonical managed strategy runtime; their modules remain only as
//! config compatibility shims for bootstrap deserialization.
//! Agents communicate with the Coordinator via `AgentContext`.
//!
//! Legacy crypto trading-agent types are intentionally not re-exported from
//! this module root. Callers should use explicit module paths so the remaining
//! compatibility surface stays narrow and obvious.

pub mod context;
pub mod crypto;
pub mod crypto_lob_ml;
pub mod crypto_rl_policy;
pub mod governance_context;
pub mod openclaw;
pub mod traits;

pub use context::AgentContext;
pub use crypto::{CryptoEntryMode, CryptoTradingConfig};
pub use governance_context::GovernanceContext;
pub use openclaw::{OpenClawAgent, OpenClawConfig};
pub use traits::GovernanceAgent;
