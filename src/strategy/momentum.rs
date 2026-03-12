//! Momentum strategy for Polymarket trading
//!
//! Implements the "gabagool22" style strategy:
//! 1. Monitor CEX (Binance) for BTC/ETH/SOL price movements
//! 2. When spot price moves significantly, Polymarket odds lag
//! 3. Enter the side that should win before odds adjust
//! 4. Exit via take-profit, stop-loss, trailing stop, or hold to resolution

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{debug, error, info, trace, warn};

use crate::adapters::{
    ChainlinkPriceCache, ChainlinkUpdate, GammaEventInfo, PolymarketClient, PriceCache,
    PriceUpdate, QuoteCache, QuoteUpdate, SpotPrice,
};
use crate::config::RiskConfig;
use crate::data_plane::CryptoDataPlaneHandle;
use crate::domain::{OrderRequest, Side};
use crate::error::Result;
use crate::strategy::OrderExecutor;
use crate::strategy::crypto::{
    horizon_for_series as crypto_horizon_for_series, known_binance_symbols,
    series_ids_for_symbol as crypto_series_ids_for_symbol,
};
use crate::strategy::dump_hedge::{DumpHedgeConfig, DumpHedgeEngine};
use crate::strategy::fee_model::FeeModel;
use crate::strategy::fund_manager::{FundManager, PositionSizeResult};
use crate::strategy::probability;
use crate::strategy::volatility::{EventTracker, VolatilityConfig, VolatilityDetector};

mod best_edge;
mod config;
mod constructor;
mod detector;
mod entry_runtime;
mod matcher;
mod pm_runtime;
mod position_exit;
mod runtime_state;
#[cfg(test)]
mod tests;
mod trade_flow;

use self::best_edge::WindowRiskTracker;
use self::runtime_state::DailyTradeCounter;
pub use config::{Direction, ExitConfig, MomentumConfig};
pub use detector::{MomentumDetector, MomentumSignal};
pub use matcher::{EventInfo, EventMatcher};
pub use position_exit::{ExitManager, ExitReason, Position};

// ============================================================================
// Momentum Engine
// ============================================================================

/// Main engine orchestrating the momentum strategy
pub struct MomentumEngine {
    config: MomentumConfig,
    #[allow(dead_code)]
    exit_config: ExitConfig,
    detector: MomentumDetector,
    exit_manager: ExitManager,
    event_matcher: EventMatcher,
    executor: OrderExecutor,
    positions: Arc<RwLock<HashMap<String, Position>>>,
    last_trade_time: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
    daily_trades: Arc<RwLock<DailyTradeCounter>>,
    dry_run: bool,
    // Volatility-based event tracking
    volatility_detector: VolatilityDetector,
    event_tracker: Arc<RwLock<EventTracker>>,
    // Fund management
    fund_manager: Option<Arc<FundManager>>,
    // Auto-claimer for winning positions
    #[cfg(feature = "claimer_daemon")]
    claimer: Option<Arc<super::claimer::AutoClaimer>>,
    // Trade logger for persistent records
    trade_logger: Option<Arc<super::trade_logger::TradeLogger>>,
    // Window risk tracker for cross-symbol exposure limits
    window_tracker: Arc<RwLock<WindowRiskTracker>>,
    // Binance LOB cache for OBI signals
    lob_cache: Option<Arc<crate::collector::LobCache>>,
    // K-line client for historical volatility
    kline_client: Option<Arc<crate::collector::BinanceKlineClient>>,
    // Dump & Hedge strategy engine
    dump_hedge: Option<Arc<DumpHedgeEngine>>,
    // Serialize entry path to avoid duplicate orders for the same event under concurrent updates.
    entry_mutex: Arc<Mutex<()>>,
    // === Directional prediction infrastructure ===
    // Dynamic fee model for cost-aware entry
    fee_model: FeeModel,
    // Chainlink price cache for ground truth oracle prices
    chainlink_cache: Option<ChainlinkPriceCache>,
    // Entry threshold for EV_net (directional mode)
    entry_threshold: f64,
}

impl MomentumEngine {
    /// Get trade logger reference
    pub fn trade_logger(&self) -> Option<&Arc<super::trade_logger::TradeLogger>> {
        self.trade_logger.as_ref()
    }

    /// Get event matcher reference
    pub fn event_matcher(&self) -> &EventMatcher {
        &self.event_matcher
    }
}
