//! Coordinator — central orchestrator for multi-agent trading
//!
//! The Coordinator owns the order queue, risk gate, and position aggregator.
//! Agents communicate with it via `CoordinatorHandle` (clone-friendly).
//! The main `run()` loop uses `tokio::select!` to:
//!   - Process incoming order intents (risk check → enqueue)
//!   - Process agent state updates (heartbeats)
//!   - Periodically drain the queue and execute orders
//!   - Periodically refresh GlobalState from aggregators

use chrono::{Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use sqlx::PgPool;

use crate::domain::OrderRequest;
use crate::error::Result;
use crate::platform::{
    AgentRiskParams, Domain, OrderIntent, OrderPriority, OrderQueue, PositionAggregator,
    RiskCheckResult, RiskGate,
};
use crate::strategy::executor::OrderExecutor;

use super::command::{CoordinatorCommand, CoordinatorControlCommand};
use super::config::CoordinatorConfig;
use super::state::{AgentSnapshot, GlobalState, QueueStatsSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngressMode {
    Running,
    Paused,
    Halted,
}

#[derive(Debug, Clone)]
struct AgentCommandChannel {
    domain: Domain,
    tx: mpsc::Sender<CoordinatorCommand>,
}

/// Clonable handle given to agents for submitting orders and state updates
#[derive(Clone)]
pub struct CoordinatorHandle {
    order_tx: mpsc::Sender<OrderIntent>,
    state_tx: mpsc::Sender<AgentSnapshot>,
    control_tx: mpsc::Sender<CoordinatorControlCommand>,
    global_state: Arc<RwLock<GlobalState>>,
    ingress_mode: Arc<RwLock<IngressMode>>,
    domain_ingress_mode: Arc<RwLock<HashMap<Domain, IngressMode>>>,
}

impl CoordinatorHandle {
    /// Submit an order intent to the coordinator for risk checking and execution
    pub async fn submit_order(&self, intent: OrderIntent) -> Result<()> {
        let global_mode = *self.ingress_mode.read().await;
        let domain_mode = self
            .domain_ingress_mode
            .read()
            .await
            .get(&intent.domain)
            .copied()
            .unwrap_or(IngressMode::Running);

        if global_mode != IngressMode::Running {
            return Err(crate::error::PloyError::Validation(format!(
                "coordinator global ingress is {:?}; new intents are blocked",
                global_mode
            )));
        }

        if domain_mode != IngressMode::Running {
            return Err(crate::error::PloyError::Validation(format!(
                "coordinator {:?} ingress is {:?}; new intents are blocked",
                intent.domain, domain_mode
            )));
        }
        self.order_tx.send(intent).await.map_err(|_| {
            crate::error::PloyError::Internal("coordinator order channel closed".into())
        })
    }

    /// Report agent state (heartbeat + position/PnL snapshot)
    pub async fn update_agent_state(&self, snapshot: AgentSnapshot) -> Result<()> {
        self.state_tx.send(snapshot).await.map_err(|_| {
            crate::error::PloyError::Internal("coordinator state channel closed".into())
        })
    }

    /// Pause all agents
    pub async fn pause_all(&self) -> Result<()> {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Paused;
        }
        self.domain_ingress_mode.write().await.clear();
        self.control_tx
            .send(CoordinatorControlCommand::PauseAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Resume all agents
    pub async fn resume_all(&self) -> Result<()> {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Running;
        }
        self.domain_ingress_mode.write().await.clear();
        self.control_tx
            .send(CoordinatorControlCommand::ResumeAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Force-close all positions and stop agents
    pub async fn force_close_all(&self) -> Result<()> {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Halted;
        }
        self.domain_ingress_mode.write().await.clear();
        self.control_tx
            .send(CoordinatorControlCommand::ForceCloseAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Shutdown all agents gracefully
    pub async fn shutdown_all(&self) -> Result<()> {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Halted;
        }
        self.domain_ingress_mode.write().await.clear();
        self.control_tx
            .send(CoordinatorControlCommand::ShutdownAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Pause a specific domain
    pub async fn pause_domain(&self, domain: Domain) -> Result<()> {
        {
            let mut domain_mode = self.domain_ingress_mode.write().await;
            domain_mode.insert(domain, IngressMode::Paused);
        }
        self.control_tx
            .send(CoordinatorControlCommand::PauseDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Resume a specific domain
    pub async fn resume_domain(&self, domain: Domain) -> Result<()> {
        {
            let mut domain_mode = self.domain_ingress_mode.write().await;
            domain_mode.remove(&domain);
        }
        self.control_tx
            .send(CoordinatorControlCommand::ResumeDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Force-close positions for one domain
    pub async fn force_close_domain(&self, domain: Domain) -> Result<()> {
        self.control_tx
            .send(CoordinatorControlCommand::ForceCloseDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Shutdown one domain
    pub async fn shutdown_domain(&self, domain: Domain) -> Result<()> {
        self.control_tx
            .send(CoordinatorControlCommand::ShutdownDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Read the current global state (non-blocking snapshot)
    pub async fn read_state(&self) -> GlobalState {
        self.global_state.read().await.clone()
    }
}

/// The Coordinator — owns shared infrastructure and runs the main event loop
pub struct Coordinator {
    config: CoordinatorConfig,
    account_id: String,
    risk_gate: Arc<RiskGate>,
    order_queue: Arc<RwLock<OrderQueue>>,
    duplicate_guard: Arc<RwLock<IntentDuplicateGuard>>,
    crypto_allocator: Arc<RwLock<CryptoCapitalAllocator>>,
    sports_allocator: Arc<RwLock<SportsCapitalAllocator>>,
    positions: Arc<PositionAggregator>,
    executor: Arc<OrderExecutor>,
    global_state: Arc<RwLock<GlobalState>>,
    execution_log_pool: Option<PgPool>,
    ingress_mode: Arc<RwLock<IngressMode>>,
    domain_ingress_mode: Arc<RwLock<HashMap<Domain, IngressMode>>>,
    stale_heartbeat_warn_at: Arc<RwLock<HashMap<String, chrono::DateTime<Utc>>>>,

    // Channels
    order_tx: mpsc::Sender<OrderIntent>,
    order_rx: mpsc::Receiver<OrderIntent>,
    state_tx: mpsc::Sender<AgentSnapshot>,
    state_rx: mpsc::Receiver<AgentSnapshot>,
    control_tx: mpsc::Sender<CoordinatorControlCommand>,
    control_rx: mpsc::Receiver<CoordinatorControlCommand>,

    // Per-agent command channels
    agent_commands: HashMap<String, AgentCommandChannel>,
}

#[derive(Debug)]
struct IntentDuplicateGuard {
    enabled: bool,
    window: ChronoDuration,
    recent_buys: HashMap<String, chrono::DateTime<Utc>>,
}

impl IntentDuplicateGuard {
    fn new(window_ms: u64, enabled: bool) -> Self {
        let clamped_ms = window_ms.min(i64::MAX as u64) as i64;
        let window = ChronoDuration::milliseconds(clamped_ms.max(1));
        Self {
            enabled,
            window,
            recent_buys: HashMap::new(),
        }
    }

    fn deployment_scope(intent: &OrderIntent) -> String {
        if let Some(scope) = intent
            .metadata
            .get("deployment_id")
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty())
        {
            return scope;
        }

        let strategy = intent
            .metadata
            .get("strategy")
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "default".to_string());

        format!(
            "agent:{}|strategy:{}",
            intent.agent_id.trim().to_ascii_lowercase(),
            strategy
        )
    }

    fn buy_key(intent: &OrderIntent) -> Option<String> {
        // Only guard normal/high-priority ENTRY orders.
        // Use market-level key so opposite-side re-entries on the same round
        // are also blocked within the duplicate window.
        // Scope by deployment to avoid blocking independent strategy deployments.
        if !intent.is_buy || intent.priority == OrderPriority::Critical {
            return None;
        }

        Some(format!(
            "{}|{}|{}",
            intent.domain,
            Self::deployment_scope(intent),
            intent.market_slug.trim().to_ascii_lowercase()
        ))
    }

    fn prune(&mut self, now: chrono::DateTime<Utc>) {
        self.recent_buys
            .retain(|_, ts| now.signed_duration_since(*ts) < self.window);
    }

    fn register_or_block(
        &mut self,
        intent: &OrderIntent,
        now: chrono::DateTime<Utc>,
    ) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let key = Self::buy_key(intent)?;
        self.prune(now);

        if let Some(last) = self.recent_buys.get(&key) {
            let elapsed_ms = now.signed_duration_since(*last).num_milliseconds().max(0);
            return Some(format!(
                "Duplicate buy intent blocked (elapsed={}ms, guard_window={}ms, key={})",
                elapsed_ms,
                self.window.num_milliseconds(),
                key
            ));
        }

        self.recent_buys.insert(key, now);
        None
    }
}

const KNOWN_5M_SERIES_IDS: &[&str] = &["10684", "10683", "10686", "10685"];
const KNOWN_15M_SERIES_IDS: &[&str] = &["10192", "10191", "10423", "10422"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CryptoHorizon {
    M5,
    M15,
    Other,
}

impl CryptoHorizon {
    fn as_str(&self) -> &'static str {
        match self {
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::Other => "other",
        }
    }

    fn from_hint(raw: &str) -> Option<Self> {
        let normalized = raw.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return None;
        }
        if normalized.contains("15m") || normalized == "15" {
            return Some(Self::M15);
        }
        if normalized.contains("5m") || normalized == "5" {
            return Some(Self::M5);
        }
        if KNOWN_15M_SERIES_IDS.iter().any(|id| *id == normalized) {
            return Some(Self::M15);
        }
        if KNOWN_5M_SERIES_IDS.iter().any(|id| *id == normalized) {
            return Some(Self::M5);
        }
        None
    }
}

#[derive(Debug, Clone)]
struct CryptoIntentDimensions {
    coin: String,
    horizon: CryptoHorizon,
    position_key: String,
}

impl CryptoIntentDimensions {
    fn from_intent(intent: &OrderIntent) -> Self {
        let coin = Self::parse_coin(intent).unwrap_or_else(|| "OTHER".to_string());
        let horizon = Self::parse_horizon(intent).unwrap_or(CryptoHorizon::Other);
        let position_key = format!(
            "{}|{}|{}|{}",
            intent.agent_id,
            intent.market_slug,
            intent.token_id,
            intent.side.as_str()
        );
        Self {
            coin,
            horizon,
            position_key,
        }
    }

    fn parse_coin(intent: &OrderIntent) -> Option<String> {
        if let Some(coin) = intent
            .metadata
            .get("coin")
            .and_then(|raw| Self::normalize_coin(raw))
        {
            return Some(coin);
        }

        if let Some(symbol) = intent.metadata.get("symbol") {
            let cleaned = symbol
                .trim()
                .to_ascii_uppercase()
                .replace("USDT", "")
                .replace("USD", "");
            if let Some(coin) = Self::normalize_coin(&cleaned) {
                return Some(coin);
            }
        }

        let slug = intent.market_slug.to_ascii_lowercase();
        for (needle, coin) in [
            ("bitcoin", "BTC"),
            ("btc", "BTC"),
            ("ethereum", "ETH"),
            ("eth", "ETH"),
            ("solana", "SOL"),
            ("sol", "SOL"),
            ("xrp", "XRP"),
        ] {
            if slug.contains(needle) {
                return Some(coin.to_string());
            }
        }

        None
    }

    fn parse_horizon(intent: &OrderIntent) -> Option<CryptoHorizon> {
        if let Some(h) = intent
            .metadata
            .get("horizon")
            .and_then(|raw| CryptoHorizon::from_hint(raw))
        {
            return Some(h);
        }

        if let Some(h) = intent
            .metadata
            .get("event_series_id")
            .and_then(|raw| CryptoHorizon::from_hint(raw))
        {
            return Some(h);
        }

        if let Some(h) = intent
            .metadata
            .get("series_id")
            .and_then(|raw| CryptoHorizon::from_hint(raw))
        {
            return Some(h);
        }

        CryptoHorizon::from_hint(&intent.market_slug)
    }

    fn normalize_coin(raw: &str) -> Option<String> {
        let coin = raw.trim().to_ascii_uppercase();
        if coin.is_empty() {
            return None;
        }
        Some(match coin.as_str() {
            "BITCOIN" | "BTC" => "BTC".to_string(),
            "ETHEREUM" | "ETH" => "ETH".to_string(),
            "SOLANA" | "SOL" => "SOL".to_string(),
            "XRP" => "XRP".to_string(),
            other => other.to_string(),
        })
    }
}

#[derive(Debug, Clone)]
struct PositionExposure {
    coin: String,
    horizon: CryptoHorizon,
    amount: Decimal,
}

#[derive(Debug, Default)]
struct ExposureBook {
    total: Decimal,
    by_coin: HashMap<String, Decimal>,
    by_horizon: HashMap<CryptoHorizon, Decimal>,
    by_position: HashMap<String, PositionExposure>,
}

impl ExposureBook {
    fn value_for_coin(&self, coin: &str) -> Decimal {
        self.by_coin.get(coin).copied().unwrap_or(Decimal::ZERO)
    }

    fn value_for_horizon(&self, horizon: CryptoHorizon) -> Decimal {
        self.by_horizon
            .get(&horizon)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    fn add(&mut self, dims: &CryptoIntentDimensions, amount: Decimal) {
        if amount <= Decimal::ZERO {
            return;
        }
        self.total += amount;
        *self
            .by_coin
            .entry(dims.coin.clone())
            .or_insert(Decimal::ZERO) += amount;
        *self.by_horizon.entry(dims.horizon).or_insert(Decimal::ZERO) += amount;
        self.by_position
            .entry(dims.position_key.clone())
            .and_modify(|pos| {
                pos.amount += amount;
                pos.coin = dims.coin.clone();
                pos.horizon = dims.horizon;
            })
            .or_insert_with(|| PositionExposure {
                coin: dims.coin.clone(),
                horizon: dims.horizon,
                amount,
            });
    }

    fn subtract_from_position_key(&mut self, position_key: &str, amount: Decimal) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let mut removed = Decimal::ZERO;
        let mut coin = None;
        let mut horizon = None;
        let mut delete_key = false;

        if let Some(pos) = self.by_position.get_mut(position_key) {
            removed = amount.min(pos.amount);
            if removed > Decimal::ZERO {
                pos.amount -= removed;
                coin = Some(pos.coin.clone());
                horizon = Some(pos.horizon);
                delete_key = pos.amount <= Decimal::ZERO;
            }
        }

        if delete_key {
            self.by_position.remove(position_key);
        }

        if removed <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        self.total = (self.total - removed).max(Decimal::ZERO);

        if let Some(c) = coin {
            if let Some(v) = self.by_coin.get_mut(&c) {
                *v = (*v - removed).max(Decimal::ZERO);
                if *v == Decimal::ZERO {
                    self.by_coin.remove(&c);
                }
            }
        }

        if let Some(h) = horizon {
            if let Some(v) = self.by_horizon.get_mut(&h) {
                *v = (*v - removed).max(Decimal::ZERO);
                if *v == Decimal::ZERO {
                    self.by_horizon.remove(&h);
                }
            }
        }

        removed
    }

    fn subtract_matching_bucket(
        &mut self,
        coin: &str,
        horizon: CryptoHorizon,
        amount: Decimal,
    ) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let mut remaining = amount;
        let keys: Vec<String> = self
            .by_position
            .iter()
            .filter(|(_, p)| p.coin == coin && p.horizon == horizon)
            .map(|(k, _)| k.clone())
            .collect();

        for key in keys {
            if remaining <= Decimal::ZERO {
                break;
            }
            let removed = self.subtract_from_position_key(&key, remaining);
            remaining -= removed;
        }

        amount - remaining
    }
}

#[derive(Debug, Clone)]
struct PendingCryptoIntent {
    dims: CryptoIntentDimensions,
    requested_notional: Decimal,
}

#[derive(Debug)]
struct CryptoCapitalAllocator {
    enabled: bool,
    total_cap: Decimal,
    coin_cap_pct: HashMap<String, Decimal>,
    horizon_cap_pct: HashMap<CryptoHorizon, Decimal>,
    open: ExposureBook,
    pending: ExposureBook,
    pending_by_intent: HashMap<Uuid, PendingCryptoIntent>,
}

impl CryptoCapitalAllocator {
    fn new(config: &CoordinatorConfig) -> Self {
        let configured_cap = config
            .crypto_allocator_total_cap_usd
            .or(config.risk.crypto_max_exposure)
            .unwrap_or(config.risk.max_platform_exposure);
        let total_cap = config
            .risk
            .crypto_max_exposure
            .map(|risk_cap| configured_cap.min(risk_cap))
            .unwrap_or(configured_cap)
            .max(Decimal::ZERO);

        let mut coin_cap_pct = HashMap::new();
        coin_cap_pct.insert(
            "BTC".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_btc_pct),
        );
        coin_cap_pct.insert(
            "ETH".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_eth_pct),
        );
        coin_cap_pct.insert(
            "SOL".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_sol_pct),
        );
        coin_cap_pct.insert(
            "XRP".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_xrp_pct),
        );
        coin_cap_pct.insert(
            "OTHER".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_other_pct),
        );

        let mut horizon_cap_pct = HashMap::new();
        horizon_cap_pct.insert(
            CryptoHorizon::M5,
            Self::normalize_pct(config.crypto_horizon_cap_5m_pct),
        );
        horizon_cap_pct.insert(
            CryptoHorizon::M15,
            Self::normalize_pct(config.crypto_horizon_cap_15m_pct),
        );
        horizon_cap_pct.insert(
            CryptoHorizon::Other,
            Self::normalize_pct(config.crypto_horizon_cap_other_pct),
        );

        Self {
            enabled: config.crypto_allocator_enabled,
            total_cap,
            coin_cap_pct,
            horizon_cap_pct,
            open: ExposureBook::default(),
            pending: ExposureBook::default(),
            pending_by_intent: HashMap::new(),
        }
    }

    fn normalize_pct(value: Decimal) -> Decimal {
        if value <= Decimal::ZERO {
            Decimal::ZERO
        } else if value >= Decimal::ONE {
            Decimal::ONE
        } else {
            value
        }
    }

    fn reserve_buy(&mut self, intent: &OrderIntent) -> std::result::Result<(), String> {
        if !self.enabled || intent.domain != Domain::Crypto || !intent.is_buy {
            return Ok(());
        }

        if self.total_cap <= Decimal::ZERO {
            return Err("Crypto allocator cap is 0; buy intent blocked".to_string());
        }

        let requested = intent.notional_value();
        if requested <= Decimal::ZERO {
            return Err("Crypto buy intent has non-positive notional".to_string());
        }

        let dims = CryptoIntentDimensions::from_intent(intent);

        let projected_total = self.open.total + self.pending.total + requested;
        if projected_total > self.total_cap {
            return Err(format!(
                "Crypto total cap exceeded: projected={} cap={}",
                projected_total, self.total_cap
            ));
        }

        let coin_cap = self.total_cap * self.coin_cap_for(&dims.coin);
        let projected_coin = self.open.value_for_coin(&dims.coin)
            + self.pending.value_for_coin(&dims.coin)
            + requested;
        if projected_coin > coin_cap {
            return Err(format!(
                "Crypto coin cap exceeded: coin={} projected={} cap={}",
                dims.coin, projected_coin, coin_cap
            ));
        }

        let horizon_cap = self.total_cap * self.horizon_cap_for(dims.horizon);
        let projected_horizon = self.open.value_for_horizon(dims.horizon)
            + self.pending.value_for_horizon(dims.horizon)
            + requested;
        if projected_horizon > horizon_cap {
            return Err(format!(
                "Crypto horizon cap exceeded: horizon={} projected={} cap={}",
                dims.horizon.as_str(),
                projected_horizon,
                horizon_cap
            ));
        }

        self.pending.add(&dims, requested);
        self.pending_by_intent.insert(
            intent.intent_id,
            PendingCryptoIntent {
                dims,
                requested_notional: requested,
            },
        );

        Ok(())
    }

    fn release_buy_reservation(&mut self, intent_id: Uuid) {
        let Some(reservation) = self.pending_by_intent.remove(&intent_id) else {
            return;
        };
        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );
    }

    fn settle_buy_execution(
        &mut self,
        intent: &OrderIntent,
        filled_shares: u64,
        fill_price: Decimal,
    ) {
        if !self.enabled || intent.domain != Domain::Crypto || !intent.is_buy {
            return;
        }

        let reservation = self
            .pending_by_intent
            .remove(&intent.intent_id)
            .unwrap_or_else(|| PendingCryptoIntent {
                dims: CryptoIntentDimensions::from_intent(intent),
                requested_notional: intent.notional_value(),
            });

        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );

        if filled_shares == 0 || fill_price <= Decimal::ZERO {
            return;
        }

        let actual_notional = fill_price * Decimal::from(filled_shares);
        self.open.add(&reservation.dims, actual_notional);
    }

    fn settle_sell_execution(
        &mut self,
        intent: &OrderIntent,
        filled_shares: u64,
        execution_price: Decimal,
    ) {
        if !self.enabled || intent.domain != Domain::Crypto || intent.is_buy || filled_shares == 0 {
            return;
        }

        let dims = CryptoIntentDimensions::from_intent(intent);
        let reference_price = intent
            .metadata
            .get("entry_price")
            .and_then(|v| Decimal::from_str(v).ok())
            .or_else(|| (execution_price > Decimal::ZERO).then_some(execution_price))
            .unwrap_or(intent.limit_price);

        if reference_price <= Decimal::ZERO {
            return;
        }

        let requested_release = Decimal::from(filled_shares) * reference_price;
        let removed_by_key = self
            .open
            .subtract_from_position_key(&dims.position_key, requested_release);
        if removed_by_key < requested_release {
            let remaining = requested_release - removed_by_key;
            self.open
                .subtract_matching_bucket(&dims.coin, dims.horizon, remaining);
        }
    }

    fn coin_cap_for(&self, coin: &str) -> Decimal {
        self.coin_cap_pct
            .get(coin)
            .copied()
            .or_else(|| self.coin_cap_pct.get("OTHER").copied())
            .unwrap_or(Decimal::ZERO)
    }

    fn horizon_cap_for(&self, horizon: CryptoHorizon) -> Decimal {
        self.horizon_cap_pct
            .get(&horizon)
            .copied()
            .or_else(|| self.horizon_cap_pct.get(&CryptoHorizon::Other).copied())
            .unwrap_or(Decimal::ZERO)
    }
}

#[derive(Debug, Clone)]
struct SportsIntentDimensions {
    market_key: String,
    position_key: String,
}

impl SportsIntentDimensions {
    fn from_intent(intent: &OrderIntent) -> Self {
        let market_key = intent.market_slug.trim().to_ascii_lowercase();
        let position_key = format!(
            "{}|{}|{}|{}",
            intent.agent_id,
            market_key,
            intent.token_id,
            intent.side.as_str()
        );
        Self {
            market_key,
            position_key,
        }
    }
}

#[derive(Debug, Clone)]
struct SportsPositionExposure {
    market_key: String,
    amount: Decimal,
}

#[derive(Debug, Default)]
struct SportsExposureBook {
    total: Decimal,
    by_market: HashMap<String, Decimal>,
    by_position: HashMap<String, SportsPositionExposure>,
}

impl SportsExposureBook {
    fn value_for_market(&self, market_key: &str) -> Decimal {
        self.by_market
            .get(market_key)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    fn add(&mut self, dims: &SportsIntentDimensions, amount: Decimal) {
        if amount <= Decimal::ZERO {
            return;
        }

        self.total += amount;
        *self
            .by_market
            .entry(dims.market_key.clone())
            .or_insert(Decimal::ZERO) += amount;
        self.by_position
            .entry(dims.position_key.clone())
            .and_modify(|pos| {
                pos.amount += amount;
                pos.market_key = dims.market_key.clone();
            })
            .or_insert_with(|| SportsPositionExposure {
                market_key: dims.market_key.clone(),
                amount,
            });
    }

    fn subtract_from_position_key(&mut self, position_key: &str, amount: Decimal) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let mut removed = Decimal::ZERO;
        let mut market_key = None;
        let mut delete_key = false;

        if let Some(pos) = self.by_position.get_mut(position_key) {
            removed = amount.min(pos.amount);
            if removed > Decimal::ZERO {
                pos.amount -= removed;
                market_key = Some(pos.market_key.clone());
                delete_key = pos.amount <= Decimal::ZERO;
            }
        }

        if delete_key {
            self.by_position.remove(position_key);
        }

        if removed <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        self.total = (self.total - removed).max(Decimal::ZERO);
        if let Some(market) = market_key {
            if let Some(v) = self.by_market.get_mut(&market) {
                *v = (*v - removed).max(Decimal::ZERO);
                if *v == Decimal::ZERO {
                    self.by_market.remove(&market);
                }
            }
        }

        removed
    }

    fn subtract_matching_market(&mut self, market_key: &str, amount: Decimal) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let mut remaining = amount;
        let keys: Vec<String> = self
            .by_position
            .iter()
            .filter(|(_, p)| p.market_key == market_key)
            .map(|(k, _)| k.clone())
            .collect();

        for key in keys {
            if remaining <= Decimal::ZERO {
                break;
            }
            let removed = self.subtract_from_position_key(&key, remaining);
            remaining -= removed;
        }

        amount - remaining
    }
}

#[derive(Debug, Clone)]
struct PendingSportsIntent {
    dims: SportsIntentDimensions,
    requested_notional: Decimal,
}

#[derive(Debug)]
struct SportsCapitalAllocator {
    enabled: bool,
    total_cap: Decimal,
    market_cap_pct: Decimal,
    auto_split_by_active_markets: bool,
    open: SportsExposureBook,
    pending: SportsExposureBook,
    pending_by_intent: HashMap<Uuid, PendingSportsIntent>,
}

impl SportsCapitalAllocator {
    fn new(config: &CoordinatorConfig) -> Self {
        let configured_cap = config
            .sports_allocator_total_cap_usd
            .or(config.risk.sports_max_exposure)
            .unwrap_or(config.risk.max_platform_exposure);
        let total_cap = config
            .risk
            .sports_max_exposure
            .map(|risk_cap| configured_cap.min(risk_cap))
            .unwrap_or(configured_cap)
            .max(Decimal::ZERO);

        Self {
            enabled: config.sports_allocator_enabled,
            total_cap,
            market_cap_pct: Self::normalize_pct(config.sports_market_cap_pct),
            auto_split_by_active_markets: config.sports_auto_split_by_active_markets,
            open: SportsExposureBook::default(),
            pending: SportsExposureBook::default(),
            pending_by_intent: HashMap::new(),
        }
    }

    fn normalize_pct(value: Decimal) -> Decimal {
        if value <= Decimal::ZERO {
            Decimal::ZERO
        } else if value >= Decimal::ONE {
            Decimal::ONE
        } else {
            value
        }
    }

    fn reserve_buy(&mut self, intent: &OrderIntent) -> std::result::Result<(), String> {
        if !self.enabled || intent.domain != Domain::Sports || !intent.is_buy {
            return Ok(());
        }

        if self.total_cap <= Decimal::ZERO {
            return Err("Sports allocator cap is 0; buy intent blocked".to_string());
        }

        let requested = intent.notional_value();
        if requested <= Decimal::ZERO {
            return Err("Sports buy intent has non-positive notional".to_string());
        }

        let dims = SportsIntentDimensions::from_intent(intent);

        let projected_total = self.open.total + self.pending.total + requested;
        if projected_total > self.total_cap {
            return Err(format!(
                "Sports total cap exceeded: projected={} cap={}",
                projected_total, self.total_cap
            ));
        }

        let market_cap = self.market_cap_for(&dims.market_key);
        let projected_market = self.open.value_for_market(&dims.market_key)
            + self.pending.value_for_market(&dims.market_key)
            + requested;
        if projected_market > market_cap {
            return Err(format!(
                "Sports market cap exceeded: market={} projected={} cap={}",
                dims.market_key, projected_market, market_cap
            ));
        }

        self.pending.add(&dims, requested);
        self.pending_by_intent.insert(
            intent.intent_id,
            PendingSportsIntent {
                dims,
                requested_notional: requested,
            },
        );

        Ok(())
    }

    fn release_buy_reservation(&mut self, intent_id: Uuid) {
        let Some(reservation) = self.pending_by_intent.remove(&intent_id) else {
            return;
        };
        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );
    }

    fn settle_buy_execution(
        &mut self,
        intent: &OrderIntent,
        filled_shares: u64,
        fill_price: Decimal,
    ) {
        if !self.enabled || intent.domain != Domain::Sports || !intent.is_buy {
            return;
        }

        let reservation = self
            .pending_by_intent
            .remove(&intent.intent_id)
            .unwrap_or_else(|| PendingSportsIntent {
                dims: SportsIntentDimensions::from_intent(intent),
                requested_notional: intent.notional_value(),
            });

        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );

        if filled_shares == 0 || fill_price <= Decimal::ZERO {
            return;
        }

        let actual_notional = fill_price * Decimal::from(filled_shares);
        self.open.add(&reservation.dims, actual_notional);
    }

    fn settle_sell_execution(
        &mut self,
        intent: &OrderIntent,
        filled_shares: u64,
        execution_price: Decimal,
    ) {
        if !self.enabled || intent.domain != Domain::Sports || intent.is_buy || filled_shares == 0 {
            return;
        }

        let dims = SportsIntentDimensions::from_intent(intent);
        let reference_price = intent
            .metadata
            .get("entry_price")
            .and_then(|v| Decimal::from_str(v).ok())
            .or_else(|| (execution_price > Decimal::ZERO).then_some(execution_price))
            .unwrap_or(intent.limit_price);

        if reference_price <= Decimal::ZERO {
            return;
        }

        let requested_release = Decimal::from(filled_shares) * reference_price;
        let removed_by_key = self
            .open
            .subtract_from_position_key(&dims.position_key, requested_release);
        if removed_by_key < requested_release {
            let remaining = requested_release - removed_by_key;
            self.open
                .subtract_matching_market(&dims.market_key, remaining);
        }
    }

    fn market_cap_for(&self, market_key: &str) -> Decimal {
        let fixed_cap = self.total_cap * self.market_cap_pct;
        if !self.auto_split_by_active_markets {
            return fixed_cap;
        }

        let mut active_markets: HashSet<String> = self
            .open
            .by_market
            .iter()
            .filter(|(_, v)| **v > Decimal::ZERO)
            .map(|(k, _)| k.clone())
            .collect();

        for (k, v) in &self.pending.by_market {
            if *v > Decimal::ZERO {
                active_markets.insert(k.clone());
            }
        }

        if !market_key.is_empty() {
            active_markets.insert(market_key.to_string());
        }

        let market_count = active_markets.len().max(1) as u64;
        let dynamic_cap = self.total_cap / Decimal::from(market_count);
        dynamic_cap.min(fixed_cap)
    }
}

