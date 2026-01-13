# 🔌 后端 API 实现要求

## 概述

前端 React 应用已完成，现在需要在 Rust 后端添加以下 HTTP API 和 WebSocket 服务。

---

## HTTP API 端点

### 1. 统计数据 API

#### GET /api/stats/today
返回今日交易统计

**响应示例**:
```json
{
  "total_trades": 42,
  "successful_trades": 38,
  "failed_trades": 4,
  "total_volume": 8500.00,
  "pnl": 1250.50,
  "win_rate": 0.905,
  "avg_trade_time_ms": 1250,
  "active_positions": 3
}
```

**实现建议**:
```rust
// src/api/stats.rs
use actix_web::{web, HttpResponse};
use crate::services::metrics::MetricsService;

pub async fn get_today_stats(
    metrics: web::Data<MetricsService>,
) -> Result<HttpResponse, Error> {
    let stats = metrics.get_today_stats().await?;
    Ok(HttpResponse::Ok().json(stats))
}
```

#### GET /api/stats/pnl?hours=24
返回指定时间段的盈亏历史（用于图表）

**查询参数**:
- `hours`: 时间范围（默认 24）

**响应示例**:
```json
[
  {
    "timestamp": "2026-01-10T10:00:00Z",
    "cumulative_pnl": 100.50,
    "trade_count": 5
  },
  {
    "timestamp": "2026-01-10T11:00:00Z",
    "cumulative_pnl": 250.75,
    "trade_count": 12
  }
]
```

**SQL 查询示例**:
```sql
SELECT
  date_trunc('hour', created_at) as timestamp,
  SUM(pnl) OVER (ORDER BY date_trunc('hour', created_at)) as cumulative_pnl,
  COUNT(*) as trade_count
FROM trades
WHERE created_at > NOW() - INTERVAL '24 hours'
  AND pnl IS NOT NULL
GROUP BY date_trunc('hour', created_at)
ORDER BY timestamp;
```

---

### 2. 交易数据 API

#### GET /api/trades
获取交易列表（支持分页和过滤）

**查询参数**:
- `limit`: 每页数量（默认 20）
- `offset`: 偏移量（默认 0）
- `status`: 状态过滤（可选: PENDING, COMPLETED, FAILED）
- `start_time`: 开始时间（ISO 8601）
- `end_time`: 结束时间（ISO 8601）

**响应示例**:
```json
{
  "trades": [
    {
      "id": "trade-123",
      "timestamp": "2026-01-10T10:30:00Z",
      "token_id": "0x1234...",
      "token_name": "Trump YES",
      "side": "UP",
      "shares": 100,
      "entry_price": 0.45,
      "exit_price": 0.52,
      "pnl": 7.00,
      "status": "COMPLETED"
    }
  ],
  "total": 150
}
```

**实现建议**:
```rust
// src/api/trades.rs
pub async fn get_trades(
    query: web::Query<TradeQuery>,
    store: web::Data<PostgresStore>,
) -> Result<HttpResponse, Error> {
    let trades = store.get_trades_paginated(
        query.limit.unwrap_or(20),
        query.offset.unwrap_or(0),
        query.status.as_deref(),
        query.start_time.as_ref(),
        query.end_time.as_ref(),
    ).await?;

    let total = store.count_trades(query.status.as_deref()).await?;

    Ok(HttpResponse::Ok().json(json!({
        "trades": trades,
        "total": total
    })))
}
```

#### GET /api/trades/:id
获取单个交易详情

**响应**: 同上单个 trade 对象

---

### 3. 仓位数据 API

#### GET /api/positions
获取当前所有活跃仓位

**响应示例**:
```json
[
  {
    "token_id": "0x1234...",
    "token_name": "Trump YES",
    "side": "UP",
    "shares": 100,
    "entry_price": 0.45,
    "current_price": 0.52,
    "unrealized_pnl": 7.00,
    "entry_time": "2026-01-10T10:00:00Z",
    "duration_seconds": 3600
  }
]
```

