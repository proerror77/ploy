#[cfg(feature = "rl")]
use ploy::error::Result;
#[cfg(feature = "rl")]
use tracing::info;

#[cfg(feature = "rl")]
pub(super) async fn run_backtest(
    episodes: usize,
    duration: u64,
    volatility: f64,
    round: &Option<i32>,
    capital: f64,
    verbose: bool,
) -> Result<()> {
    use ploy::rl::algorithms::ppo::{PPOTrainer, PPOTrainerConfig};
    use ploy::rl::training::{summarize_backtest_results, train_backtest};
    use ploy::rl::{MarketConfig, PPOConfig, TradingEnvConfig};

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║               Ploy RL Backtest Mode                          ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!(
        "║  Episodes:       {:>10}                                    ║",
        episodes
    );
    println!(
        "║  Duration:       {:>10} mins                               ║",
        duration
    );
    println!(
        "║  Volatility:     {:>10.4}                                    ║",
        volatility
    );
    println!(
        "║  Initial Capital: {:>9.2}                                   ║",
        capital
    );
    if let Some(r) = round {
        println!(
            "║  Round ID:       {:>10}                                    ║",
            r
        );
    }
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let ppo_config = PPOTrainerConfig {
        ppo: PPOConfig::default(),
        hidden_dim: 128,
    };
    let mut trainer = PPOTrainer::with_exploration(ppo_config, 0.998, 0.05);

    let env_config = TradingEnvConfig {
        market: MarketConfig::default(),
        initial_capital: capital,
        max_position: 100,
        transaction_cost: 0.001,
        max_steps: (duration as usize) * 60 * 2,
        take_profit: 0.05,
        stop_loss: 0.02,
    };

    info!("Starting backtest with {} episodes...", episodes);

    let results = train_backtest(
        &mut trainer,
        env_config,
        episodes,
        duration,
        volatility,
        verbose,
    );

    let summary = summarize_backtest_results(&results);

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║               Backtest Summary                               ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!(
        "║  Episodes:        {:>10}                                   ║",
        summary.num_episodes
    );
    println!(
        "║  Avg PnL:         {:>10.2}                                   ║",
        summary.avg_pnl
    );
    println!(
        "║  Total PnL:       {:>10.2}                                   ║",
        summary.total_pnl
    );
    println!(
        "║  Avg Trades:      {:>10.1}                                   ║",
        summary.avg_trades
    );
    println!(
        "║  Win Rate:        {:>9.1}%                                   ║",
        summary.avg_win_rate * 100.0
    );
    println!(
        "║  Episode Win %:   {:>9.1}%                                   ║",
        summary.episode_win_rate * 100.0
    );
    println!(
        "║  Profit Factor:   {:>10.2}                                   ║",
        summary.profit_factor
    );
    println!(
        "║  Max Drawdown:    {:>9.1}%                                   ║",
        summary.max_drawdown * 100.0
    );
    println!("╚══════════════════════════════════════════════════════════════╝");

    if episodes >= 20 {
        let phase_size = episodes / 5;
        println!("\n╔══════════════════════════════════════════════════════════════╗");
        println!("║               Phase Analysis                                 ║");
        println!("╠══════════════════════════════════════════════════════════════╣");

        for (i, phase) in results.chunks(phase_size).enumerate() {
            let phase_summary = summarize_backtest_results(phase);
            println!(
                "║  Phase {}: pnl={:>7.2}, trades={:>5.1}, win={:>5.1}%           ║",
                i + 1,
                phase_summary.avg_pnl,
                phase_summary.avg_trades,
                phase_summary.avg_win_rate * 100.0
            );
        }
        println!("╚══════════════════════════════════════════════════════════════╝");
    }

    Ok(())
}
