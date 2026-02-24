//! Crypto Domain Agent - 加密貨幣策略 Agent
//!
//! 專門處理 BTC/ETH/SOL 等 15 分鐘 UP/DOWN 輪的策略 Agent。
//! 實作 DomainAgent trait，可接入 Order Platform。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info, warn};

use crate::domain::Side;
use crate::error::Result;
use crate::platform::{
    AgentRiskParams, AgentStatus, Domain, DomainAgent, DomainEvent, ExecutionReport, OrderIntent,
    OrderPriority,
};

const DEPLOYMENT_ID_CRYPTO_SPLIT_ARB: &str = "crypto.pm.split_arb";

/// Crypto Agent 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoAgentConfig {
    /// Agent ID
    pub id: String,
    /// Agent 名稱
    pub name: String,
    /// 監控的幣種 (e.g., "BTC", "ETH", "SOL")
    pub coins: Vec<String>,
    /// Sum of asks 入場閾值 (低於此值考慮入場)
    pub sum_threshold: Decimal,
    /// 最小動量閾值 (1秒動量)
    pub min_momentum_1s: f64,
    /// 預設下單數量
    pub default_shares: u64,
    /// 止盈目標
    pub take_profit: Decimal,
    /// 止損限制
    pub stop_loss: Decimal,
    /// 風控參數
    pub risk_params: AgentRiskParams,
}

impl Default for CryptoAgentConfig {
    fn default() -> Self {
        Self {
            id: "crypto-agent-1".to_string(),
            name: "Crypto Split Arb Agent".to_string(),
            coins: vec!["BTC".to_string(), "ETH".to_string(), "SOL".to_string()],
            sum_threshold: dec!(0.96),
            min_momentum_1s: 0.001,
            default_shares: 100,
            take_profit: dec!(0.02),
            stop_loss: dec!(0.05),
            risk_params: AgentRiskParams::default(),
        }
    }
}

/// 內部持倉追蹤
#[derive(Debug, Clone)]
struct InternalPosition {
    market_slug: String,
    token_id: String,
    side: Side,
    shares: u64,
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    is_hedged: bool,
}

impl InternalPosition {
    fn unrealized_pnl(&self, current_price: Decimal) -> Decimal {
        (current_price - self.entry_price) * Decimal::from(self.shares)
    }
}

/// Crypto 策略 Agent
///
/// 監聽 Crypto 事件，當發現有利的 sum of asks 時下單。
pub struct CryptoAgent {
    config: CryptoAgentConfig,
    status: AgentStatus,
    /// 內部持倉追蹤
    positions: HashMap<String, InternalPosition>,
    /// 今日 PnL
    daily_pnl: Decimal,
    /// 總暴露
    total_exposure: Decimal,
    /// 最後價格緩存 (market_slug -> prices)
    price_cache: HashMap<String, (Decimal, Decimal)>, // (up_ask, down_ask)
    /// 最後動量緩存
    momentum_cache: HashMap<String, [f64; 4]>,
    /// 連續失敗計數
    consecutive_failures: u32,
}

impl CryptoAgent {
    /// 創建新的 Crypto Agent
    pub fn new(config: CryptoAgentConfig) -> Self {
        info!("Creating CryptoAgent: {} ({})", config.name, config.id);
        Self {
            config,
            status: AgentStatus::Initializing,
            positions: HashMap::new(),
            daily_pnl: Decimal::ZERO,
            total_exposure: Decimal::ZERO,
            price_cache: HashMap::new(),
            momentum_cache: HashMap::new(),
            consecutive_failures: 0,
        }
    }

    /// 使用默認配置創建
    pub fn with_defaults() -> Self {
        Self::new(CryptoAgentConfig::default())
    }

