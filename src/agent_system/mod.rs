//! Canonical agent namespace for the whole runtime.
//!
//! This module provides a single entry point to avoid mixed imports from:
//! - `crate::agent` (AI/advisory clients)
//! - `crate::agents` (coordinator runtime trading agents)
//! - `crate::platform::agents` (legacy platform agents)

/// AI/advisory agents and provider clients (formerly `crate::agent::*`).
pub mod ai {
    pub use crate::agent::*;
}

/// Coordinator-native runtime trading agents (formerly `crate::agents::*`).
pub mod runtime {
    pub use crate::agents::*;
}

/// Legacy platform agent implementations kept for compatibility.
pub mod legacy_platform {
    pub use crate::platform::agents::*;
}
