//! Risk Gate - 訂單風控閘門
//!
//! 負責在訂單執行前進行多層風控檢查：
//! - Agent 級別風控 (單筆限額、市場限制)
//! - 平台級別風控 (總暴露、熔斷機制)
//! - 組合級別風控 (每日損失、連續失敗)

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use super::traits::AgentRiskParams;
use super::types::{Domain, OrderIntent, OrderPriority};

/// 風控配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// 平台最大總暴露 (USD)
    pub max_platform_exposure: Decimal,
    /// 最大連續失敗次數 (熔斷)
    pub max_consecutive_failures: u32,
    /// 每日最大損失 (USD)
    pub daily_loss_limit: Decimal,
    /// 最大點差 (basis points)
    pub max_spread_bps: u32,
    /// 緊急訂單是否跳過部分檢查
    pub critical_bypass_exposure: bool,
    /// Enable automatic circuit-breaker recovery after cooldown.
    #[serde(default = "default_circuit_breaker_auto_recover")]
    pub circuit_breaker_auto_recover: bool,
    /// Cooldown before auto-recovering from HALTED state.
    #[serde(default = "default_circuit_breaker_cooldown_secs")]
    pub circuit_breaker_cooldown_secs: u64,
    /// Optional per-domain exposure caps (USD)
    pub crypto_max_exposure: Option<Decimal>,
    pub sports_max_exposure: Option<Decimal>,
    pub politics_max_exposure: Option<Decimal>,
    pub economics_max_exposure: Option<Decimal>,
    /// Optional per-domain daily loss limits (USD)
    pub crypto_daily_loss_limit: Option<Decimal>,
    pub sports_daily_loss_limit: Option<Decimal>,
    pub politics_daily_loss_limit: Option<Decimal>,
    pub economics_daily_loss_limit: Option<Decimal>,
}

fn default_circuit_breaker_auto_recover() -> bool {
    true
}

fn default_circuit_breaker_cooldown_secs() -> u64 {
    300
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_platform_exposure: Decimal::from(5000),
            max_consecutive_failures: 5,
            daily_loss_limit: Decimal::from(1000),
            max_spread_bps: 500, // 5%
            critical_bypass_exposure: false,
            circuit_breaker_auto_recover: default_circuit_breaker_auto_recover(),
            circuit_breaker_cooldown_secs: default_circuit_breaker_cooldown_secs(),
            crypto_max_exposure: None,
            sports_max_exposure: None,
            politics_max_exposure: None,
            economics_max_exposure: None,
            crypto_daily_loss_limit: None,
            sports_daily_loss_limit: None,
            politics_daily_loss_limit: None,
            economics_daily_loss_limit: None,
        }
    }
}

impl RiskConfig {
    fn domain_exposure_limit(&self, domain: Domain) -> Option<Decimal> {
        match domain {
            Domain::Crypto => self.crypto_max_exposure,
            Domain::Sports => self.sports_max_exposure,
            Domain::Politics => self.politics_max_exposure,
            Domain::Economics => self.economics_max_exposure,
            Domain::Custom(_) => None,
        }
    }

    fn domain_daily_loss_limit(&self, domain: Domain) -> Option<Decimal> {
        match domain {
            Domain::Crypto => self.crypto_daily_loss_limit,
            Domain::Sports => self.sports_daily_loss_limit,
            Domain::Politics => self.politics_daily_loss_limit,
            Domain::Economics => self.economics_daily_loss_limit,
            Domain::Custom(_) => None,
        }
    }
}

/// 風控檢查結果
#[derive(Debug, Clone)]
pub enum RiskCheckResult {
    /// 通過
    Passed,
    /// 被攔截
    Blocked(BlockReason),
    /// 需要調整 (例如減少數量)
    Adjusted(AdjustmentSuggestion),
}

impl RiskCheckResult {
    pub fn is_passed(&self) -> bool {
        matches!(self, RiskCheckResult::Passed)
    }

    pub fn is_blocked(&self) -> bool {
        matches!(self, RiskCheckResult::Blocked(_))
    }
}

