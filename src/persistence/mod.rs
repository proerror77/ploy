//! Persistence Layer for State Management and Recovery
//!
//! This module provides persistence infrastructure for crash recovery:
//! - Checkpoint service for periodic state snapshots
//! - Dead letter queue processor for failed operation retry
//! - Event store for event sourcing (audit trail and state replay)

pub mod checkpoint;
pub mod dlq_processor;
pub mod event_store;
mod market_persistence;
mod runtime_schema;

pub use checkpoint::{CheckpointConfig, CheckpointService, Checkpointable};
pub use dlq_processor::{DLQHandler, DLQProcessor, DLQProcessorConfig};
pub use event_store::{EventMetadata, EventStore, StoredEvent};
pub(crate) use market_persistence::{
    ensure_clob_trade_alerts_table, spawn_pm_token_settlement_persistence,
    spawn_polymarket_trade_persistence, spawn_polymarket_trade_persistence_from_collector_targets,
};
pub(crate) use runtime_schema::{
    ensure_accounts_table, ensure_agent_order_executions_table, ensure_binance_lob_ticks_table,
    ensure_binance_price_ticks_table, ensure_clob_orderbook_snapshots_table,
    ensure_clob_quote_ticks_table, ensure_coordinator_governance_policies_table,
    ensure_coordinator_governance_policy_history_table, ensure_pm_market_metadata_table,
    ensure_pm_token_settlements_table, ensure_risk_runtime_state_table, ensure_schema_repairs,
    ensure_strategy_observability_tables, upsert_account_from_config,
};