**实现建议**:
```rust
// src/api/positions.rs
pub async fn get_positions(
    store: web::Data<PostgresStore>,
    market_data: web::Data<MarketDataCache>,
) -> Result<HttpResponse, Error> {
    // 获取所有未完成的 cycles
    let open_cycles = store.get_open_cycles().await?;

    let positions: Vec<Position> = open_cycles
        .into_iter()
        .map(|cycle| {
            let current_price = market_data.get_price(&cycle.token_id)?;
            let unrealized_pnl = calculate_pnl(
                cycle.shares,
                cycle.entry_price,
                current_price,
                cycle.side
            );

            Position {
                token_id: cycle.token_id,
                token_name: cycle.token_name,
                side: cycle.side,
                shares: cycle.shares,
                entry_price: cycle.entry_price,
                current_price,
                unrealized_pnl,
                entry_time: cycle.created_at,
                duration_seconds: (Utc::now() - cycle.created_at).num_seconds(),
            }
        })
        .collect();

    Ok(HttpResponse::Ok().json(positions))
}
```

---

### 4. 系统控制 API

#### GET /api/system/status
获取系统状态

**响应示例**:
```json
{
  "status": "running",
  "uptime_seconds": 86400,
  "version": "1.0.0",
  "strategy": "momentum",
  "last_trade_time": "2026-01-10T10:30:00Z",
  "websocket_connected": true,
  "database_connected": true,
  "error_count_1h": 2
}
```

**实现建议**:
```rust
// src/api/system.rs
pub async fn get_system_status(
    app_state: web::Data<AppState>,
) -> Result<HttpResponse, Error> {
    let status = SystemStatus {
        status: app_state.get_status(),
        uptime_seconds: app_state.get_uptime().as_secs(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        strategy: app_state.config.strategy.clone(),
        last_trade_time: app_state.get_last_trade_time(),
        websocket_connected: app_state.ws_connected.load(Ordering::Relaxed),
        database_connected: app_state.db_connected.load(Ordering::Relaxed),
        error_count_1h: app_state.get_error_count_1h(),
    };

    Ok(HttpResponse::Ok().json(status))
}
```

#### POST /api/system/start
启动交易系统

**响应示例**:
```json
{
  "success": true,
  "message": "系统已启动"
}
```

**实现建议**:
```rust
pub async fn start_system(
    app_state: web::Data<AppState>,
) -> Result<HttpResponse, Error> {
    app_state.start().await?;

    Ok(HttpResponse::Ok().json(json!({
        "success": true,
        "message": "系统已启动"
    })))
}
```

#### POST /api/system/stop
停止交易系统

**响应**: 同上

#### POST /api/system/restart
重启交易系统

**响应**: 同上

---

### 5. 配置管理 API

#### GET /api/config
获取当前策略配置

**响应示例**:
```json
{
  "symbols": ["BTCUSDT", "ETHUSDT", "SOLUSDT"],
  "min_move": 0.15,
  "max_entry": 45,
  "shares": 100,
  "predictive": true,
  "take_profit": 20,
  "stop_loss": 12
}
```

**实现建议**:
```rust
// src/api/config.rs
pub async fn get_config(
    app_state: web::Data<AppState>,
) -> Result<HttpResponse, Error> {
    let config = app_state.config.clone();
    Ok(HttpResponse::Ok().json(config))
}
```

#### PUT /api/config
更新策略配置

**请求体**: 同上 config 对象（部分更新）

**响应示例**:
```json
{
  "success": true
}
```

**实现建议**:
```rust
pub async fn update_config(
    new_config: web::Json<PartialConfig>,
    app_state: web::Data<AppState>,
) -> Result<HttpResponse, Error> {
    app_state.update_config(new_config.into_inner()).await?;

    Ok(HttpResponse::Ok().json(json!({
        "success": true
    })))
}
```

---

### 6. 安全审计 API

#### GET /api/security/events
获取安全事件列表

**查询参数**:
- `limit`: 数量限制（默认 100）
- `severity`: 严重程度过滤（LOW, MEDIUM, HIGH, CRITICAL）
- `start_time`: 开始时间（ISO 8601）

