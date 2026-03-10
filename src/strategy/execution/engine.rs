use super::engine_store::EngineStore;
use crate::adapters::{QuoteCache, QuoteUpdate};
use crate::config::AppConfig;
use crate::domain::{Round, Side, StrategyState};
use crate::error::{PloyError, Result};
use crate::strategy::{
    OrderExecutor, RiskManager, SignalDetector, SlippageConfig, SlippageProtection,
    TradingCalculator,
};
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use tracing::{debug, error, info, warn};

mod hedge_flow;
mod leg1;
mod lifecycle;
mod round_flow;

use lifecycle::{
    abort_cycle as abort_cycle_impl,
    abort_cycle_and_halt_safely as abort_cycle_and_halt_safely_impl,
    abort_cycle_neutral as abort_cycle_neutral_impl,
    force_leg2_or_abort as force_leg2_or_abort_impl,
    persist_halt_if_needed as persist_halt_if_needed_impl,
    persist_strategy_state_best_effort as persist_strategy_state_best_effort_impl,
    transition_to_idle as transition_to_idle_impl,
};

/// Main strategy engine orchestrating all components
pub struct StrategyEngine {
    config: AppConfig,
    store: Box<dyn EngineStore>,
    executor: OrderExecutor,
    risk_manager: Arc<RiskManager>,
    signal_detector: Arc<RwLock<SignalDetector>>,
    quote_cache: QuoteCache,
    state: Arc<RwLock<EngineState>>,
    calculator: TradingCalculator,
    /// Slippage protection for order execution
    slippage: SlippageProtection,
    /// Mutex to prevent concurrent order submissions (separate from state lock)
    execution_mutex: Mutex<()>,
}

/// Internal engine state
#[derive(Debug, Clone)]
struct EngineState {
    /// Current strategy state
    strategy_state: StrategyState,
    /// Current round being traded
    current_round: Option<Round>,
    /// Current cycle
    current_cycle: Option<CycleContext>,
    /// Whether we should stop
    shutdown: bool,
    /// Version number for optimistic locking (prevents race conditions)
    version: u64,
}

/// Context for an active cycle
#[derive(Debug, Clone)]
struct CycleContext {
    cycle_id: i32,
    leg1_side: Side,
    leg1_price: Decimal,
    leg1_shares: u64,
    #[allow(dead_code)]
    leg1_order_id: String,
    leg2_order_id: Option<String>,
    /// Guard against duplicate forced Leg2 submissions from concurrent paths.
    force_leg2_attempted: bool,
    /// DB row version for optimistic locking (cycles.version column)
    cycle_version: i32,
}

/// `cycles.version` starts at 1 in the database migration.
const INITIAL_CYCLE_DB_VERSION: i32 = 1;

impl Default for EngineState {
    fn default() -> Self {
        Self {
            strategy_state: StrategyState::Idle,
            current_round: None,
            current_cycle: None,
            shutdown: false,
            version: 0,
        }
    }
}

impl StrategyEngine {
    /// Create a new strategy engine
    pub async fn new(
        config: AppConfig,
        store: impl EngineStore + 'static,
        executor: OrderExecutor,
        quote_cache: QuoteCache,
    ) -> Result<Self> {
        // Safety guard: if we can't confirm fills, the current engine would treat
        // submitted (but unconfirmed) orders as failures, risking stray live orders.
        if !config.dry_run.enabled && !config.execution.confirm_fills {
            return Err(PloyError::Validation(
                "execution.confirm_fills must be true when dry_run.enabled is false".to_string(),
            ));
        }

        let risk_manager = Arc::new(RiskManager::new(config.risk.clone()));
        let signal_detector = SignalDetector::new(config.strategy.clone());

        // Create calculator from config buffers
        let calculator = TradingCalculator::with_buffers(
            config.strategy.fee_buffer,
            config.strategy.slippage_buffer,
            config.strategy.profit_buffer,
        );

        // Create slippage protection from config
        let slippage = SlippageProtection::new(SlippageConfig {
            max_slippage_pct: config.strategy.slippage_buffer,
            ..SlippageConfig::default()
        });

        Ok(Self {
            config,
            store: Box::new(store),
            executor,
            risk_manager,
            signal_detector: Arc::new(RwLock::new(signal_detector)),
            quote_cache,
            state: Arc::new(RwLock::new(EngineState::default())),
            calculator,
            slippage,
            execution_mutex: Mutex::new(()),
        })
    }

