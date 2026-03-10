//! Risk Gate - 訂單風控閘門
//!
//! 負責在訂單執行前進行多層風控檢查：
//! - Agent 級別風控 (單筆限額、市場限制)
//! - 平台級別風控 (總暴露、熔斷機制)
//! - 組合級別風控 (每日損失、連續失敗)

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

use super::types::{Domain, OrderIntent, OrderPriority};
use crate::agent_runtime::AgentRiskParams;
mod checks;
mod config;
mod exposure;
mod queries;
mod stats;
mod transitions;
mod types;

pub use self::config::RiskConfig;
use self::stats::{AgentRiskStats, DailyStats, DrawdownStats};
pub use self::types::{
    AdjustmentSuggestion, BlockReason, CircuitBreakerEvent, DrawdownSnapshot, PlatformRiskState,
    RiskCheckResult,
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
    async fn test_unregister_agent_releases_domain_exposure() {
        let gate = RiskGate::new(RiskConfig::default());

        gate.register_agent_with_domain("agent1", Domain::Crypto, AgentRiskParams::default())
            .await;
        gate.update_agent_exposure("agent1", Decimal::from(15), Decimal::ZERO, 1, 0)
            .await;

        assert_eq!(*gate.total_exposure.read().await, Decimal::from(15));
        assert_eq!(
            gate.domain_exposure
                .read()
                .await
                .get(&Domain::Crypto)
                .copied(),
            Some(Decimal::from(15))
        );

        gate.unregister_agent("agent1").await;

        assert_eq!(gate.domain_exposure.read().await.get(&Domain::Crypto), None);
        assert_eq!(gate.agent_stats("agent1").await, None);
        assert!(gate.agent_params.read().await.get("agent1").is_none());
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
    async fn test_query_helpers_report_runtime_snapshots() {
        let mut config = RiskConfig::default();
        config.max_consecutive_failures = 1;
        config.circuit_breaker_auto_recover = false;
        let gate = RiskGate::new(config);

        gate.register_agent("agent1", AgentRiskParams::default())
            .await;
        gate.record_success("agent1", Decimal::from(10)).await;
        gate.record_success("agent1", Decimal::from(-4)).await;

        let (daily_pnl, success_count, failure_count) = gate.daily_stats().await;
        assert_eq!(daily_pnl, Decimal::from(6));
        assert_eq!(success_count, 2);
        assert_eq!(failure_count, 0);

        let snapshot = gate.drawdown_snapshot().await;
        assert_eq!(snapshot.current_equity, Decimal::from(6));
        assert_eq!(snapshot.equity_peak, Decimal::from(10));
        assert_eq!(snapshot.current_drawdown, Decimal::from(4));

        gate.record_failure("agent1", "forced failure").await;
        assert_eq!(gate.state().await, PlatformRiskState::Halted);
        assert!(!gate.can_trade().await);
        assert!(!gate.circuit_breaker_events().await.is_empty());
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
