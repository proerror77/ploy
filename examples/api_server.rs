// 简单的 API 服务器启动示例
//
// 使用方法:
// 1. 设置环境变量: export DATABASE_URL="postgresql://localhost/ploy"
// 2. 运行: cargo run --example api_server

#[cfg(feature = "api")]
use ploy::adapters::{start_api_server, PostgresStore};
#[cfg(feature = "api")]
use ploy::api::state::StrategyConfigState;
#[cfg(feature = "api")]
use std::sync::Arc;

#[cfg(not(feature = "api"))]
fn main() {
    eprintln!("This example requires the `api` feature.");
    eprintln!("Try: cargo run --example api_server --features api");
}

#[cfg(feature = "api")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("info,ploy=debug")
        .init();

    // 从环境变量获取数据库 URL
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://localhost/ploy".to_string());

    println!("🔌 Connecting to database: {}", database_url);

    // 连接数据库
    let store = Arc::new(PostgresStore::new(&database_url, 10).await?);

    println!("✅ Database connected");

    // 配置策略参数
    let config = StrategyConfigState {
        symbols: vec![
            "BTCUSDT".to_string(),
            "ETHUSDT".to_string(),
            "SOLUSDT".to_string(),
        ],
        min_move: 0.15,
        max_entry: 45.0,
        shares: 100,
        predictive: false,
        exit_edge_floor: Some(0.20),
        exit_price_band: Some(0.12),
        time_decay_exit_secs: None,
        liquidity_exit_spread_bps: None,
    };

    println!("🚀 Starting API server on http://0.0.0.0:8080");
    println!("📡 WebSocket available at ws://0.0.0.0:8080/ws");
    println!();
    println!("API Endpoints:");
    println!("  GET  /api/stats/today");
    println!("  GET  /api/stats/pnl?hours=24");
    println!("  GET  /api/trades");
    println!("  GET  /api/positions");
    println!("  GET  /api/system/status");
    println!("  POST /api/system/start");
    println!("  POST /api/system/stop");
    println!("  GET  /api/config");
    println!("  PUT  /api/config");
    println!("  GET  /api/security/events");
    println!();

    // 启动 API 服务器
    start_api_server(store, 8080, config).await?;

    Ok(())
}