    /// Get current state
    pub async fn state(&self) -> StrategyState {
        self.state.read().await.strategy_state
    }

    /// Signal shutdown
    pub async fn shutdown(&self) {
        info!("Shutdown requested");
        self.state.write().await.shutdown = true;
    }

    /// Main run loop
    pub async fn run(&self, mut updates: broadcast::Receiver<QuoteUpdate>) -> Result<()> {
        info!("Strategy engine starting");

        loop {
            // Check for shutdown
            if self.state.read().await.shutdown {
                info!("Shutting down strategy engine");
                break;
            }

            // Receive quote update with timeout
            match tokio::time::timeout(std::time::Duration::from_secs(1), updates.recv()).await {
                Ok(Ok(update)) => {
                    if let Err(e) = self.on_quote_update(update).await {
                        error!("Error processing quote update: {}", e);
                    }
                }
                Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                    warn!("Missed {} quote updates", n);
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    error!("Quote update channel closed");
                    break;
                }
                Err(_) => {
                    // Timeout - check round status
                    self.check_round_transition().await?;
                }
            }
        }

        Ok(())
    }

    /// Handle a quote update
    async fn on_quote_update(&self, update: QuoteUpdate) -> Result<()> {
        round_flow::on_quote_update(self, update).await
    }

    /// Check for round transitions
    async fn check_round_transition(&self) -> Result<()> {
        round_flow::check_round_transition(self).await
    }

    /// Set the current round
    pub async fn set_round(&self, round: Round) -> Result<()> {
        round_flow::set_round(self, round).await
    }

    /// Enter Leg1 position.
    async fn enter_leg1(&self, side: Side, price: Decimal) -> Result<()> {
        leg1::enter_leg1(self, side, price).await
    }

    async fn persist_halt_if_needed(&self) {
        persist_halt_if_needed_impl(self).await
    }

    async fn persist_strategy_state_best_effort(
        &self,
        state: StrategyState,
        round_id: Option<i32>,
        cycle_id: Option<i32>,
    ) {
        persist_strategy_state_best_effort_impl(self, state, round_id, cycle_id).await
    }

    /// Abort the current cycle and halt trading.
    ///
    /// If we're in `LEG1_FILLED` and no Leg2 has been started, this will attempt a best-effort
    /// unwind (SELL IOC) to reduce directional exposure before halting.
    async fn abort_cycle_and_halt_safely(&self, reason: &str) -> Result<()> {
        abort_cycle_and_halt_safely_impl(self, reason).await
    }

    /// Force Leg2 or abort when time is running out
    async fn force_leg2_or_abort(&self) -> Result<()> {
        force_leg2_or_abort_impl(self).await
    }

    /// Abort the current cycle
    async fn abort_cycle(&self, reason: &str) -> Result<()> {
        abort_cycle_impl(self, reason).await
    }

    /// Abort the current cycle without recording a risk failure.
    ///
    /// This is for expected/neutral aborts where no exposure exists (e.g. an IOC order got 0 fill).
    async fn abort_cycle_neutral(&self, reason: &str) -> Result<()> {
        abort_cycle_neutral_impl(self, reason).await
    }

    /// Transition back to idle state
    async fn transition_to_idle(&self) -> Result<()> {
        transition_to_idle_impl(self).await
    }

    /// Get risk manager for external queries
    pub fn risk_manager(&self) -> Arc<RiskManager> {
        Arc::clone(&self.risk_manager)
    }

    /// Check if dry run mode is enabled
    pub fn is_dry_run(&self) -> bool {
        self.executor.is_dry_run()
    }
}

#[cfg(test)]
mod tests;
