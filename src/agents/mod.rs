//! Trading Agents — pull-based compatibility implementations
//!
//! New live strategy work must go through the canonical `Strategy` runtime.
//! These modules remain available for transitional compatibility and niche
//! governance/runtime-adapter use only.

pub mod context;
pub mod crypto;
pub mod governance_context;
pub mod openclaw;
pub mod traits;