impl Coordinator {
    pub fn new(
        config: CoordinatorConfig,
        executor: Arc<OrderExecutor>,
        account_id: String,
    ) -> Self {
        let (order_tx, order_rx) = mpsc::channel(256);
        let (state_tx, state_rx) = mpsc::channel(128);
        let (control_tx, control_rx) = mpsc::channel(32);

        let risk_gate = Arc::new(RiskGate::new(config.risk.clone()));
        let order_queue = Arc::new(RwLock::new(OrderQueue::new(1024)));
        let duplicate_guard = Arc::new(RwLock::new(IntentDuplicateGuard::new(
            config.duplicate_guard_window_ms,
            config.duplicate_guard_enabled,
        )));
        let crypto_allocator = Arc::new(RwLock::new(CryptoCapitalAllocator::new(&config)));
        let sports_allocator = Arc::new(RwLock::new(SportsCapitalAllocator::new(&config)));
        let positions = Arc::new(PositionAggregator::new());
        let global_state = Arc::new(RwLock::new(GlobalState::new()));
        let ingress_mode = Arc::new(RwLock::new(IngressMode::Running));
        let domain_ingress_mode = Arc::new(RwLock::new(HashMap::new()));
        let stale_heartbeat_warn_at = Arc::new(RwLock::new(HashMap::new()));
        let account_id = if account_id.trim().is_empty() {
            "default".to_string()
        } else {
            account_id
        };

        Self {
            config,
            account_id,
            risk_gate,
            order_queue,
            duplicate_guard,
            crypto_allocator,
            sports_allocator,
            positions,
            executor,
            global_state,
            execution_log_pool: None,
            ingress_mode,
            domain_ingress_mode,
            stale_heartbeat_warn_at,
            order_tx,
            order_rx,
            state_tx,
            state_rx,
            control_tx,
            control_rx,
            agent_commands: HashMap::new(),
        }
    }