    /// 分析 Crypto 事件，決定是否下單
    fn analyze_crypto_event(
        &mut self,
        event: &super::super::types::CryptoEvent,
    ) -> Vec<OrderIntent> {
        let mut intents = Vec::new();

        // 檢查是否是我們監控的幣種
        let coin = event.symbol.replace("USDT", "");
        if !self.config.coins.iter().any(|c| c == &coin) {
            return intents;
        }

        // 更新動量緩存
        if let Some(momentum) = &event.momentum {
            self.momentum_cache.insert(event.symbol.clone(), *momentum);
        }

        // 需要報價數據
        let quotes = match &event.quotes {
            Some(q) => q,
            None => return intents,
        };

        // 需要 round_slug
        let round_slug = match &event.round_slug {
            Some(s) => s.clone(),
            None => return intents,
        };

        // 更新價格緩存
        self.price_cache
            .insert(round_slug.clone(), (quotes.up_ask, quotes.down_ask));

        // 計算 sum of asks
        let sum_of_asks = quotes.sum_of_asks();

        debug!(
            "[{}] {} spot={}, sum={}, momentum={:?}",
            self.config.id, round_slug, event.spot_price, sum_of_asks, event.momentum
        );

        // 檢查是否已有該市場的持倉
        if self.positions.contains_key(&round_slug) {
            // 已有持倉，檢查是否需要對沖或平倉
            return self.check_exit_conditions(&round_slug, quotes);
        }

        // 檢查入場條件
        if sum_of_asks < self.config.sum_threshold {
            // 檢查動量
            let momentum_ok = event
                .momentum
                .map(|m| m[0].abs() >= self.config.min_momentum_1s)
                .unwrap_or(true);

            if momentum_ok {
                // 決定方向
                let (side, token_id) = self.decide_side(event, quotes);

                // 計算下單價格
                let limit_price = match side {
                    Side::Up => quotes.up_ask,
                    Side::Down => quotes.down_ask,
                };

                // 創建訂單意圖
                let intent = OrderIntent::new(
                    &self.config.id,
                    Domain::Crypto,
                    &round_slug,
                    &token_id,
                    side,
                    true, // is_buy
                    self.config.default_shares,
                    limit_price,
                )
                .with_priority(OrderPriority::Normal)
                .with_metadata("strategy", "crypto_split_arb")
                .with_deployment_id(DEPLOYMENT_ID_CRYPTO_SPLIT_ARB)
                .with_metadata("coin", &coin)
                .with_metadata("sum_of_asks", &sum_of_asks.to_string());

                info!(
                    "[{}] Signal: {} sum={} -> {} @ {}",
                    self.config.id, round_slug, sum_of_asks, side, limit_price
                );

                intents.push(intent);
            }
        }

        intents
    }

    /// 決定下單方向
    fn decide_side(
        &self,
        event: &super::super::types::CryptoEvent,
        _quotes: &super::super::types::QuoteData,
    ) -> (Side, String) {
        // 根據動量決定方向
        let side = if let Some(momentum) = &event.momentum {
            if momentum[0] > 0.0 {
                Side::Up
            } else {
                Side::Down
            }
        } else {
            // 無動量數據時默認 Up
            Side::Up
        };

        // 生成 token_id (實際應該從市場數據獲取)
        let token_id = format!(
            "{}-{}",
            event.round_slug.as_ref().unwrap_or(&"unknown".to_string()),
            side
        );

        (side, token_id)
    }