**响应示例**:
```json
[
  {
    "id": "event-123",
    "timestamp": "2026-01-10T10:30:00Z",
    "event_type": "DUPLICATE_ORDER",
    "severity": "MEDIUM",
    "details": "检测到重复订单提交，已自动拒绝",
    "metadata": {
      "idempotency_key": "abc123",
      "order_id": "order-456"
    }
  }
]
```

**实现建议**:
```rust
// src/api/security.rs
pub async fn get_security_events(
    query: web::Query<SecurityEventQuery>,
    store: web::Data<PostgresStore>,
) -> Result<HttpResponse, Error> {
    let events = store.get_security_events(
        query.limit.unwrap_or(100),
        query.severity.as_deref(),
        query.start_time.as_ref(),
    ).await?;

    Ok(HttpResponse::Ok().json(events))
}
```

---

## WebSocket 服务

### 实现要求

使用 Socket.io 协议（或兼容方案）在 `/ws` 路径提供 WebSocket 服务。

### 事件类型

#### 1. log (日志事件)
```json
{
  "timestamp": "2026-01-10T10:30:00Z",
  "level": "INFO",
  "component": "strategy_engine",
  "message": "检测到交易信号",
  "metadata": {
    "token_id": "0x1234...",
    "signal_strength": 0.85
  }
}
```

#### 2. trade (交易事件)
```json
{
  "id": "trade-123",
  "timestamp": "2026-01-10T10:30:00Z",
  "token_id": "0x1234...",
  "token_name": "Trump YES",
  "side": "UP",
  "shares": 100,
  "entry_price": 0.45,
  "exit_price": null,
  "pnl": null,
  "status": "PENDING"
}
```

#### 3. position (仓位更新)
```json
{
  "token_id": "0x1234...",
  "token_name": "Trump YES",
  "side": "UP",
  "shares": 100,
  "entry_price": 0.45,
  "current_price": 0.47,
  "unrealized_pnl": 2.00,
  "entry_time": "2026-01-10T10:00:00Z",
  "duration_seconds": 1800
}
```

#### 4. market (市场数据)
```json
{
  "token_id": "0x1234...",
  "token_name": "Trump YES",
  "best_bid": 0.46,
  "best_ask": 0.47,
  "spread": 0.01,
  "last_price": 0.465,
  "volume_24h": 1000000,
  "timestamp": "2026-01-10T10:30:00Z"
}
```

#### 5. status (系统状态)
```json
{
  "status": "running"
}
```

### 实现建议

```rust
// src/api/websocket.rs
use actix::prelude::*;
use actix_web_actors::ws;

pub struct WsConnection {
    id: String,
    broadcaster: Addr<WsBroadcaster>,
}

impl Actor for WsConnection {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        // 注册连接
        self.broadcaster.do_send(Connect {
            id: self.id.clone(),
            addr: ctx.address(),
        });
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsConnection {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Ping(msg)) => ctx.pong(&msg),
            Ok(ws::Message::Close(_)) => ctx.stop(),
            _ => {}
        }
    }
}

// 广播器
pub struct WsBroadcaster {
    sessions: HashMap<String, Addr<WsConnection>>,
}

impl WsBroadcaster {
    pub fn broadcast_log(&self, log: LogEntry) {
        let msg = serde_json::to_string(&json!({
            "type": "log",
            "data": log
        })).unwrap();

        for session in self.sessions.values() {
            session.do_send(WsMessage(msg.clone()));
        }
    }

    pub fn broadcast_trade(&self, trade: Trade) {
        // 类似实现
    }

    // ... 其他广播方法
}
```

### 集成到现有系统

在交易引擎中添加广播调用：

```rust
// src/strategy/engine.rs

impl StrategyEngine {
    pub async fn execute_trade(&self, signal: Signal) -> Result<()> {
        // 记录日志并广播
        let log = LogEntry {
            timestamp: Utc::now(),
            level: "INFO",
            component: "strategy_engine",
            message: "执行交易".to_string(),
            metadata: Some(json!({ "signal": signal })),
        };
        self.ws_broadcaster.broadcast_log(log);

        // 创建交易
        let trade = self.create_trade(&signal).await?;
        self.ws_broadcaster.broadcast_trade(trade);

        // ... 继续执行
    }
}
```

