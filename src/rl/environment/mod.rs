//! Simulated Trading Environment for RL Training
//!
//! This module provides a gym-like environment for training RL agents
//! on simulated market data without risking real capital.

mod backtest;
mod leadlag;
mod market;
mod trading;

pub use backtest::{
    generate_sample_data, BacktestEnvironment, BacktestInfo, BacktestStepResult, HistoricalData,
    TickData,
};
pub use leadlag::{
    LeadLagAction, LeadLagConfig, LeadLagEnvironment, LeadLagInfo, LeadLagStepResult, LobDataPoint,
    LobObservation,
};
pub use market::{MarketConfig, MarketState, SimulatedMarket};
pub use trading::{EnvAction, StepResult, TradingEnvConfig, TradingEnvironment};