    /// Enable DB logging for order execution outcomes (including dry-run).
    pub fn set_execution_log_pool(&mut self, pool: PgPool) {
        self.execution_log_pool = Some(pool);
    }

    /// Create a clonable handle for agents
    pub fn handle(&self) -> CoordinatorHandle {
        CoordinatorHandle {
            order_tx: self.order_tx.clone(),
            state_tx: self.state_tx.clone(),
            control_tx: self.control_tx.clone(),
            global_state: self.global_state.clone(),
            ingress_mode: self.ingress_mode.clone(),
            domain_ingress_mode: self.domain_ingress_mode.clone(),
        }
    }

    /// Shared global state reference (for TUI)
    pub fn global_state(&self) -> Arc<RwLock<GlobalState>> {
        self.global_state.clone()
    }

    /// Position aggregator reference
    pub fn positions(&self) -> Arc<PositionAggregator> {
        self.positions.clone()
    }

    /// Register an agent and return its command receiver
    pub fn register_agent(
        &mut self,
        agent_id: String,
        domain: Domain,
        risk_params: AgentRiskParams,
    ) -> mpsc::Receiver<CoordinatorCommand> {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        self.agent_commands
            .insert(agent_id.clone(), AgentCommandChannel { domain, tx: cmd_tx });

        // Register with risk gate (fire-and-forget via spawn since we're not async here)
        let risk_gate = self.risk_gate.clone();
        let id = agent_id.clone();
        tokio::spawn(async move {
            risk_gate
                .register_agent_with_domain(&id, domain, risk_params)
                .await;
        });

        info!(agent_id, "agent registered with coordinator");
        cmd_rx
    }

