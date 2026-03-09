//! Legacy runtime compatibility surface.
//!
//! These types support the old `DomainAgent` + `EventRouter` + `OrderPlatform`
//! runtime that is still used by CLI-only compatibility paths such as the RL
//! agent command. Canonical live trading should go through the coordinator
//! runtime instead of importing these from the main `platform` public surface.

pub use super::platform::{OrderPlatform, PlatformConfig};
pub use super::router::{AgentSubscription, EventRouter};
pub use super::traits::DomainAgent;