/// 攔截原因
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BlockReason {
    /// 熔斷觸發
    CircuitBreakerTripped { reason: String },
    /// 超過單筆限額
    ExceedsSingleLimit { limit: Decimal, requested: Decimal },
    /// 超過總暴露
    ExceedsTotalExposure {
        limit: Decimal,
        current: Decimal,
        requested: Decimal,
    },
    /// Domain exposure cap exceeded
    DomainExposureExceeded {
        domain: Domain,
        limit: Decimal,
        current: Decimal,
        requested: Decimal,
    },
    /// 每日損失超限
    DailyLossExceeded { limit: Decimal, current: Decimal },
    /// Domain daily loss cap exceeded
    DomainDailyLossExceeded {
        domain: Domain,
        limit: Decimal,
        current: Decimal,
    },
    /// 市場不允許
    MarketNotAllowed { market: String, agent: String },
    /// Agent 狀態不允許交易
    AgentNotActive { agent: String, status: String },
    /// 訂單已過期
    OrderExpired,
    /// 未對沖倉位過多
    TooManyUnhedgedPositions { limit: u32, current: u32 },
}

impl std::fmt::Display for BlockReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockReason::CircuitBreakerTripped { reason } => {
                write!(f, "Circuit breaker: {}", reason)
            }
            BlockReason::ExceedsSingleLimit { limit, requested } => {
                write!(f, "Single order ${} exceeds limit ${}", requested, limit)
            }
            BlockReason::ExceedsTotalExposure {
                limit,
                current,
                requested,
            } => {
                write!(
                    f,
                    "Total exposure ${} + ${} exceeds ${}",
                    current, requested, limit
                )
            }
            BlockReason::DomainExposureExceeded {
                domain,
                limit,
                current,
                requested,
            } => {
                write!(
                    f,
                    "{} exposure ${} + ${} exceeds ${}",
                    domain, current, requested, limit
                )
            }
            BlockReason::DailyLossExceeded { limit, current } => {
                write!(f, "Daily loss ${} exceeds limit ${}", current, limit)
            }
            BlockReason::DomainDailyLossExceeded {
                domain,
                limit,
                current,
            } => {
                write!(
                    f,
                    "{} daily loss ${} exceeds limit ${}",
                    domain, current, limit
                )
            }
            BlockReason::MarketNotAllowed { market, agent } => {
                write!(f, "Agent {} not allowed in market {}", agent, market)
            }
            BlockReason::AgentNotActive { agent, status } => {
                write!(f, "Agent {} is {} (not active)", agent, status)
            }
            BlockReason::OrderExpired => write!(f, "Order has expired"),
            BlockReason::TooManyUnhedgedPositions { limit, current } => {
                write!(f, "Unhedged positions {} exceeds limit {}", current, limit)
            }
        }
    }
}

/// 調整建議
#[derive(Debug, Clone)]
pub struct AdjustmentSuggestion {
    /// 建議的最大數量
    pub max_shares: u64,
    /// 原因
    pub reason: String,
}

/// 平台風控狀態
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlatformRiskState {
    /// 正常
    Normal,
    /// 警戒 (減少新開倉)
    Elevated,
    /// 熔斷 (停止交易)
    Halted,
}

impl Default for PlatformRiskState {
    fn default() -> Self {
        PlatformRiskState::Normal
    }
}

impl PlatformRiskState {
    pub fn can_trade(&self) -> bool {
        !matches!(self, PlatformRiskState::Halted)
    }

    pub fn can_open_new(&self) -> bool {
        matches!(self, PlatformRiskState::Normal)
    }
}

/// Circuit breaker state transitions (for UI/audit)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerEvent {
    pub timestamp: DateTime<Utc>,
    pub reason: String,
    pub state: PlatformRiskState,
}

/// Agent 風控統計
#[derive(Debug, Clone, Default)]
struct AgentRiskStats {
    /// 當前暴露
    exposure: Decimal,
    /// 未實現損益
    unrealized_pnl: Decimal,
    /// 今日已實現損益
    realized_pnl: Decimal,
    /// 持倉數量
    position_count: usize,
    /// 未對沖倉位數量
    unhedged_count: u32,
    /// 連續失敗
    consecutive_failures: u32,
    /// 最後更新
    last_update: Option<DateTime<Utc>>,
}

