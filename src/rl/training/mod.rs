//! Training Infrastructure
//!
//! Training loops, checkpointing, and evaluation utilities.

pub mod checkpointing;
pub mod trainer;

pub use checkpointing::Checkpointer;
pub use trainer::{
    run_backtest, summarize_backtest_results, summarize_results, train_backtest, train_simulated,
    BacktestResult, BacktestSummary, EpisodeResult, TrainingLoop, TrainingStats, TrainingSummary,
};