    /// 檢查平倉條件
    fn check_exit_conditions(
        &mut self,
        market_slug: &str,
        quotes: &super::super::types::QuoteData,
    ) -> Vec<OrderIntent> {
        let mut intents = Vec::new();

        if let Some(position) = self.positions.get(market_slug) {
            let current_price = match position.side {
                Side::Up => quotes.up_bid,
                Side::Down => quotes.down_bid,
            };

            let pnl_pct = (current_price - position.entry_price) / position.entry_price;

            // 止盈
            if pnl_pct >= self.config.take_profit {
                let intent = OrderIntent::new(
                    &self.config.id,
                    Domain::Crypto,
                    market_slug,
                    &position.token_id,
                    position.side,
                    false, // is_buy = false (賣出)
                    position.shares,
                    current_price,
                )
                .with_priority(OrderPriority::High)
                .with_deployment_id(DEPLOYMENT_ID_CRYPTO_SPLIT_ARB)
                .with_metadata("exit_reason", "take_profit");

                info!(
                    "[{}] Take profit: {} pnl={}%",
                    self.config.id,
                    market_slug,
                    pnl_pct * dec!(100)
                );

                intents.push(intent);
            }
            // 止損
            else if pnl_pct <= -self.config.stop_loss {
                let intent = OrderIntent::new(
                    &self.config.id,
                    Domain::Crypto,
                    market_slug,
                    &position.token_id,
                    position.side,
                    false, // is_buy = false (賣出)
                    position.shares,
                    current_price,
                )
                .with_priority(OrderPriority::Critical) // 緊急止損
                .with_deployment_id(DEPLOYMENT_ID_CRYPTO_SPLIT_ARB)
                .with_metadata("exit_reason", "stop_loss");

                warn!(
                    "[{}] Stop loss: {} pnl={}%",
                    self.config.id,
                    market_slug,
                    pnl_pct * dec!(100)
                );

                intents.push(intent);
            }
            // 檢查對沖機會
            else if !position.is_hedged {
                let other_side = match position.side {
                    Side::Up => Side::Down,
                    Side::Down => Side::Up,
                };
                let other_ask = match other_side {
                    Side::Up => quotes.up_ask,
                    Side::Down => quotes.down_ask,
                };

                // 檢查是否可以完成對沖並鎖定利潤
                let total_cost = position.entry_price + other_ask;
                if total_cost < dec!(1.0) {
                    let locked_profit = dec!(1.0) - total_cost;
                    let other_token_id = format!("{}-{}", market_slug, other_side);

                    let intent = OrderIntent::new(
                        &self.config.id,
                        Domain::Crypto,
                        market_slug,
                        &other_token_id,
                        other_side,
                        true, // is_buy
                        position.shares,
                        other_ask,
                    )
                    .with_priority(OrderPriority::High)
                    .with_deployment_id(DEPLOYMENT_ID_CRYPTO_SPLIT_ARB)
                    .with_metadata("hedge_for", &position.token_id)
                    .with_metadata("locked_profit", &locked_profit.to_string());

                    info!(
                        "[{}] Hedge opportunity: {} locked_profit={}",
                        self.config.id, market_slug, locked_profit
                    );

                    intents.push(intent);
                }
            }
        }

        intents
    }

    /// 處理執行結果
    fn handle_execution(&mut self, report: &ExecutionReport) {
        if report.is_success() {
            self.consecutive_failures = 0;

            // 查找相關的 intent metadata
            // 實際實作中應該追蹤 intent -> execution 的映射

            if report.filled_shares > 0 {
                info!(
                    "[{}] Execution success: {} shares @ {:?}",
                    self.config.id, report.filled_shares, report.avg_fill_price
                );
            }
        } else {
            self.consecutive_failures += 1;
            warn!(
                "[{}] Execution failed: {:?}. Consecutive failures: {}",
                self.config.id, report.error_message, self.consecutive_failures
            );

            // 連續失敗超過閾值，暫停交易
            if self.consecutive_failures >= 3 {
                warn!("[{}] Too many failures, pausing", self.config.id);
                self.status = AgentStatus::Paused;
            }
        }
    }

    /// 更新內部暴露計算
    fn update_exposure(&mut self) {
        self.total_exposure = self
            .positions
            .values()
            .map(|p| p.entry_price * Decimal::from(p.shares))
            .sum();
    }
}

#[async_trait]
impl DomainAgent for CryptoAgent {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn name(&self) -> &str {
        &self.config.name
    }

    fn domain(&self) -> Domain {
        Domain::Crypto
    }

    fn status(&self) -> AgentStatus {
        self.status
    }

    fn risk_params(&self) -> &AgentRiskParams {
        &self.config.risk_params
    }

    async fn on_event(&mut self, event: DomainEvent) -> Result<Vec<OrderIntent>> {
        // 只處理 Running 狀態
        if !self.status.can_trade() {
            return Ok(vec![]);
        }

        match event {
            DomainEvent::Crypto(crypto_event) => Ok(self.analyze_crypto_event(&crypto_event)),
            DomainEvent::QuoteUpdate(update) => {
                // 處理報價更新 (更新價格緩存)
                if update.domain == Domain::Crypto {
                    self.price_cache.insert(
                        update.market_slug.clone(),
                        (update.ask, update.bid), // 簡化處理
                    );
                }
                Ok(vec![])
            }
            DomainEvent::Tick(_) => {
                // 定時檢查持倉狀態
                // 可以在這裡實作超時平倉邏輯
                Ok(vec![])
            }
            _ => Ok(vec![]),
        }
    }

