//! Persistence Layer for State Management and Recovery
//!
//! This module provides persistence infrastructure for crash recovery:
//! - Checkpoint service for periodic state snapshots
//! - Dead letter queue processor for failed operation retry
//! - Event store for event sourcing (audit trail and state replay)

pub mod checkpoint;
pub mod dlq_processor;
pub mod event_store;

pub use checkpoint::{CheckpointConfig, CheckpointService, Checkpointable};
pub use dlq_processor::{DLQHandler, DLQProcessor, DLQProcessorConfig};
pub use event_store::{EventMetadata, EventStore, StoredEvent};