/// 每日統計
#[derive(Debug, Clone, Default)]
struct DailyStats {
    date: Option<NaiveDate>,
    total_pnl: Decimal,
    domain_pnl: HashMap<Domain, Decimal>,
    order_count: u32,
    success_count: u32,
    failure_count: u32,
}

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
                        "No risk params for agent {}, using defaults",
                        intent.agent_id
                    );
                    AgentRiskParams::default()
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

    /// 記錄成功執行
    pub async fn record_success(&self, agent_id: &str, pnl: Decimal) {
        let domain = self.agent_domains.read().await.get(agent_id).copied();

        // 重置連續失敗
        self.consecutive_failures.store(0, Ordering::SeqCst);

        // 更新 Agent 統計
        {
            let mut stats_map = self.agent_stats.write().await;
            let stats = stats_map.entry(agent_id.to_string()).or_default();
            stats.consecutive_failures = 0;
            stats.realized_pnl += pnl;
        }

        // 更新每日統計
        {
            let mut daily = self.daily_stats.write().await;
            self.ensure_daily_reset(&mut daily);
            daily.total_pnl += pnl;
            if let Some(domain) = domain {
                *daily.domain_pnl.entry(domain).or_insert(Decimal::ZERO) += pnl;
            }
            daily.order_count += 1;
            daily.success_count += 1;
        }

        // 如果處於警戒狀態，考慮恢復正常
        if *self.state.read().await == PlatformRiskState::Elevated {
            *self.state.write().await = PlatformRiskState::Normal;
            info!("Risk state normalized after successful execution");
        }
    }

    /// 記錄失敗
    pub async fn record_failure(&self, agent_id: &str, reason: &str) {
        let global_failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;

        // 更新 Agent 統計
        let agent_failures = {
            let mut stats_map = self.agent_stats.write().await;
            let stats = stats_map.entry(agent_id.to_string()).or_default();
            stats.consecutive_failures += 1;
            stats.consecutive_failures
        };

        // 更新每日統計
        {
            let mut daily = self.daily_stats.write().await;
            self.ensure_daily_reset(&mut daily);
            daily.order_count += 1;
            daily.failure_count += 1;
        }

        warn!(
            "Agent {} failed: {}. Failures: agent={}, global={}",
            agent_id, reason, agent_failures, global_failures
        );

        // 檢查熔斷
        if global_failures >= self.config.max_consecutive_failures {
            self.trigger_circuit_breaker("Too many consecutive failures")
                .await;
        } else if global_failures >= self.config.max_consecutive_failures / 2 {
            *self.state.write().await = PlatformRiskState::Elevated;
            warn!("Platform risk elevated due to failures");
        }
    }

    /// 記錄損失
    pub async fn record_loss(&self, agent_id: &str, loss: Decimal) {
        let domain = self.agent_domains.read().await.get(agent_id).copied();

        // 更新 Agent 統計
        {
            let mut stats_map = self.agent_stats.write().await;
            let stats = stats_map.entry(agent_id.to_string()).or_default();
            stats.realized_pnl -= loss.abs();
        }

        // 更新每日損益
        let should_halt = {
            let mut daily = self.daily_stats.write().await;
            self.ensure_daily_reset(&mut daily);
            daily.total_pnl -= loss.abs();
            if let Some(domain) = domain {
                *daily.domain_pnl.entry(domain).or_insert(Decimal::ZERO) -= loss.abs();
            }
            daily.total_pnl.abs() >= self.config.daily_loss_limit
        };

        if should_halt {
            self.trigger_circuit_breaker("Daily loss limit exceeded")
                .await;
        }
    }

    /// 觸發熔斷
    pub async fn trigger_circuit_breaker(&self, reason: &str) {
        let mut state = self.state.write().await;
        if *state == PlatformRiskState::Halted {
            return;
        }
        error!("CIRCUIT BREAKER TRIGGERED: {}", reason);
        *state = PlatformRiskState::Halted;
        drop(state);

        *self.halted_at.write().await = Some(Utc::now());
        self.push_circuit_event(reason.to_string(), PlatformRiskState::Halted)
            .await;
    }

    /// 重置熔斷
    pub async fn reset_circuit_breaker(&self) {
        self.reset_circuit_breaker_with_reason("reset".to_string())
            .await;
    }

    async fn reset_circuit_breaker_with_reason(&self, reason: String) {
        info!("Circuit breaker reset: {}", reason);
        *self.state.write().await = PlatformRiskState::Normal;
        self.consecutive_failures.store(0, Ordering::SeqCst);
        *self.halted_at.write().await = None;

        // 重置所有 Agent 失敗計數
        let mut stats_map = self.agent_stats.write().await;
        for stats in stats_map.values_mut() {
            stats.consecutive_failures = 0;
        }
        drop(stats_map);

        self.push_circuit_event(reason, PlatformRiskState::Normal)
            .await;
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

    /// Restore runtime counters after coordinator cold-start replay.
    pub async fn restore_runtime_counters(
        &self,
        date: NaiveDate,
        total_pnl: Decimal,
        domain_pnl: HashMap<Domain, Decimal>,
        order_count: u32,
        success_count: u32,
        failure_count: u32,
        consecutive_failures: u32,
        agent_realized_pnl: HashMap<String, Decimal>,
        agent_consecutive_failures: HashMap<String, u32>,
        last_risk_event_at: Option<DateTime<Utc>>,
    ) {
        {
            let mut daily = self.daily_stats.write().await;
            *daily = DailyStats {
                date: Some(date),
                total_pnl,
                domain_pnl,
                order_count,
                success_count,
                failure_count,
            };
        }

        self.consecutive_failures
            .store(consecutive_failures, Ordering::SeqCst);

        {
            let mut stats_map = self.agent_stats.write().await;
            for (agent_id, realized_pnl) in agent_realized_pnl {
                let stats = stats_map.entry(agent_id).or_default();
                stats.realized_pnl = realized_pnl;
            }
            for (agent_id, failures) in agent_consecutive_failures {
                let stats = stats_map.entry(agent_id).or_default();
                stats.consecutive_failures = failures;
            }
        }

        let failure_limit = self.config.max_consecutive_failures.max(1);
        let daily_loss_exceeded =
            total_pnl < Decimal::ZERO && total_pnl.abs() >= self.config.daily_loss_limit;
        let next_state = if daily_loss_exceeded || consecutive_failures >= failure_limit {
            PlatformRiskState::Halted
        } else if consecutive_failures >= (failure_limit / 2).max(1) {
            PlatformRiskState::Elevated
        } else {
            PlatformRiskState::Normal
        };

        {
            let mut state = self.state.write().await;
            *state = next_state;
        }

        {
            let mut halted_at = self.halted_at.write().await;
            *halted_at = if next_state == PlatformRiskState::Halted {
                Some(last_risk_event_at.unwrap_or_else(Utc::now))
            } else {
                None
            };
        }

        debug!(
            date = %date,
            total_pnl = %total_pnl,
            order_count,
            success_count,
            failure_count,
            consecutive_failures,
            state = ?next_state,
            "restored risk gate runtime counters"
        );
    }

    /// Daily loss limit (USD)
    pub fn daily_loss_limit(&self) -> Decimal {
        self.config.daily_loss_limit
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

    fn ensure_daily_reset(&self, daily: &mut DailyStats) {
        let today = Utc::now().date_naive();
        if daily.date != Some(today) {
            *daily = DailyStats {
                date: Some(today),
                ..Default::default()
            };
        }
    }

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
        self.circuit_events.write().await.clear();
        *self.halted_at.write().await = None;
    }

    async fn try_auto_recover_circuit_breaker(&self) {
        if !self.config.circuit_breaker_auto_recover {
            return;
        }
        if *self.state.read().await != PlatformRiskState::Halted {
            return;
        }

        let halted_at = *self.halted_at.read().await;
        let Some(halted_at) = halted_at else {
            self.reset_circuit_breaker_with_reason(
                "auto-recover: missing halted timestamp".to_string(),
            )
            .await;
            return;
        };

        let elapsed_secs = Utc::now()
            .signed_duration_since(halted_at)
            .num_seconds()
            .max(0) as u64;
        if elapsed_secs < self.config.circuit_breaker_cooldown_secs {
            return;
        }

        self.reset_circuit_breaker_with_reason(format!(
            "auto-recover after cooldown ({}s >= {}s)",
            elapsed_secs, self.config.circuit_breaker_cooldown_secs
        ))
        .await;
    }

    async fn push_circuit_event(&self, reason: String, state: PlatformRiskState) {
        let mut events = self.circuit_events.write().await;
        events.push(CircuitBreakerEvent {
            timestamp: Utc::now(),
            reason,
            state,
        });
        if events.len() > 100 {
            let drain = events.len() - 100;
            events.drain(0..drain);
        }
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