    async fn on_execution(&mut self, report: ExecutionReport) {
        self.handle_execution(&report);
    }

    async fn start(&mut self) -> Result<()> {
        info!("[{}] Starting...", self.config.id);
        self.status = AgentStatus::Running;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("[{}] Stopping...", self.config.id);
        self.status = AgentStatus::Stopped;
        Ok(())
    }

    fn pause(&mut self) {
        info!("[{}] Pausing...", self.config.id);
        self.status = AgentStatus::Paused;
    }

    fn resume(&mut self) {
        info!("[{}] Resuming...", self.config.id);
        self.consecutive_failures = 0;
        self.status = AgentStatus::Running;
    }

    fn position_count(&self) -> usize {
        self.positions.len()
    }

    fn total_exposure(&self) -> Decimal {
        self.total_exposure
    }

    fn daily_pnl(&self) -> Decimal {
        self.daily_pnl
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::types::{CryptoEvent, QuoteData};

    fn make_crypto_event(
        symbol: &str,
        spot: Decimal,
        up_ask: Decimal,
        down_ask: Decimal,
    ) -> CryptoEvent {
        CryptoEvent {
            symbol: symbol.to_string(),
            spot_price: spot,
            round_slug: Some(format!("{}-15m-round-1", symbol.to_lowercase())),
            quotes: Some(QuoteData {
                up_bid: up_ask - dec!(0.01),
                up_ask,
                down_bid: down_ask - dec!(0.01),
                down_ask,
                timestamp: Utc::now(),
            }),
            momentum: Some([0.002, 0.001, 0.0005, 0.0001]),
        }
    }

    #[tokio::test]
    async fn test_agent_creation() {
        let agent = CryptoAgent::with_defaults();
        assert_eq!(agent.id(), "crypto-agent-1");
        assert_eq!(agent.status(), AgentStatus::Initializing);
    }

    #[tokio::test]
    async fn test_agent_start_stop() {
        let mut agent = CryptoAgent::with_defaults();

        agent.start().await.unwrap();
        assert_eq!(agent.status(), AgentStatus::Running);

        agent.pause();
        assert_eq!(agent.status(), AgentStatus::Paused);

        agent.resume();
        assert_eq!(agent.status(), AgentStatus::Running);

        agent.stop().await.unwrap();
        assert_eq!(agent.status(), AgentStatus::Stopped);
    }

    #[tokio::test]
    async fn test_signal_generation() {
        let mut agent = CryptoAgent::with_defaults();
        agent.start().await.unwrap();

        // 創建一個有利的信號 (sum < 0.96)
        let event = make_crypto_event("BTCUSDT", dec!(50000), dec!(0.47), dec!(0.48));
        let domain_event = DomainEvent::Crypto(event);

        let intents = agent.on_event(domain_event).await.unwrap();

        // 應該產生一個訂單意圖
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].agent_id, "crypto-agent-1");
        assert!(intents[0].is_buy);
    }

    #[tokio::test]
    async fn test_no_signal_on_high_sum() {
        let mut agent = CryptoAgent::with_defaults();
        agent.start().await.unwrap();

        // 創建一個不利的信號 (sum > 0.96)
        let event = make_crypto_event("BTCUSDT", dec!(50000), dec!(0.50), dec!(0.50));
        let domain_event = DomainEvent::Crypto(event);

        let intents = agent.on_event(domain_event).await.unwrap();

        // 不應該產生訂單
        assert!(intents.is_empty());
    }

    #[tokio::test]
    async fn test_ignored_coin() {
        let mut agent = CryptoAgent::with_defaults();
        agent.start().await.unwrap();

        // DOGE 不在監控列表中
        let event = make_crypto_event("DOGEUSDT", dec!(0.1), dec!(0.47), dec!(0.48));
        let domain_event = DomainEvent::Crypto(event);

        let intents = agent.on_event(domain_event).await.unwrap();

        // 應該忽略
        assert!(intents.is_empty());
    }
}
