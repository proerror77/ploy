//! Governance agents and coordinator-facing agent helpers.
//!
//! Live trading now runs through the canonical `Strategy` runtime. The
//! remaining `agents/*` surface is governance-oriented.

pub mod governance_context;
pub mod openclaw;
pub mod traits;