    /// Send a command to a specific agent
    pub async fn send_command(&self, agent_id: &str, cmd: CoordinatorCommand) -> Result<()> {
        if let Some(tx) = self.agent_commands.get(agent_id) {
            tx.tx.send(cmd).await.map_err(|_| {
                crate::error::PloyError::Internal(format!(
                    "agent {} command channel closed",
                    agent_id
                ))
            })
        } else {
            Err(crate::error::PloyError::Internal(format!(
                "agent {} not registered",
                agent_id
            )))
        }
    }

    fn domain_for_agent(&self, agent_id: &str) -> Option<Domain> {
        self.agent_commands.get(agent_id).map(|entry| entry.domain)
    }

    fn should_apply_domain_cmd(&self, entry: &AgentCommandChannel, target: Domain) -> bool {
        entry.domain == target
    }

    async fn set_domain_mode(&self, domain: Domain, mode: IngressMode) {
        let mut domain_modes = self.domain_ingress_mode.write().await;
        match mode {
            IngressMode::Running => {
                domain_modes.remove(&domain);
            }
            _ => {
                domain_modes.insert(domain, mode);
            }
        }
    }

    /// Pause all agents
    pub async fn pause_all(&self) {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Paused;
        }
        self.domain_ingress_mode.write().await.clear();
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::Pause).await {
                warn!(agent_id = %id, error = %e, "failed to send pause");
            }
        }
    }

    /// Resume all agents
    pub async fn resume_all(&self) {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Running;
        }
        self.domain_ingress_mode.write().await.clear();
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::Resume).await {
                warn!(agent_id = %id, error = %e, "failed to send resume");
            }
        }
    }

    /// Force-close all agents (best-effort)
    pub async fn force_close_all(&self) {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Halted;
        }
        self.domain_ingress_mode.write().await.clear();
        info!("coordinator: sending force-close to all agents");
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::ForceClose).await {
                warn!(agent_id = %id, error = %e, "failed to send force-close");
            }
        }
    }

    /// Shutdown all agents gracefully
    pub async fn shutdown(&self) {
        {
            let mut mode = self.ingress_mode.write().await;
            *mode = IngressMode::Halted;
        }
        self.domain_ingress_mode.write().await.clear();
        info!("coordinator: sending shutdown to all agents");
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::Shutdown).await {
                warn!(agent_id = %id, error = %e, "failed to send shutdown");
            }
        }
    }

    /// Pause one domain
    pub async fn pause_domain(&self, domain: Domain) {
        self.set_domain_mode(domain, IngressMode::Paused).await;
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::Pause).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain pause");
                }
            }
        }
    }

    /// Resume one domain
    pub async fn resume_domain(&self, domain: Domain) {
        self.set_domain_mode(domain, IngressMode::Running).await;
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::Resume).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain resume");
                }
            }
        }
    }

    /// Force-close all agents in one domain
    pub async fn force_close_domain(&self, domain: Domain) {
        self.set_domain_mode(domain, IngressMode::Halted).await;
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::ForceClose).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain force-close");
                }
            }
        }
    }

    /// Shutdown all agents in one domain
    pub async fn shutdown_domain(&self, domain: Domain) {
        self.set_domain_mode(domain, IngressMode::Halted).await;
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::Shutdown).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain shutdown");
                }
            }
        }
    }

    /// Main coordinator loop — blocks until shutdown
    pub async fn run(mut self, mut shutdown_rx: tokio::sync::broadcast::Receiver<()>) {
        info!(
            agents = self.agent_commands.len(),
            "coordinator starting main loop"
        );

        let drain_interval = tokio::time::Duration::from_millis(self.config.queue_drain_ms);
        let refresh_interval = tokio::time::Duration::from_millis(self.config.state_refresh_ms);

        let mut drain_tick = tokio::time::interval(drain_interval);
        let mut refresh_tick = tokio::time::interval(refresh_interval);

        // Don't burst-fire missed ticks
        drain_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                // --- Control commands (pause/resume/force-close) ---
                Some(cmd) = self.control_rx.recv() => {
                    match cmd {
                        CoordinatorControlCommand::PauseAll => self.pause_all().await,
                        CoordinatorControlCommand::ResumeAll => self.resume_all().await,
                        CoordinatorControlCommand::ForceCloseAll => self.force_close_all().await,
                        CoordinatorControlCommand::ShutdownAll => self.shutdown().await,
                        CoordinatorControlCommand::PauseDomain(domain) => self.pause_domain(domain).await,
                        CoordinatorControlCommand::ResumeDomain(domain) => {
                            self.resume_domain(domain).await
                        }
                        CoordinatorControlCommand::ForceCloseDomain(domain) => {
                            self.force_close_domain(domain).await
                        }
                        CoordinatorControlCommand::ShutdownDomain(domain) => {
                            self.shutdown_domain(domain).await
                        }
                    }
                }

                // --- Incoming order intents ---
                Some(intent) = self.order_rx.recv() => {
                    self.handle_order_intent(intent).await;
                }

                // --- Agent state updates (heartbeats) ---
                Some(snapshot) = self.state_rx.recv() => {
                    self.handle_state_update(snapshot).await;
                }

                // --- Periodic: drain queue and execute ---
                _ = drain_tick.tick() => {
                    self.drain_and_execute().await;
                }

                // --- Periodic: refresh global state ---
                _ = refresh_tick.tick() => {
                    self.refresh_global_state().await;
                }

                // --- Shutdown signal ---
                _ = shutdown_rx.recv() => {
                    info!("coordinator: shutdown signal received");
                    self.shutdown().await;
                    break;
                }
            }
        }

        info!("coordinator: main loop exited");
    }

    /// Risk-check an incoming order intent and enqueue if passed
    async fn handle_order_intent(&self, intent: OrderIntent) {
        let agent_id = intent.agent_id.clone();
        let intent_id = intent.intent_id;

        let ingress_mode = *self.ingress_mode.read().await;
        if ingress_mode != IngressMode::Running {
            let reason = format!(
                "Coordinator ingress is {:?}; blocking new intent while paused/halted",
                ingress_mode
            );
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by coordinator ingress state"
            );
            return;
        }
        let domain_mode = self
            .domain_ingress_mode
            .read()
            .await
            .get(&intent.domain)
            .copied()
            .unwrap_or(IngressMode::Running);
        if domain_mode != IngressMode::Running {
            let reason = format!(
                "Domain {:?} ingress is {:?}; blocking new intent while paused/halted",
                intent.domain, domain_mode
            );
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by coordinator domain ingress state"
            );
            return;
        }

        self.persist_signal_from_intent(&intent).await;
        if !intent.is_buy {
            self.persist_exit_reason_intent(&intent).await;
        }

        if let Some(reason) = self.check_duplicate_intent(&intent).await {
            self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by duplicate-intent guard"
            );
            return;
        }

        match self.risk_gate.check_order(&intent).await {
            RiskCheckResult::Passed => {
                if let Some(reason) = self.reserve_domain_capital(&intent).await {
                    self.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                        .await;
                    warn!(
                        %agent_id, %intent_id, reason = %reason,
                        "order blocked by domain allocator"
                    );
                    return;
                }

                self.persist_risk_decision(&intent, "PASSED", None, None)
                    .await;
                let mut queue = self.order_queue.write().await;
                match queue.enqueue(intent) {
                    Ok(()) => {
                        debug!(
                            %agent_id, %intent_id,
                            "order enqueued"
                        );
                    }
                    Err(e) => {
                        self.release_domain_reservation(intent_id).await;
                        warn!(%agent_id, %intent_id, error = %e, "queue full, order dropped");
                    }
                }
            }
            RiskCheckResult::Blocked(reason) => {
                self.persist_risk_decision(&intent, "BLOCKED", Some(reason.to_string()), None)
                    .await;
                warn!(
                    %agent_id, %intent_id,
                    reason = ?reason,
                    "order blocked by risk gate"
                );
            }
            RiskCheckResult::Adjusted(suggestion) => {
                self.persist_risk_decision(
                    &intent,
                    "ADJUSTED",
                    None,
                    Some((suggestion.max_shares, suggestion.reason.clone())),
                )
                .await;
                info!(
                    %agent_id, %intent_id,
                    max_shares = suggestion.max_shares,
                    reason = %suggestion.reason,
                    "order adjusted by risk gate — dropping (agent should resubmit)"
                );
            }
        }
    }

    async fn check_duplicate_intent(&self, intent: &OrderIntent) -> Option<String> {
        let mut guard = self.duplicate_guard.write().await;
        guard.register_or_block(intent, Utc::now())
    }

    async fn reserve_domain_capital(&self, intent: &OrderIntent) -> Option<String> {
        if !intent.is_buy {
            return None;
        }
        match intent.domain {
            Domain::Crypto => {
                let mut allocator = self.crypto_allocator.write().await;
                allocator.reserve_buy(intent).err()
            }
            Domain::Sports => {
                let mut allocator = self.sports_allocator.write().await;
                allocator.reserve_buy(intent).err()
            }
            _ => None,
        }
    }

    async fn release_domain_reservation(&self, intent_id: Uuid) {
        {
            let mut allocator = self.crypto_allocator.write().await;
            allocator.release_buy_reservation(intent_id);
        }
        let mut sports_allocator = self.sports_allocator.write().await;
        sports_allocator.release_buy_reservation(intent_id);
    }

    async fn settle_domain_success(
        &self,
        intent: &OrderIntent,
        filled_shares: u64,
        fill_price: Decimal,
    ) {
        match intent.domain {
            Domain::Crypto => {
                let mut allocator = self.crypto_allocator.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            Domain::Sports => {
                let mut allocator = self.sports_allocator.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            _ => {}
        }
    }

    async fn settle_domain_failure(&self, intent: &OrderIntent) {
        if !intent.is_buy {
            return;
        }
        match intent.domain {
            Domain::Crypto => {
                let mut allocator = self.crypto_allocator.write().await;
                allocator.release_buy_reservation(intent.intent_id);
            }
            Domain::Sports => {
                let mut allocator = self.sports_allocator.write().await;
                allocator.release_buy_reservation(intent.intent_id);
            }
            _ => {}
        }
    }

    /// Update agent snapshot in global state
    async fn handle_state_update(&self, snapshot: AgentSnapshot) {
        let agent_id = snapshot.agent_id.clone();

        // Update risk gate with latest exposure data
        self.risk_gate
            .update_agent_exposure(
                &agent_id,
                snapshot.exposure,
                snapshot.unrealized_pnl,
                snapshot.position_count,
                0, // unhedged count not tracked in snapshot
            )
            .await;

        // Store snapshot
        let mut state = self.global_state.write().await;
        state.agents.insert(agent_id, snapshot);
    }

    /// Drain the order queue and execute via OrderExecutor
    async fn drain_and_execute(&self) {
        let batch = {
            let mut queue = self.order_queue.write().await;
            queue.cleanup_expired();
            queue.dequeue_batch(self.config.batch_size)
        };

        if batch.is_empty() {
            return;
        }

        debug!(count = batch.len(), "draining order queue");

        for intent in batch {
            let agent_id = intent.agent_id.clone();
            let intent_id = intent.intent_id;
            let execute_started_at = Utc::now();
            let queue_delay_ms = execute_started_at
                .signed_duration_since(intent.created_at)
                .num_milliseconds()
                .max(0);

            // Convert OrderIntent → OrderRequest for the executor
            let request = self.intent_to_request(&intent);

            match self.executor.execute(&request).await {
                Ok(result) => {
                    info!(
                        %agent_id, %intent_id,
                        order_id = %result.order_id,
                        filled = result.filled_shares,
                        "order executed successfully"
                    );

                    self.persist_execution(
                        &intent,
                        &request,
                        Some(&result),
                        None,
                        Some(queue_delay_ms),
                    )
                    .await;

                    // Record success with risk gate
                    self.risk_gate
                        .record_success(&agent_id, Decimal::ZERO)
                        .await;

                    let fill_price = result.avg_fill_price.unwrap_or(intent.limit_price);
                    self.settle_domain_success(&intent, result.filled_shares, fill_price)
                        .await;

                    if result.filled_shares > 0 {
                        if intent.is_buy {
                            let _ = self
                                .positions
                                .open_position(
                                    &agent_id,
                                    intent.domain.clone(),
                                    &intent.market_slug,
                                    &intent.token_id,
                                    intent.side.clone(),
                                    result.filled_shares,
                                    fill_price,
                                )
                                .await;
                        } else {
                            self.apply_sell_fill_to_positions(
                                &intent,
                                result.filled_shares,
                                fill_price,
                            )
                            .await;
                        }
                    }
                }
                Err(e) => {
                    error!(
                        %agent_id, %intent_id,
                        error = %e,
                        "order execution failed"
                    );

                    self.persist_execution(
                        &intent,
                        &request,
                        None,
                        Some(e.to_string()),
                        Some(queue_delay_ms),
                    )
                    .await;

                    self.risk_gate
                        .record_failure(&agent_id, &e.to_string())
                        .await;

                    self.settle_domain_failure(&intent).await;
                }
            }
        }
    }

    async fn apply_sell_fill_to_positions(
        &self,
        intent: &OrderIntent,
        filled_shares: u64,
        exit_price: Decimal,
    ) {
        if filled_shares == 0 {
            return;
        }

        let mut remaining = filled_shares;
        let mut matching_positions = self
            .positions
            .get_agent_positions(&intent.agent_id)
            .await
            .into_iter()
            .filter(|pos| {
                pos.domain == intent.domain
                    && pos.market_slug == intent.market_slug
                    && pos.token_id == intent.token_id
                    && pos.side == intent.side
            })
            .collect::<Vec<_>>();

        matching_positions.sort_by_key(|p| p.entry_time);

        for pos in matching_positions {
            if remaining == 0 {
                break;
            }
            let reduce_by = remaining.min(pos.shares);
            let _ = self
                .positions
                .reduce_position(&pos.position_id, reduce_by, exit_price)
                .await;
            remaining -= reduce_by;
        }

        if remaining > 0 {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                unmatched_shares = remaining,
                "sell fill exceeded tracked position shares; allocator adjusted, position book partially unmatched"
            );
        }
    }

    async fn persist_execution(
        &self,
        intent: &OrderIntent,
        request: &OrderRequest,
        result: Option<&crate::strategy::executor::ExecutionResult>,
        error_message: Option<String>,
        queue_delay_ms: Option<i64>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let dry_run = self.executor.is_dry_run();

        let (order_id, status, filled_shares, avg_fill_price, elapsed_ms) = match result {
            Some(r) => (
                Some(r.order_id.clone()),
                format!("{:?}", r.status),
                r.filled_shares as i64,
                r.avg_fill_price,
                Some(r.elapsed_ms as i64),
            ),
            None => (
                None,
                format!("{:?}", crate::domain::OrderStatus::Failed),
                0,
                None,
                None,
            ),
        };

        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));
        let config_hash = intent.metadata.get("config_hash").cloned();

        let query = sqlx::query(
            r#"
            INSERT INTO agent_order_executions (
                account_id,
                agent_id,
                intent_id,
                domain,
                market_slug,
                token_id,
                market_side,
                is_buy,
                shares,
                limit_price,
                order_id,
                status,
                filled_shares,
                avg_fill_price,
                elapsed_ms,
                dry_run,
                error,
                intent_created_at,
                metadata
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19
            )
            ON CONFLICT (intent_id) DO UPDATE SET
                order_id = EXCLUDED.order_id,
                status = EXCLUDED.status,
                filled_shares = EXCLUDED.filled_shares,
                avg_fill_price = EXCLUDED.avg_fill_price,
                elapsed_ms = EXCLUDED.elapsed_ms,
                dry_run = EXCLUDED.dry_run,
                error = EXCLUDED.error,
                metadata = EXCLUDED.metadata,
                executed_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(&intent.agent_id)
        .bind(intent.intent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.side.as_str())
        .bind(intent.is_buy)
        .bind(intent.shares as i64)
        .bind(request.limit_price)
        .bind(order_id)
        .bind(status)
        .bind(filled_shares)
        .bind(avg_fill_price)
        .bind(elapsed_ms)
        .bind(dry_run)
        .bind(error_message.clone())
        .bind(intent.created_at)
        .bind(sqlx::types::Json(metadata));

        if let Err(e) = query.execute(pool).await {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist agent order execution"
            );
        }

        self.persist_execution_analysis(intent, request, result, queue_delay_ms, config_hash)
            .await;

        if !intent.is_buy {
            self.persist_exit_reason_execution(intent, result, error_message)
                .await;
        }
    }

    fn metadata_decimal(intent: &OrderIntent, key: &str) -> Option<Decimal> {
        intent
            .metadata
            .get(key)
            .and_then(|v| Decimal::from_str(v).ok())
    }

    async fn persist_signal_from_intent(&self, intent: &OrderIntent) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let strategy_id = intent
            .metadata
            .get("strategy")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let signal_type = intent
            .metadata
            .get("signal_type")
            .cloned()
            .unwrap_or_else(|| {
                if intent.is_buy {
                    "entry_intent".to_string()
                } else {
                    "exit_intent".to_string()
                }
            });
        let symbol = intent.metadata.get("symbol").cloned();
        let confidence = Self::metadata_decimal(intent, "signal_confidence");
        let momentum_value = Self::metadata_decimal(intent, "signal_momentum_value");
        let short_ma = Self::metadata_decimal(intent, "signal_short_ma");
        let long_ma = Self::metadata_decimal(intent, "signal_long_ma");
        let rolling_volatility = Self::metadata_decimal(intent, "signal_rolling_volatility");
        let fair_value = Self::metadata_decimal(intent, "signal_fair_value");
        let market_price = Self::metadata_decimal(intent, "signal_market_price");
        let edge = Self::metadata_decimal(intent, "signal_edge");
        let config_hash = intent.metadata.get("config_hash").cloned();
        let context =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO signal_history (
                account_id, intent_id, agent_id, strategy_id, domain, signal_type, market_slug, token_id,
                symbol, side, confidence, momentum_value, short_ma, long_ma, rolling_volatility,
                fair_value, market_price, edge, config_hash, context
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,
                $9,$10,$11,$12,$13,$14,$15,
                $16,$17,$18,$19,$20
            )
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(&strategy_id)
        .bind(intent.domain.to_string())
        .bind(&signal_type)
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(symbol)
        .bind(intent.side.as_str())
        .bind(confidence)
        .bind(momentum_value)
        .bind(short_ma)
        .bind(long_ma)
        .bind(rolling_volatility)
        .bind(fair_value)
        .bind(market_price)
        .bind(edge)
        .bind(config_hash)
        .bind(sqlx::types::Json(context))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist signal history"
            );
        }
    }

    async fn persist_risk_decision(
        &self,
        intent: &OrderIntent,
        decision: &str,
        block_reason: Option<String>,
        adjusted: Option<(u64, String)>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let (suggestion_max_shares, suggestion_reason) = adjusted
            .map(|(shares, reason)| (Some(shares as i64), Some(reason)))
            .unwrap_or((None, None));
        let config_hash = intent.metadata.get("config_hash").cloned();
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO risk_gate_decisions (
                account_id, intent_id, agent_id, domain, decision, block_reason, suggestion_max_shares,
                suggestion_reason, notional_value, config_hash, metadata
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
            ON CONFLICT (intent_id) DO UPDATE SET
                decision = EXCLUDED.decision,
                block_reason = EXCLUDED.block_reason,
                suggestion_max_shares = EXCLUDED.suggestion_max_shares,
                suggestion_reason = EXCLUDED.suggestion_reason,
                notional_value = EXCLUDED.notional_value,
                config_hash = EXCLUDED.config_hash,
                metadata = EXCLUDED.metadata,
                decided_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(decision)
        .bind(block_reason)
        .bind(suggestion_max_shares)
        .bind(suggestion_reason)
        .bind(intent.notional_value())
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist risk gate decision"
            );
        }
    }

    async fn persist_exit_reason_intent(&self, intent: &OrderIntent) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let reason_code = intent
            .metadata
            .get("exit_reason")
            .or_else(|| intent.metadata.get("reason_code"))
            .cloned()
            .unwrap_or_else(|| "UNKNOWN".to_string());
        let reason_detail = intent.metadata.get("exit_detail").cloned();
        let entry_price = Self::metadata_decimal(intent, "entry_price");
        let pnl_pct = Self::metadata_decimal(intent, "pnl_pct");
        let config_hash = intent.metadata.get("config_hash").cloned();
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO exit_reasons (
                account_id, intent_id, agent_id, domain, market_slug, token_id, market_side, reason_code,
                reason_detail, entry_price, pnl_pct, status, config_hash, metadata
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'INTENT_SUBMITTED',$12,$13)
            ON CONFLICT (intent_id) DO UPDATE SET
                reason_code = EXCLUDED.reason_code,
                reason_detail = EXCLUDED.reason_detail,
                entry_price = COALESCE(EXCLUDED.entry_price, exit_reasons.entry_price),
                pnl_pct = COALESCE(EXCLUDED.pnl_pct, exit_reasons.pnl_pct),
                status = EXCLUDED.status,
                config_hash = EXCLUDED.config_hash,
                metadata = EXCLUDED.metadata,
                updated_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.side.as_str())
        .bind(reason_code)
        .bind(reason_detail)
        .bind(entry_price)
        .bind(pnl_pct)
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist exit reason intent"
            );
        }
    }

    async fn persist_exit_reason_execution(
        &self,
        intent: &OrderIntent,
        result: Option<&crate::strategy::executor::ExecutionResult>,
        error_message: Option<String>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let executed_price = result.and_then(|r| r.avg_fill_price);
        let status = result
            .map(|r| format!("{:?}", r.status))
            .unwrap_or_else(|| "Failed".to_string());
        let reason_detail = error_message.or_else(|| {
            intent
                .metadata
                .get("exit_detail")
                .cloned()
                .or_else(|| intent.metadata.get("error").cloned())
        });
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO exit_reasons (
                account_id, intent_id, agent_id, domain, market_slug, token_id, market_side, reason_code,
                reason_detail, entry_price, exit_price, pnl_pct, status, config_hash, metadata
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,
                $9,$10,$11,$12,$13,$14,$15
            )
            ON CONFLICT (intent_id) DO UPDATE SET
                reason_detail = COALESCE(EXCLUDED.reason_detail, exit_reasons.reason_detail),
                exit_price = COALESCE(EXCLUDED.exit_price, exit_reasons.exit_price),
                pnl_pct = COALESCE(EXCLUDED.pnl_pct, exit_reasons.pnl_pct),
                status = EXCLUDED.status,
                config_hash = COALESCE(EXCLUDED.config_hash, exit_reasons.config_hash),
                metadata = EXCLUDED.metadata,
                updated_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.side.as_str())
        .bind(
            intent
                .metadata
                .get("exit_reason")
                .or_else(|| intent.metadata.get("reason_code"))
                .cloned()
                .unwrap_or_else(|| "UNKNOWN".to_string()),
        )
        .bind(reason_detail)
        .bind(Self::metadata_decimal(intent, "entry_price"))
        .bind(executed_price)
        .bind(Self::metadata_decimal(intent, "pnl_pct"))
        .bind(status)
        .bind(intent.metadata.get("config_hash").cloned())
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist exit reason execution"
            );
        }
    }

    async fn persist_execution_analysis(
        &self,
        intent: &OrderIntent,
        request: &OrderRequest,
        result: Option<&crate::strategy::executor::ExecutionResult>,
        queue_delay_ms: Option<i64>,
        config_hash: Option<String>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let expected_price = request.limit_price;
        let executed_price = result.and_then(|r| r.avg_fill_price);
        let execution_latency_ms = result.map(|r| r.elapsed_ms as i64);
        let total_latency_ms = match (queue_delay_ms, execution_latency_ms) {
            (Some(q), Some(e)) => Some(q + e),
            (Some(q), None) => Some(q),
            (None, Some(e)) => Some(e),
            (None, None) => None,
        };

        let actual_slippage_bps = executed_price.and_then(|fill| {
            if expected_price.is_zero() {
                return None;
            }
            let signed = if intent.is_buy {
                (fill - expected_price) / expected_price
            } else {
                (expected_price - fill) / expected_price
            };
            Some(signed * Decimal::from(10_000))
        });

        let expected_slippage_bps = Self::metadata_decimal(intent, "expected_slippage_bps")
            .or_else(|| Self::metadata_decimal(intent, "signal_expected_slippage_bps"));
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));
        let status = result
            .map(|r| format!("{:?}", r.status))
            .unwrap_or_else(|| "Failed".to_string());

        let result = sqlx::query(
            r#"
            INSERT INTO execution_analysis (
                account_id, intent_id, agent_id, domain, market_slug, token_id, is_buy,
                expected_price, executed_price, expected_slippage_bps, actual_slippage_bps,
                queue_delay_ms, execution_latency_ms, total_latency_ms,
                status, dry_run, config_hash, metadata
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,
                $8,$9,$10,$11,
                $12,$13,$14,
                $15,$16,$17,$18
            )
            ON CONFLICT (intent_id) DO UPDATE SET
                executed_price = EXCLUDED.executed_price,
                expected_slippage_bps = EXCLUDED.expected_slippage_bps,
                actual_slippage_bps = EXCLUDED.actual_slippage_bps,
                queue_delay_ms = EXCLUDED.queue_delay_ms,
                execution_latency_ms = EXCLUDED.execution_latency_ms,
                total_latency_ms = EXCLUDED.total_latency_ms,
                status = EXCLUDED.status,
                dry_run = EXCLUDED.dry_run,
                config_hash = EXCLUDED.config_hash,
                metadata = EXCLUDED.metadata,
                recorded_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.is_buy)
        .bind(expected_price)
        .bind(executed_price)
        .bind(expected_slippage_bps)
        .bind(actual_slippage_bps)
        .bind(queue_delay_ms)
        .bind(execution_latency_ms)
        .bind(total_latency_ms)
        .bind(status)
        .bind(self.executor.is_dry_run())
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist execution analysis"
            );
        }
    }

    /// Refresh GlobalState from aggregators
    async fn refresh_global_state(&self) {
        let portfolio = self.positions.aggregate().await;
        let positions = self.positions.all_positions().await;
        let risk_state = self.risk_gate.state().await;
        let (daily_pnl, _, _) = self.risk_gate.daily_stats().await;
        let daily_loss_limit = self.risk_gate.daily_loss_limit();
        let circuit_breaker_events = self.risk_gate.circuit_breaker_events().await;
        let queue_stats = self.order_queue.read().await.stats();
        let total_realized = self.positions.total_realized_pnl().await;

        let mut state = self.global_state.write().await;
        state.portfolio = portfolio;
        state.positions = positions;
        state.risk_state = risk_state;
        state.daily_pnl = daily_pnl;
        state.daily_loss_limit = daily_loss_limit;
        state.circuit_breaker_events = circuit_breaker_events;
        state.queue_stats = QueueStatsSnapshot::from(queue_stats);
        state.total_realized_pnl = total_realized;
        state.last_refresh = Utc::now();

        // Check for stale agents
        let timeout = chrono::Duration::milliseconds(self.config.heartbeat_timeout_ms as i64);
        let stale_warn_cooldown =
            chrono::Duration::seconds(self.config.heartbeat_stale_warn_cooldown_secs as i64);
        let now = Utc::now();
        let mut stale_warn_at = self.stale_heartbeat_warn_at.write().await;
        for (id, agent) in state.agents.iter_mut() {
            if now - agent.last_heartbeat > timeout
                && matches!(agent.status, crate::platform::AgentStatus::Running)
            {
                let should_warn = stale_warn_at
                    .get(id)
                    .map(|last_warned_at| now - *last_warned_at >= stale_warn_cooldown)
                    .unwrap_or(true);
                if should_warn {
                    warn!(
                        agent_id = %id,
                        last_heartbeat = %agent.last_heartbeat,
                        stale_ms = (now - agent.last_heartbeat).num_milliseconds(),
                        timeout_ms = self.config.heartbeat_timeout_ms,
                        "agent heartbeat stale"
                    );
                    stale_warn_at.insert(id.clone(), now);
                }
                agent.error_message = Some("heartbeat timeout".into());
            }
        }
    }

    fn infer_time_bucket_seconds(intent: &OrderIntent) -> i64 {
        if let Some(raw) = intent.metadata.get("event_window_secs") {
            if let Ok(v) = raw.trim().parse::<i64>() {
                if v > 0 {
                    return v;
                }
            }
        }

        let mut hints: Vec<&str> = Vec::new();
        if let Some(h) = intent.metadata.get("timeframe") {
            hints.push(h.as_str());
        }
        if let Some(h) = intent.metadata.get("horizon") {
            hints.push(h.as_str());
        }
        if let Some(h) = intent.metadata.get("series_id") {
            hints.push(h.as_str());
        }

        for raw in hints {
            if let Some(horizon) = CryptoHorizon::from_hint(raw) {
                return match horizon {
                    CryptoHorizon::M15 => 15 * 60,
                    CryptoHorizon::M5 => 5 * 60,
                    CryptoHorizon::Other => 5 * 60,
                };
            }
        }

        5 * 60
    }

    fn sanitize_idempotency_component(input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        for ch in input.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':' | '.' | '|') {
                out.push(ch);
            } else {
                out.push('_');
            }
        }
        out
    }

    fn stable_idempotency_key(account_id: &str, intent: &OrderIntent) -> String {
        if let Some(key) = intent
            .metadata
            .get("idempotency_key")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
        {
            return Self::sanitize_idempotency_component(key);
        }

        let deployment_id = intent
            .metadata
            .get("deployment_id")
            .map(String::as_str)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase())
            .unwrap_or_else(|| {
                let strategy = intent
                    .metadata
                    .get("strategy")
                    .map(|v| v.trim().to_ascii_lowercase())
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| "default".to_string());
                format!(
                    "agent:{}|strategy:{}",
                    intent.agent_id.trim().to_ascii_lowercase(),
                    strategy
                )
            });

        let window_secs = Self::infer_time_bucket_seconds(intent);
        let ts = intent
            .metadata
            .get("event_time")
            .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(intent.created_at)
            .timestamp();
        let bucket = ts.div_euclid(window_secs);
        let side = intent.side.as_str();
        let order_kind = if intent.is_buy { "buy" } else { "sell" };

        Self::sanitize_idempotency_component(&format!(
            "acct:{account}|dep:{dep}|dom:{dom}|mkt:{mkt}|side:{side}|kind:{kind}|bucket:{bucket}",
            account = account_id,
            dep = deployment_id,
            dom = intent.domain.to_string().to_ascii_lowercase(),
            mkt = intent.market_slug.trim().to_ascii_lowercase(),
            side = side.to_ascii_lowercase(),
            kind = order_kind,
            bucket = bucket,
        ))
    }

    /// Convert an OrderIntent into an OrderRequest for the executor
    fn intent_to_request(&self, intent: &OrderIntent) -> OrderRequest {
        use crate::domain::OrderSide;

        let order_side = if intent.is_buy {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };

        let idempotency_key = Self::stable_idempotency_key(&self.account_id, intent);
        OrderRequest {
            client_order_id: format!("intent:{}", intent.intent_id),
            idempotency_key: Some(idempotency_key),
            token_id: intent.token_id.clone(),
            market_side: intent.side.clone(),
            order_side,
            shares: intent.shares,
            limit_price: intent.limit_price,
            order_type: crate::domain::OrderType::Limit,
            time_in_force: crate::domain::TimeInForce::GTC,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{AgentStatus, Domain, OrderPriority, QueueStats};
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    fn mock_snapshot(agent_id: &str) -> AgentSnapshot {
        AgentSnapshot {
            agent_id: agent_id.into(),
            name: agent_id.into(),
            domain: Domain::Crypto,
            status: AgentStatus::Running,
            position_count: 1,
            exposure: dec!(100),
            daily_pnl: dec!(5),
            unrealized_pnl: dec!(2),
            metrics: HashMap::new(),
            last_heartbeat: Utc::now(),
            error_message: None,
        }
    }

    #[test]
    fn test_global_state_defaults() {
        let state = GlobalState::new();
        assert_eq!(state.active_agent_count(), 0);
        assert_eq!(state.total_exposure(), Decimal::ZERO);
        assert_eq!(state.total_unrealized_pnl(), Decimal::ZERO);
    }

    #[test]
    fn test_global_state_active_count() {
        let mut state = GlobalState::new();
        state.agents.insert("a".into(), mock_snapshot("a"));
        state.agents.insert("b".into(), {
            let mut s = mock_snapshot("b");
            s.status = AgentStatus::Paused;
            s
        });
        assert_eq!(state.active_agent_count(), 1);
    }

    #[test]
    fn test_queue_stats_snapshot_from() {
        let qs = QueueStats {
            current_size: 5,
            max_size: 100,
            enqueued_total: 50,
            dequeued_total: 45,
            expired_total: 3,
            critical_count: 1,
            high_count: 2,
            normal_count: 1,
            low_count: 1,
        };
        let snap = QueueStatsSnapshot::from(qs);
        assert_eq!(snap.current_size, 5);
        assert_eq!(snap.enqueued_total, 50);
    }

    fn make_intent(is_buy: bool, priority: OrderPriority) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "crypto_lob_ml",
            Domain::Crypto,
            "btc-updown-5m-123",
            "token-up-123",
            crate::domain::Side::Up,
            is_buy,
            100,
            dec!(0.42),
        );
        intent.priority = priority;
        intent
    }

    #[test]
    fn test_duplicate_guard_blocks_repeated_buy_within_window() {
        let mut guard = IntentDuplicateGuard::new(1000, true);
        let now = Utc::now();
        let intent = make_intent(true, OrderPriority::Normal);

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(300))
            .is_some());
    }

    #[test]
    fn test_duplicate_guard_allows_after_window() {
        let mut guard = IntentDuplicateGuard::new(500, true);
        let now = Utc::now();
        let intent = make_intent(true, OrderPriority::Normal);

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(700))
            .is_none());
    }

    #[test]
    fn test_duplicate_guard_blocks_same_market_even_if_token_differs() {
        let mut guard = IntentDuplicateGuard::new(1_000, true);
        let now = Utc::now();
        let first = make_intent(true, OrderPriority::Normal);
        let mut second = make_intent(true, OrderPriority::Normal);
        second.token_id = "token-down-123".to_string();
        second.side = crate::domain::Side::Down;

        assert!(guard.register_or_block(&first, now).is_none());
        assert!(guard
            .register_or_block(&second, now + chrono::Duration::milliseconds(100))
            .is_some());
    }

    #[test]
    fn test_duplicate_guard_allows_same_market_for_different_deployments() {
        let mut guard = IntentDuplicateGuard::new(1_000, true);
        let now = Utc::now();
        let mut first = make_intent(true, OrderPriority::Normal);
        let mut second = make_intent(true, OrderPriority::Normal);

        first.metadata.insert(
            "deployment_id".to_string(),
            "crypto.pm.btc.15m.momentum".to_string(),
        );
        second.metadata.insert(
            "deployment_id".to_string(),
            "crypto.pm.btc.15m.patternmem".to_string(),
        );

        assert!(guard.register_or_block(&first, now).is_none());
        assert!(guard
            .register_or_block(&second, now + chrono::Duration::milliseconds(100))
            .is_none());
    }

    #[test]
    fn test_duplicate_guard_does_not_block_sells() {
        let mut guard = IntentDuplicateGuard::new(10_000, true);
        let now = Utc::now();
        let intent = make_intent(false, OrderPriority::Normal);

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(10))
            .is_none());
    }

    #[test]
    fn test_duplicate_guard_skips_critical_orders() {
        let mut guard = IntentDuplicateGuard::new(10_000, true);
        let now = Utc::now();
        let intent = make_intent(true, OrderPriority::Critical);

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(10))
            .is_none());
    }

    #[test]
    fn test_intent_to_request_uses_stable_idempotency_key_by_window() {
        let mut intent = OrderIntent::new(
            "openclaw",
            Domain::Crypto,
            "btc-updown-15m-20260219-1200",
            "token-up-1",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.45),
        );
        intent.metadata.insert(
            "deployment_id".to_string(),
            "crypto.pm.btc.15m.patternmem".to_string(),
        );
        intent
            .metadata
            .insert("horizon".to_string(), "15m".to_string());
        intent
            .metadata
            .insert("event_time".to_string(), "2026-02-19T12:07:00Z".to_string());

        let key = Coordinator::stable_idempotency_key("acct-main", &intent);

        assert_ne!(key, intent.intent_id.to_string());
        assert!(key.contains("acct-main"));
        assert!(key.contains("crypto.pm.btc.15m.patternmem"));
        assert!(key.contains("btc-updown-15m-20260219-1200"));
    }

    #[test]
    fn test_stable_idempotency_key_fallback_uses_intent_created_at() {
        let mut first = OrderIntent::new(
            "openclaw",
            Domain::Crypto,
            "btc-updown-15m",
            "token-up-1",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.45),
        );
        let mut second = first.clone();
        first.created_at = chrono::DateTime::parse_from_rfc3339("2026-02-19T12:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);
        second.created_at = chrono::DateTime::parse_from_rfc3339("2026-02-19T13:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);

        let first_key = Coordinator::stable_idempotency_key("acct-main", &first);
        let second_key = Coordinator::stable_idempotency_key("acct-main", &second);

        assert_ne!(first_key, second_key);
    }

    fn make_allocator_config(total_cap: Decimal) -> CoordinatorConfig {
        let mut cfg = CoordinatorConfig::default();
        cfg.crypto_allocator_enabled = true;
        cfg.crypto_allocator_total_cap_usd = Some(total_cap);
        cfg.crypto_coin_cap_btc_pct = dec!(0.40);
        cfg.crypto_coin_cap_eth_pct = dec!(0.40);
        cfg.crypto_coin_cap_sol_pct = dec!(0.30);
        cfg.crypto_coin_cap_xrp_pct = dec!(0.20);
        cfg.crypto_coin_cap_other_pct = dec!(0.10);
        cfg.crypto_horizon_cap_5m_pct = dec!(0.50);
        cfg.crypto_horizon_cap_15m_pct = dec!(0.60);
        cfg.crypto_horizon_cap_other_pct = dec!(0.25);
        cfg
    }

    fn make_crypto_intent(
        coin: &str,
        horizon: &str,
        is_buy: bool,
        shares: u64,
        limit_price: Decimal,
    ) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "crypto",
            Domain::Crypto,
            "btc-up-or-down",
            "token-up-123",
            crate::domain::Side::Up,
            is_buy,
            shares,
            limit_price,
        );
        intent.metadata.insert("coin".to_string(), coin.to_string());
        intent
            .metadata
            .insert("horizon".to_string(), horizon.to_string());
        if !is_buy {
            intent
                .metadata
                .insert("entry_price".to_string(), limit_price.to_string());
        }
        intent
    }

    #[test]
    fn test_crypto_allocator_blocks_buy_when_coin_cap_exceeded() {
        let cfg = make_allocator_config(dec!(100));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);

        let first = make_crypto_intent("BTC", "5m", true, 60, dec!(0.5)); // $30
        let second = make_crypto_intent("BTC", "5m", true, 30, dec!(0.5)); // $15 -> total $45 > BTC cap $40

        assert!(allocator.reserve_buy(&first).is_ok());
        assert!(allocator.reserve_buy(&second).is_err());
    }

    #[test]
    fn test_crypto_allocator_clamps_total_cap_to_risk_domain_cap() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.crypto_allocator_total_cap_usd = Some(dec!(100));
        cfg.risk.crypto_max_exposure = Some(dec!(60));

        let allocator = CryptoCapitalAllocator::new(&cfg);
        assert_eq!(allocator.total_cap, dec!(60));
    }

    #[test]
    fn test_crypto_allocator_releases_pending_on_buy_failure() {
        let cfg = make_allocator_config(dec!(100));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);
        let intent = make_crypto_intent("BTC", "5m", true, 50, dec!(0.5)); // $25

        assert!(allocator.reserve_buy(&intent).is_ok());
        assert!(allocator.pending.total > Decimal::ZERO);

        allocator.release_buy_reservation(intent.intent_id);

        assert_eq!(allocator.pending.total, Decimal::ZERO);
        assert!(allocator.pending_by_intent.is_empty());
    }

    #[test]
    fn test_crypto_allocator_settles_buy_then_sell() {
        let cfg = make_allocator_config(dec!(200));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);
        let buy = make_crypto_intent("BTC", "15m", true, 100, dec!(0.5)); // reserve $50

        assert!(allocator.reserve_buy(&buy).is_ok());
        allocator.settle_buy_execution(&buy, 80, dec!(0.5)); // open $40

        assert_eq!(allocator.pending.total, Decimal::ZERO);
        assert_eq!(allocator.open.total, dec!(40));

        let mut sell = make_crypto_intent("BTC", "15m", false, 40, dec!(0.5));
        sell.market_slug = buy.market_slug.clone();
        sell.token_id = buy.token_id.clone();
        sell.side = buy.side;
        allocator.settle_sell_execution(&sell, 40, dec!(0.55)); // release by entry price metadata ($20)

        assert_eq!(allocator.open.total, dec!(20));
    }

    fn make_sports_intent(
        market_slug: &str,
        is_buy: bool,
        shares: u64,
        limit_price: Decimal,
    ) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "sports",
            Domain::Sports,
            market_slug,
            "sports-token-yes",
            crate::domain::Side::Up,
            is_buy,
            shares,
            limit_price,
        );
        if !is_buy {
            intent
                .metadata
                .insert("entry_price".to_string(), limit_price.to_string());
        }
        intent
    }

    #[test]
    fn test_sports_allocator_auto_splits_by_active_markets() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(30));
        cfg.sports_market_cap_pct = dec!(0.70);
        cfg.sports_auto_split_by_active_markets = true;

        let mut allocator = SportsCapitalAllocator::new(&cfg);

        let game1_buy = make_sports_intent("nba-game-1", true, 100, dec!(0.15)); // $15
        let game2_buy = make_sports_intent("nba-game-2", true, 100, dec!(0.15)); // $15
        let game1_extra = make_sports_intent("nba-game-1", true, 10, dec!(0.10)); // $1

        assert!(allocator.reserve_buy(&game1_buy).is_ok());
        assert!(allocator.reserve_buy(&game2_buy).is_ok());
        assert!(allocator.reserve_buy(&game1_extra).is_err());
    }

    #[test]
    fn test_sports_allocator_releases_pending_on_buy_failure() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(30));

        let mut allocator = SportsCapitalAllocator::new(&cfg);
        let intent = make_sports_intent("nba-game-1", true, 100, dec!(0.10)); // $10

        assert!(allocator.reserve_buy(&intent).is_ok());
        assert!(allocator.pending.total > Decimal::ZERO);

        allocator.release_buy_reservation(intent.intent_id);

        assert_eq!(allocator.pending.total, Decimal::ZERO);
        assert!(allocator.pending_by_intent.is_empty());
    }

    #[test]
    fn test_sports_allocator_clamps_total_cap_to_risk_domain_cap() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(50));
        cfg.risk.sports_max_exposure = Some(dec!(25));

        let allocator = SportsCapitalAllocator::new(&cfg);
        assert_eq!(allocator.total_cap, dec!(25));
    }
}
