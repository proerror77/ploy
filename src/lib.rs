pub mod adapters;
pub mod agent;
pub mod cli;
pub mod collector;
pub mod config;
pub mod domain;
pub mod error;
pub mod platform;
pub mod services;
pub mod signing;
pub mod strategy;
pub mod tui;

// Reinforcement Learning module (optional, enabled with "rl" feature)
#[cfg(feature = "rl")]
pub mod rl;

pub use agent::{AdvisoryAgent, AutonomousAgent, AutonomousConfig, ClaudeAgentClient};
pub use collector::{BinanceDepthStream, LobCache, LobSnapshot, SyncCollector, SyncCollectorConfig};
pub use config::AppConfig;
pub use error::{PloyError, Result};
pub use platform::{
    OrderPlatform, PlatformConfig, DomainAgent, AgentStatus, Domain,
    OrderIntent, ExecutionReport, RiskGate, EventRouter,
};
pub use signing::Wallet;

// RL exports (when feature enabled)
#[cfg(feature = "rl")]
pub use rl::{RLConfig, RLStrategy};
