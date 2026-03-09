//! Risk Gate - 訂單風控閘門
//!
//! 負責在訂單執行前進行多層風控檢查：
//! - Agent 級別風控 (單筆限額、市場限制)
//! - 平台級別風控 (總暴露、熔斷機制)
//! - 組合級別風控 (每日損失、連續失敗)

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use super::types::{Domain, OrderIntent, OrderPriority};
use crate::agent_runtime::AgentRiskParams;
mod config;
mod stats;
mod transitions;
mod types;

pub use self::config::RiskConfig;
use self::stats::{AgentRiskStats, DailyStats, DrawdownStats};
pub use self::types::{
    AdjustmentSuggestion, BlockReason, CircuitBreakerEvent, DrawdownSnapshot,
    PlatformRiskState, RiskCheckResult,
};

/// 風控閘門
///
/// 所有訂單在執行前都必須通過這個閘門的檢查。
pub struct RiskGate {
    config: RiskConfig,
    /// 平台風控狀態
    state: Arc<RwLock<PlatformRiskState>>,
    /// 每個 Agent 的風控統計
    agent_stats: Arc<RwLock<HashMap<String, AgentRiskStats>>>,
    /// 每個 Agent 的風控參數
    agent_params: Arc<RwLock<HashMap<String, AgentRiskParams>>>,
    /// Agent -> domain mapping for domain-level controls
    agent_domains: Arc<RwLock<HashMap<String, Domain>>>,
    /// 平台總暴露
    total_exposure: Arc<RwLock<Decimal>>,
    /// Exposure by domain
    domain_exposure: Arc<RwLock<HashMap<Domain, Decimal>>>,
    /// 全局連續失敗計數
    consecutive_failures: AtomicU32,
    /// 每日統計
    daily_stats: Arc<RwLock<DailyStats>>,
    /// Drawdown stats (runtime cumulative realized curve).
    drawdown_stats: Arc<RwLock<DrawdownStats>>,
    /// Circuit breaker event history (bounded)
    circuit_events: Arc<RwLock<Vec<CircuitBreakerEvent>>>,
    /// Last HALTED timestamp (for auto-recovery cooldown checks)
    halted_at: Arc<RwLock<Option<DateTime<Utc>>>>,
}