---

## 路由配置

```rust
// src/main.rs
use actix_web::{web, App, HttpServer};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    HttpServer::new(move || {
        App::new()
            // CORS 配置
            .wrap(
                Cors::default()
                    .allowed_origin("http://localhost:3000")
                    .allowed_origin("https://trading.example.com")
                    .allowed_methods(vec!["GET", "POST", "PUT"])
                    .allowed_headers(vec![header::CONTENT_TYPE])
                    .max_age(3600)
            )
            // API 路由
            .service(
                web::scope("/api")
                    // Stats
                    .route("/stats/today", web::get().to(api::stats::get_today_stats))
                    .route("/stats/pnl", web::get().to(api::stats::get_pnl_history))
                    // Trades
                    .route("/trades", web::get().to(api::trades::get_trades))
                    .route("/trades/{id}", web::get().to(api::trades::get_trade_by_id))
                    // Positions
                    .route("/positions", web::get().to(api::positions::get_positions))
                    // System
                    .route("/system/status", web::get().to(api::system::get_system_status))
                    .route("/system/start", web::post().to(api::system::start_system))
                    .route("/system/stop", web::post().to(api::system::stop_system))
                    .route("/system/restart", web::post().to(api::system::restart_system))
                    // Config
                    .route("/config", web::get().to(api::config::get_config))
                    .route("/config", web::put().to(api::config::update_config))
                    // Security
                    .route("/security/events", web::get().to(api::security::get_security_events))
            )
            // WebSocket
            .route("/ws", web::get().to(api::websocket::websocket_handler))
            // 静态文件服务（可选）
            .service(Files::new("/", "./static").index_file("index.html"))
    })
    .bind(("0.0.0.0", 8080))?
    .run()
    .await
}
```

---

## 依赖项

在 `Cargo.toml` 添加：

```toml
[dependencies]
actix-web = "4"
actix-web-actors = "4"
actix = "0.13"
actix-files = "0.6"  # 静态文件服务
actix-cors = "0.7"   # CORS 支持
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = { version = "0.4", features = ["serde"] }
```

---

## 实现优先级

### Phase 1: 基础 API（2-3 小时）
1. ✅ GET /api/stats/today
2. ✅ GET /api/trades
3. ✅ GET /api/positions
4. ✅ GET /api/system/status

### Phase 2: 控制 API（1-2 小时）
1. ✅ POST /api/system/start/stop/restart
2. ✅ GET/PUT /api/config

### Phase 3: WebSocket（2-3 小时）
1. ✅ 基础 WebSocket 连接
2. ✅ log, trade, position 事件
3. ✅ 集成到现有系统

### Phase 4: 高级功能（1-2 小时）
1. ✅ GET /api/stats/pnl (图表数据)
2. ✅ GET /api/security/events
3. ✅ 性能优化

**总计预估时间**: 6-10 小时

---

## 测试建议

### 单元测试示例

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, App};

    #[actix_web::test]
    async fn test_get_today_stats() {
        let app = test::init_service(
            App::new()
                .route("/api/stats/today", web::get().to(get_today_stats))
        ).await;

        let req = test::TestRequest::get()
            .uri("/api/stats/today")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }
}
```

### 手动测试

```bash
# 测试统计 API
curl http://localhost:8080/api/stats/today

# 测试交易列表
curl "http://localhost:8080/api/trades?limit=10&status=COMPLETED"

# 测试系统控制
curl -X POST http://localhost:8080/api/system/start

# 测试 WebSocket（使用 wscat）
npm install -g wscat
wscat -c ws://localhost:8080/ws
```

---

## 后续优化

1. **缓存**: 使用 Redis 缓存热数据
2. **限流**: 防止 API 滥用
3. **认证**: JWT 或 API Key 认证
4. **监控**: Prometheus metrics
5. **日志**: 结构化日志（tracing）

---

**文档生成时间**: 2026-01-10
**预计实现时间**: 6-10 小时
**优先级**: 高 - 前端已就绪，等待后端 API
