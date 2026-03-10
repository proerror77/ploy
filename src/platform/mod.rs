//! Order Platform - legacy execution surface
//!
//! 提供舊版 queue/risk/execution 元件給 RL CLI 等兼容層使用。
//! 正式 live trading runtime 已由 coordinator 接管。

pub mod data_plane;
pub mod freshness;
mod market_persistence;
pub mod persistence_pipeline;
pub mod persistence_schema;
mod types;

pub use data_plane::{
    BinanceDataPlaneHandle, CryptoDataPlaneHandle, DataPlaneConfig, DataPlaneHealth,
    PlatformDataPlane, SourceHealth,
};
pub use freshness::{DataPlaneFreshness, DataSource};
pub(crate) use market_persistence::{
    ensure_clob_trade_alerts_table, spawn_pm_token_settlement_persistence,
    spawn_polymarket_trade_persistence, spawn_polymarket_trade_persistence_from_collector_targets,
};
pub use persistence_pipeline::{
    BinanceLobTick, BinancePriceTick, ChainlinkPriceTick, ClobOrderbookSnapshot, ClobQuoteTick,
    PersistenceConfig, PersistenceEvent, PersistencePipeline, PersistencePipelineHandle,
    PipelineStats,
};
pub use types::Domain;
