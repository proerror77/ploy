//! Trading Agents — pull-based agent implementations
//!
//! Each agent implements `TradingAgent` and owns its main loop.
//! Agents communicate with the Coordinator via `AgentContext`.

pub mod context;
pub mod crypto;
pub mod crypto_lob_ml;
pub mod crypto_rl_policy;
pub mod politics;
pub mod sports;
pub mod traits;

pub use context::AgentContext;
pub use crypto::{CryptoTradingAgent, CryptoTradingConfig};
pub use crypto_lob_ml::{CryptoLobMlAgent, CryptoLobMlConfig, CryptoLobMlExitMode};
pub use crypto_rl_policy::{CryptoRlPolicyAgent, CryptoRlPolicyConfig};
pub use politics::{PoliticsTradingAgent, PoliticsTradingConfig};
pub use sports::{SportsTradingAgent, SportsTradingConfig};
pub use traits::{AgentConfig, TradingAgent};