impl RiskGate {
    /// 創建新的風控閘門
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            state: Arc::new(RwLock::new(PlatformRiskState::Normal)),
            agent_stats: Arc::new(RwLock::new(HashMap::new())),
            agent_params: Arc::new(RwLock::new(HashMap::new())),
            agent_domains: Arc::new(RwLock::new(HashMap::new())),
            total_exposure: Arc::new(RwLock::new(Decimal::ZERO)),
            domain_exposure: Arc::new(RwLock::new(HashMap::new())),
            consecutive_failures: AtomicU32::new(0),
            daily_stats: Arc::new(RwLock::new(DailyStats::default())),
            drawdown_stats: Arc::new(RwLock::new(DrawdownStats::default())),
            circuit_events: Arc::new(RwLock::new(Vec::new())),
            halted_at: Arc::new(RwLock::new(None)),
        }
    }

    /// 註冊 Agent 的風控參數
    pub async fn register_agent(&self, agent_id: &str, params: AgentRiskParams) {
        let mut params_map = self.agent_params.write().await;
        params_map.insert(agent_id.to_string(), params);
        debug!("Registered risk params for agent {}", agent_id);
    }

    /// 註冊 Agent 的風控參數 (含 domain)
    pub async fn register_agent_with_domain(
        &self,
        agent_id: &str,
        domain: Domain,
        params: AgentRiskParams,
    ) {
        self.register_agent(agent_id, params).await;
        self.agent_domains
            .write()
            .await
            .insert(agent_id.to_string(), domain);
    }

    /// 取消註冊 Agent
    pub async fn unregister_agent(&self, agent_id: &str) {
        let removed_domain = self.agent_domains.write().await.remove(agent_id);
        if let Some(domain) = removed_domain {
            let old_exposure = self
                .agent_stats
                .read()
                .await
                .get(agent_id)
                .map(|s| s.exposure)
                .unwrap_or(Decimal::ZERO);
            if old_exposure > Decimal::ZERO {
                let mut domain_map = self.domain_exposure.write().await;
                if let Some(current) = domain_map.get_mut(&domain) {
                    *current = (*current - old_exposure).max(Decimal::ZERO);
                    if *current == Decimal::ZERO {
                        domain_map.remove(&domain);
                    }
                }
            }
        }
        self.agent_params.write().await.remove(agent_id);
        self.agent_stats.write().await.remove(agent_id);
        debug!("Unregistered agent {}", agent_id);
    }

    // ==================== 核心風控檢查 ====================

    /// 檢查訂單是否可以執行
    ///
    /// 這是主要的風控入口點，會依序執行多層檢查。
    pub async fn check_order(&self, intent: &OrderIntent) -> RiskCheckResult {
        // Try automatic recovery before evaluating trading eligibility.
        self.try_auto_recover_circuit_breaker().await;

        // 1. 檢查訂單是否過期
        if intent.is_expired() {
            return RiskCheckResult::Blocked(BlockReason::OrderExpired);
        }

        // Binary-options semantics (Polymarket): SELL intents are reduce-only exits.
        // They must stay allowed during circuit-breaker, daily-loss, and exposure limits.
        if !intent.is_buy {
            return RiskCheckResult::Passed;
        }

        // 2. 檢查平台狀態 (BUY only)
        let platform_state = *self.state.read().await;
        if !platform_state.can_trade() {
            return RiskCheckResult::Blocked(BlockReason::CircuitBreakerTripped {
                reason: "Platform trading halted".to_string(),
            });
        }

        // 3. Critical 訂單不再繞過風控檢查
        let is_critical = intent.priority == OrderPriority::Critical;
        if is_critical && self.config.critical_bypass_exposure {
            warn!(
                "critical_bypass_exposure is enabled for intent {} but is ignored by policy",
                intent.intent_id
            );
        }

        // 4. 獲取 Agent 風控參數
        let params = {
            let params_map = self.agent_params.read().await;
            match params_map.get(&intent.agent_id) {
                Some(p) => p.clone(),
                None => {
                    warn!(
                        "No risk params for agent {}, blocking order",
                        intent.agent_id
                    );
                    return RiskCheckResult::Blocked(BlockReason::UnregisteredAgent {
                        agent: intent.agent_id.clone(),
                    });
                }
            }
        };

        // 5. 檢查市場是否允許
        if !params.is_market_allowed(&intent.market_slug) {
            return RiskCheckResult::Blocked(BlockReason::MarketNotAllowed {
                market: intent.market_slug.clone(),
                agent: intent.agent_id.clone(),
            });
        }

        // 6. 計算訂單價值
        let order_value = intent.notional_value();

        // 7. 檢查單筆限額
        if order_value > params.max_order_value {
            // 可以建議調整數量
            let max_shares = (params.max_order_value / intent.limit_price)
                .to_u64()
                .unwrap_or(0);

            if max_shares > 0 {
                return RiskCheckResult::Adjusted(AdjustmentSuggestion {
                    max_shares,
                    reason: format!(
                        "Order value ${} exceeds agent limit ${}",
                        order_value, params.max_order_value
                    ),
                });
            } else {
                return RiskCheckResult::Blocked(BlockReason::ExceedsSingleLimit {
                    limit: params.max_order_value,
                    requested: order_value,
                });
            }
        }

        // 8. 檢查 Agent 總暴露
        let agent_stats = self.agent_stats.read().await;
        let current_agent_exposure = agent_stats
            .get(&intent.agent_id)
            .map(|s| s.exposure)
            .unwrap_or(Decimal::ZERO);
        drop(agent_stats);

        if current_agent_exposure + order_value > params.max_total_exposure {
            return RiskCheckResult::Blocked(BlockReason::ExceedsTotalExposure {
                limit: params.max_total_exposure,
                current: current_agent_exposure,
                requested: order_value,
            });
        }

        // 8b. Domain exposure cap (if configured)
        if let Some(domain_limit) = self.config.domain_exposure_limit(intent.domain) {
            let current_domain_exposure = self
                .domain_exposure
                .read()
                .await
                .get(&intent.domain)
                .copied()
                .unwrap_or(Decimal::ZERO);
            if current_domain_exposure + order_value > domain_limit {
                return RiskCheckResult::Blocked(BlockReason::DomainExposureExceeded {
                    domain: intent.domain,
                    limit: domain_limit,
                    current: current_domain_exposure,
                    requested: order_value,
                });
            }
        }

        // 9. 檢查平台總暴露
        let current_platform_exposure = *self.total_exposure.read().await;
        if current_platform_exposure + order_value > self.config.max_platform_exposure {
            return RiskCheckResult::Blocked(BlockReason::ExceedsTotalExposure {
                limit: self.config.max_platform_exposure,
                current: current_platform_exposure,
                requested: order_value,
            });
        }

        // 10. 新開倉時的額外檢查
        if intent.is_buy && !platform_state.can_open_new() {
            // 警戒狀態下可能需要更嚴格的檢查
            debug!(
                "Elevated state: allowing buy order {} with extra scrutiny",
                intent.intent_id
            );
        }

        // 11. 檢查每日損失
        let daily = self.daily_stats.read().await;
        if daily.total_pnl < Decimal::ZERO && daily.total_pnl.abs() >= self.config.daily_loss_limit
        {
            return RiskCheckResult::Blocked(BlockReason::DailyLossExceeded {
                limit: self.config.daily_loss_limit,
                current: daily.total_pnl.abs(),
            });
        }

        if let Some(domain_loss_limit) = self.config.domain_daily_loss_limit(intent.domain) {
            let domain_pnl = daily
                .domain_pnl
                .get(&intent.domain)
                .copied()
                .unwrap_or(Decimal::ZERO);
            if domain_pnl < Decimal::ZERO && domain_pnl.abs() >= domain_loss_limit {
                return RiskCheckResult::Blocked(BlockReason::DomainDailyLossExceeded {
                    domain: intent.domain,
                    limit: domain_loss_limit,
                    current: domain_pnl.abs(),
                });
            }
        }

        // 12. 檢查回撤上限 (runtime cumulative realized curve)
        if let Some(limit) = self.config.max_drawdown_limit {
            let current_drawdown = self.drawdown_stats.read().await.current_drawdown;
            if limit > Decimal::ZERO && current_drawdown >= limit {
                return RiskCheckResult::Blocked(BlockReason::DrawdownExceeded {
                    limit,
                    current: current_drawdown,
                });
            }
        }

        RiskCheckResult::Passed
    }

    // ==================== 狀態更新 ====================

    /// 更新 Agent 暴露
    pub async fn update_agent_exposure(
        &self,
        agent_id: &str,
        exposure: Decimal,
        unrealized_pnl: Decimal,
        position_count: usize,
        unhedged_count: u32,
    ) {
        let domain = self.agent_domains.read().await.get(agent_id).copied();

        let mut stats_map = self.agent_stats.write().await;
        let stats = stats_map.entry(agent_id.to_string()).or_default();

        let old_exposure = stats.exposure;
        stats.exposure = exposure;
        stats.unrealized_pnl = unrealized_pnl;
        stats.position_count = position_count;
        stats.unhedged_count = unhedged_count;
        stats.last_update = Some(Utc::now());

        drop(stats_map);

        // 更新平台總暴露
        let mut total = self.total_exposure.write().await;
        *total = *total - old_exposure + exposure;

        // 更新 domain 暴露
        if let Some(domain) = domain {
            let mut domain_map = self.domain_exposure.write().await;
            let current = domain_map.entry(domain).or_insert(Decimal::ZERO);
            *current = (*current - old_exposure + exposure).max(Decimal::ZERO);
            if *current == Decimal::ZERO {
                domain_map.remove(&domain);
            }
        }
    }

    // ==================== 查詢方法 ====================

    /// 當前平台狀態
    pub async fn state(&self) -> PlatformRiskState {
        *self.state.read().await
    }

    /// 是否可以交易
    pub async fn can_trade(&self) -> bool {
        self.state.read().await.can_trade()
    }

    /// 當前平台總暴露
    pub async fn total_exposure(&self) -> Decimal {
        *self.total_exposure.read().await
    }

    /// Agent 統計
    pub async fn agent_stats(&self, agent_id: &str) -> Option<(Decimal, Decimal, usize, u32)> {
        let stats_map = self.agent_stats.read().await;
        stats_map.get(agent_id).map(|s| {
            (
                s.exposure,
                s.realized_pnl,
                s.position_count,
                s.consecutive_failures,
            )
        })
    }

    /// 每日統計
    pub async fn daily_stats(&self) -> (Decimal, u32, u32) {
        let daily = self.daily_stats.read().await;
        (daily.total_pnl, daily.success_count, daily.failure_count)
    }

    /// Daily loss limit (USD)
    pub fn daily_loss_limit(&self) -> Decimal {
        self.config.daily_loss_limit
    }

    /// Optional max drawdown limit (USD)
    pub fn max_drawdown_limit(&self) -> Option<Decimal> {
        self.config.max_drawdown_limit
    }

    /// Current drawdown + max observed drawdown (USD)
    pub async fn drawdown_stats(&self) -> (Decimal, Decimal) {
        let drawdown = self.drawdown_stats.read().await;
        (drawdown.current_drawdown, drawdown.max_drawdown_observed)
    }

    /// Full drawdown snapshot for persistence/recovery.
    pub async fn drawdown_snapshot(&self) -> DrawdownSnapshot {
        let drawdown = self.drawdown_stats.read().await;
        DrawdownSnapshot {
            current_equity: drawdown.current_equity,
            equity_peak: drawdown.equity_peak,
            current_drawdown: drawdown.current_drawdown,
            max_drawdown_observed: drawdown.max_drawdown_observed,
        }
    }

    /// Circuit breaker event history
    pub async fn circuit_breaker_events(&self) -> Vec<CircuitBreakerEvent> {
        self.circuit_events.read().await.clone()
    }

    /// 連續失敗數
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::SeqCst)
    }

    // ==================== 輔助方法 ====================

    /// 清理 (測試用)
    pub async fn clear(&self) {
        *self.state.write().await = PlatformRiskState::Normal;
        self.agent_stats.write().await.clear();
        self.agent_params.write().await.clear();
        self.agent_domains.write().await.clear();
        self.consecutive_failures.store(0, Ordering::SeqCst);
        *self.total_exposure.write().await = Decimal::ZERO;
        self.domain_exposure.write().await.clear();
        *self.daily_stats.write().await = DailyStats::default();
        *self.drawdown_stats.write().await = DrawdownStats::default();
        self.circuit_events.write().await.clear();
        *self.halted_at.write().await = None;
    }

}

