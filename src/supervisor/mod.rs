//! Supervisor Layer for System Monitoring and Recovery
//!
//! This module provides automated supervision infrastructure:
//! - Watchdog for heartbeat monitoring and auto-restart
//! - Alert manager for Feishu integration
//! - Playbook for recovery actions

pub mod alert_manager;
pub mod playbook;
pub mod watchdog;

pub use alert_manager::{AlertLevel, AlertManager, AlertManagerConfig};
pub use playbook::{RecoveryAction, RecoveryPlaybook};
pub use watchdog::{Watchdog, WatchdogConfig, ComponentHealth};
