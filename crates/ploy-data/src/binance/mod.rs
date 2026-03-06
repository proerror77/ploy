//! Binance data sources — spot prices, klines, and order book depth.

pub mod depth;
pub mod kline_ws;
pub mod klines;
pub mod ws;

pub use depth::{
    BinanceDepthStream, DepthUpdate, LobCache, LobSnapshot, LobUpdate, OrderBookState,
};
pub use kline_ws::{BinanceKlineBar, BinanceKlineWebSocket, KlineUpdate};
pub use klines::{BinanceKlineClient, Kline, VolatilityStats};
pub use ws::{BinanceWebSocket, PriceCache, PriceUpdate, SpotPrice};
