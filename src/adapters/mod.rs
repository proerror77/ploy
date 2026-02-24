#[cfg(feature = "api")]
pub mod api_server;
pub mod binance_kline_ws;
pub mod binance_ws;
pub mod feishu;
pub mod kalshi_rest;
pub mod onchain_indexer;
pub mod polymarket_clob;
pub mod polymarket_official;
pub mod polymarket_ws;
pub mod postgres;
pub mod transaction_manager;

#[cfg(feature = "api")]
pub use api_server::{
    start_api_server, start_api_server_background, start_api_server_platform_background,
};
pub use binance_kline_ws::{BinanceKlineBar, BinanceKlineWebSocket, KlineUpdate};
pub use binance_ws::{BinanceWebSocket, PriceCache, PriceUpdate, SpotPrice};
pub use feishu::FeishuNotifier;
pub use kalshi_rest::KalshiClient;
pub use polymarket_clob::{
    AccountSummary, BalanceResponse, GammaEventInfo, MarketResponse, MarketSummary, OrderResponse,
    PolymarketClient, PositionResponse, TradeResponse,
};
pub use polymarket_ws::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerState, DisplayQuote, PolymarketWebSocket,
    QuoteCache, QuoteUpdate,
};
pub use postgres::{
    DailyMetrics, IncompleteCycle, OrphanedOrder, PersistedState, PostgresStore, RecoverySummary,
};
pub use transaction_manager::{DLQEntry, ManagedTransaction, TransactionManager, TransactionScope};

// Official Polymarket SDK re-export
pub use polymarket_official::sdk as polymarket_sdk;
