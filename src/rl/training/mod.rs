//! Training Infrastructure
//!
//! Training loops, checkpointing, and evaluation utilities.

pub mod trainer;
pub mod checkpointing;

pub use trainer::{
    TrainingLoop, TrainingStats, EpisodeResult, TrainingSummary,
    train_simulated, summarize_results,
    BacktestResult, BacktestSummary,
    run_backtest, train_backtest, summarize_backtest_results,
};
pub use checkpointing::Checkpointer;