impl Default for RiskGate {
    fn default() -> Self {
        Self::new(RiskConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::Domain;
    use super::*;
    use crate::domain::Side;

    fn make_intent(agent: &str, shares: u64, price: Decimal) -> OrderIntent {
        OrderIntent::new(
            agent,
            Domain::Crypto,
            "btc-15m",
            "token-123",
            Side::Up,
            true,
            shares,
            price,
        )
    }

    fn make_sell_intent(agent: &str, shares: u64, price: Decimal) -> OrderIntent {
        OrderIntent::new(
            agent,
            Domain::Crypto,
            "btc-15m",
            "token-123",
            Side::Up,
            false,
            shares,
            price,
        )
    }

    #[tokio::test]
    async fn test_basic_check() {
        let gate = RiskGate::new(RiskConfig::default());

        // 註冊 Agent
        gate.register_agent("agent1", AgentRiskParams::default())
            .await;

        // 正常訂單應該通過
        let intent = make_intent("agent1", 100, Decimal::from_str_exact("0.50").unwrap());
        let result = gate.check_order(&intent).await;
        assert!(result.is_passed());
    }

    #[tokio::test]
    async fn test_unregistered_agent_is_blocked() {
        let gate = RiskGate::new(RiskConfig::default());
        let intent = make_intent(
            "unknown-agent",
            10,
            Decimal::from_str_exact("0.50").unwrap(),
        );
        let result = gate.check_order(&intent).await;
        match result {
            RiskCheckResult::Blocked(BlockReason::UnregisteredAgent { agent }) => {
                assert_eq!(agent, "unknown-agent");
            }
            _ => panic!("Expected UnregisteredAgent block"),
        }
    }

    #[tokio::test]
    async fn test_single_limit() {
        let gate = RiskGate::new(RiskConfig::default());

        let mut params = AgentRiskParams::default();
        params.max_order_value = Decimal::from(10); // 很低的限額
        gate.register_agent("agent1", params).await;

        // 超過限額
        let intent = make_intent("agent1", 100, Decimal::from_str_exact("0.50").unwrap());
        let result = gate.check_order(&intent).await;

        match result {
            RiskCheckResult::Adjusted(adj) => {
                assert!(adj.max_shares < 100);
            }
            _ => panic!("Expected Adjusted result"),
        }
    }

    #[tokio::test]
    async fn test_circuit_breaker() {
        let mut config = RiskConfig::default();
        config.max_consecutive_failures = 3;
        config.circuit_breaker_auto_recover = false;
        let gate = RiskGate::new(config);

        // 記錄失敗
        for i in 0..3 {
            gate.record_failure("agent1", &format!("Failure {}", i))
                .await;
        }

        // 應該觸發熔斷
        assert_eq!(gate.state().await, PlatformRiskState::Halted);
        assert!(!gate.can_trade().await);

        // 重置
        gate.reset_circuit_breaker().await;
        assert!(gate.can_trade().await);
    }

    #[tokio::test]
    async fn test_sell_allowed_when_halted() {
        let mut config = RiskConfig::default();
        config.max_consecutive_failures = 1;
        config.circuit_breaker_auto_recover = false;
        let gate = RiskGate::new(config);

        gate.register_agent("agent1", AgentRiskParams::default())
            .await;

        gate.record_failure("agent1", "forced failure").await;
        assert_eq!(gate.state().await, PlatformRiskState::Halted);

        let buy_intent = make_intent("agent1", 10, Decimal::from_str_exact("0.50").unwrap());
        assert!(gate.check_order(&buy_intent).await.is_blocked());

        let sell_intent = make_sell_intent("agent1", 10, Decimal::from_str_exact("0.50").unwrap());
        assert!(gate.check_order(&sell_intent).await.is_passed());
    }

    #[tokio::test]
    async fn test_sell_allowed_when_daily_loss_exceeded() {
        let mut config = RiskConfig::default();
        config.daily_loss_limit = Decimal::from(5);
        let gate = RiskGate::new(config);

        gate.register_agent("agent1", AgentRiskParams::default())
            .await;

        {
            let mut daily = gate.daily_stats.write().await;
            daily.date = Some(Utc::now().date_naive());
            daily.total_pnl = Decimal::from(-6);
        }

        let buy_intent = make_intent("agent1", 10, Decimal::from_str_exact("0.50").unwrap());
        assert!(gate.check_order(&buy_intent).await.is_blocked());

        let sell_intent = make_sell_intent("agent1", 10, Decimal::from_str_exact("0.50").unwrap());
        assert!(gate.check_order(&sell_intent).await.is_passed());
    }

    #[tokio::test]
    async fn test_circuit_breaker_auto_recover_on_check_order() {
        let mut config = RiskConfig::default();
        config.max_consecutive_failures = 1;
        config.circuit_breaker_auto_recover = true;
        config.circuit_breaker_cooldown_secs = 0;
        let gate = RiskGate::new(config);

        gate.register_agent("agent1", AgentRiskParams::default())
            .await;
        gate.record_failure("agent1", "forced failure").await;
        assert_eq!(gate.state().await, PlatformRiskState::Halted);

        let intent = make_intent("agent1", 10, Decimal::from_str_exact("0.50").unwrap());
        let result = gate.check_order(&intent).await;
        assert!(result.is_passed());
        assert_eq!(gate.state().await, PlatformRiskState::Normal);
    }

    #[tokio::test]
    async fn test_critical_bypass_still_checked() {
        let mut config = RiskConfig::default();
        config.max_platform_exposure = Decimal::from(10); // 很低
        config.critical_bypass_exposure = true;
        let gate = RiskGate::new(config);

        gate.register_agent("agent1", AgentRiskParams::default())
            .await;

        // 普通訂單被攔截
        let intent = make_intent("agent1", 100, Decimal::from_str_exact("0.50").unwrap());
        let result = gate.check_order(&intent).await;
        assert!(result.is_blocked());

        // Critical 訂單也要經過完整檢查
        let critical_intent = intent.with_priority(OrderPriority::Critical);
        let result = gate.check_order(&critical_intent).await;
        assert!(result.is_blocked());
    }

    #[tokio::test]
    async fn test_domain_exposure_limit() {
        let mut config = RiskConfig::default();
        config.crypto_max_exposure = Some(Decimal::from(20));
        let gate = RiskGate::new(config);

        gate.register_agent_with_domain("agent1", Domain::Crypto, AgentRiskParams::default())
            .await;
        gate.update_agent_exposure("agent1", Decimal::from(15), Decimal::ZERO, 1, 0)
            .await;

        let intent = make_intent("agent1", 20, Decimal::from_str_exact("0.50").unwrap()); // $10
        let result = gate.check_order(&intent).await;
        match result {
            RiskCheckResult::Blocked(BlockReason::DomainExposureExceeded { domain, .. }) => {
                assert_eq!(domain, Domain::Crypto);
            }
            _ => panic!("Expected domain exposure block"),
        }
    }

    #[tokio::test]
    async fn test_domain_daily_loss_limit() {
        let mut config = RiskConfig::default();
        config.crypto_daily_loss_limit = Some(Decimal::from(5));
        let gate = RiskGate::new(config);

        gate.register_agent_with_domain("agent1", Domain::Crypto, AgentRiskParams::default())
            .await;
        gate.record_loss("agent1", Decimal::from(6)).await;

        let intent = make_intent("agent1", 5, Decimal::from_str_exact("0.50").unwrap()); // $2.5
        let result = gate.check_order(&intent).await;
        match result {
            RiskCheckResult::Blocked(BlockReason::DomainDailyLossExceeded { domain, .. }) => {
                assert_eq!(domain, Domain::Crypto);
            }
            _ => panic!("Expected domain daily loss block"),
        }
    }

    #[tokio::test]
    async fn test_drawdown_limit_triggers_circuit_breaker() {
        let mut config = RiskConfig::default();
        config.max_drawdown_limit = Some(Decimal::from(5));
        let gate = RiskGate::new(config);

        gate.register_agent_with_domain("agent1", Domain::Crypto, AgentRiskParams::default())
            .await;

        gate.record_success("agent1", Decimal::from(10)).await; // equity peak
        gate.record_success("agent1", Decimal::from(-3)).await; // drawdown = 3
        assert_eq!(gate.state().await, PlatformRiskState::Normal);

        gate.record_success("agent1", Decimal::from(-3)).await; // drawdown = 6
        assert_eq!(gate.state().await, PlatformRiskState::Halted);

        let (current_drawdown, max_drawdown) = gate.drawdown_stats().await;
        assert_eq!(current_drawdown, Decimal::from(6));
        assert_eq!(max_drawdown, Decimal::from(6));
    }

    #[tokio::test]
    async fn test_restore_runtime_counters_restores_agent_and_failure_streaks() {
        let mut config = RiskConfig::default();
        config.max_consecutive_failures = 4;
        config.daily_loss_limit = Decimal::from(1000);
        let gate = RiskGate::new(config);

        gate.register_agent("agent1", AgentRiskParams::default())
            .await;

        let today = Utc::now().date_naive();
        let mut agent_realized_pnl = HashMap::new();
        agent_realized_pnl.insert("agent1".to_string(), Decimal::from(12));
        let mut agent_consecutive_failures = HashMap::new();
        agent_consecutive_failures.insert("agent1".to_string(), 2);

        gate.restore_runtime_counters(
            today,
            Decimal::from(12),
            HashMap::new(),
            5,
            3,
            2,
            2,
            agent_realized_pnl,
            agent_consecutive_failures,
            Some(Utc::now()),
        )
        .await;

        assert_eq!(gate.state().await, PlatformRiskState::Elevated);
        assert_eq!(gate.consecutive_failures(), 2);
        let stats = gate
            .agent_stats("agent1")
            .await
            .expect("agent stats restored");
        assert_eq!(stats.1, Decimal::from(12));
        assert_eq!(stats.3, 2);
    }

    #[tokio::test]
    async fn test_restore_runtime_counters_halts_when_daily_loss_exceeded() {
        let mut config = RiskConfig::default();
        config.daily_loss_limit = Decimal::from(50);
        let gate = RiskGate::new(config);

        gate.register_agent("agent1", AgentRiskParams::default())
            .await;

        let today = Utc::now().date_naive();
        let mut domain_pnl = HashMap::new();
        domain_pnl.insert(Domain::Crypto, Decimal::from(-60));

        gate.restore_runtime_counters(
            today,
            Decimal::from(-60),
            domain_pnl,
            4,
            1,
            3,
            1,
            HashMap::new(),
            HashMap::new(),
            Some(Utc::now()),
        )
        .await;

        assert_eq!(gate.state().await, PlatformRiskState::Halted);
        let (total_pnl, success, failure) = gate.daily_stats().await;
        assert_eq!(total_pnl, Decimal::from(-60));
        assert_eq!(success, 1);
        assert_eq!(failure, 3);
    }
}
