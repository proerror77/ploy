//! Simulated Trading Environment for RL Training
//!
//! This module provides a gym-like environment for training RL agents
//! on simulated market data without risking real capital.

mod market;
mod trading;
mod backtest;
mod leadlag;

pub use market::{MarketConfig, SimulatedMarket, MarketState};
pub use trading::{TradingEnvironment, TradingEnvConfig, StepResult, EnvAction};
pub use backtest::{
    BacktestEnvironment, BacktestStepResult, BacktestInfo,
    HistoricalData, TickData, generate_sample_data,
};
pub use leadlag::{
    LeadLagEnvironment, LeadLagConfig, LeadLagAction, LeadLagStepResult,
    LobObservation, LobDataPoint, LeadLagInfo,
};
