//! Order Platform - legacy execution surface
//!
//! 提供舊版 queue/risk/execution 元件給 RL CLI 等兼容層使用。
//! 正式 live trading runtime 已由 coordinator 接管。

pub mod persistence_pipeline;
pub mod persistence_schema;
mod types;

pub use crate::data_plane::{
    BinanceDataPlaneHandle, CryptoDataPlaneHandle, DataPlaneConfig, DataPlaneHealth,
    PlatformDataPlane, SourceHealth,
};
pub use crate::data_plane::{DataPlaneFreshness, DataSource};
pub use persistence_pipeline::{
    BinanceLobTick, BinancePriceTick, ChainlinkPriceTick, ClobOrderbookSnapshot, ClobQuoteTick,
    PersistenceConfig, PersistenceEvent, PersistencePipeline, PersistencePipelineHandle,
    PipelineStats,
};
pub use types::Domain;
