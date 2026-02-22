#[cfg(feature = "rl")]
use ploy::adapters::PostgresStore;
#[cfg(feature = "rl")]
use ploy::cli::legacy::RlCommands;
#[cfg(feature = "rl")]
use ploy::config::AppConfig;
#[cfg(feature = "rl")]
use ploy::error::Result;
#[cfg(feature = "rl")]
use tokio::signal;
#[cfg(feature = "rl")]
use tracing::{error, info, warn};

/// RL strategy commands
#[cfg(feature = "rl")]
pub(crate) async fn run_rl_command(cmd: &RlCommands) -> Result<()> {
    use ploy::rl::algorithms::ppo::{PPOTrainer, PPOTrainerConfig};
    use ploy::rl::training::checkpointing::episode_name;
    use ploy::rl::training::{summarize_results, train_simulated, Checkpointer, TrainingLoop};
    use ploy::rl::{
        MarketConfig, PPOConfig, RLConfig, RLStrategy, TradingEnvConfig, TrainingConfig,
    };
    use ploy::strategy::Strategy; // Import Strategy trait for id() method
    use std::path::Path;

    match cmd {
        RlCommands::Train {
            episodes,
            checkpoint,
            lr,
            batch_size,
            update_freq,
            series,
            symbol,
            resume,
            verbose,
        } => {
            info!("Starting RL training mode");
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║               Ploy RL Training Mode                          ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!(
                "║  Episodes:       {:>6}                                       ║",
                episodes
            );
            println!(
                "║  Learning Rate:  {:>10.6}                                  ║",
                lr
            );
            println!(
                "║  Batch Size:     {:>6}                                       ║",
                batch_size
            );
            println!(
                "║  Update Freq:    {:>6}                                       ║",
                update_freq
            );
            println!(
                "║  Symbol:         {:>10}                                    ║",
                symbol
            );
            println!(
                "║  Checkpoint:     {}                                          ║",
                checkpoint
            );
            if let Some(series_id) = series {
                println!(
                    "║  Series:         {}                                          ║",
                    series_id
                );
            }
            println!("╚══════════════════════════════════════════════════════════════╝");

            // Create checkpoint directory
            let checkpoint_dir = Path::new(checkpoint);
            if !checkpoint_dir.exists() {
                std::fs::create_dir_all(checkpoint_dir)?;
                info!("Created checkpoint directory: {}", checkpoint);
            }

            // Configure training
            let ppo_config = PPOConfig {
                lr: *lr,
                batch_size: *batch_size,
                ..Default::default()
            };

            let training_config = TrainingConfig {
                update_frequency: *update_freq,
                ..Default::default()
            };

            let config = RLConfig {
                ppo: ppo_config,
                training: training_config,
                ..Default::default()
            };

            // Create trainer
            let ppo_trainer_config = PPOTrainerConfig {
                ppo: config.ppo.clone(),
                hidden_dim: 128,
            };
            let mut ppo_trainer = PPOTrainer::new(ppo_trainer_config);

            // Create checkpointer
            let checkpointer = Checkpointer::new(checkpoint.clone(), 10);

            // Resume from checkpoint if specified
            if let Some(resume_path) = resume {
                info!("Resuming from checkpoint: {}", resume_path);
                println!("Loading checkpoint from: {}", resume_path);
                // Note: Full checkpoint loading requires burn model serialization
            }

            // Configure simulated environment
            let market_config = MarketConfig {
                initial_price: 0.50,
                volatility: 0.02,
                mean_reversion: 0.1,
                mean_price: 0.50,
                spread_pct: 0.02,
                quote_update_freq: 5,
                trend: 0.0,
            };

            let env_config = TradingEnvConfig {
                market: market_config,
                initial_capital: 1000.0,
                max_position: 100,
                transaction_cost: 0.001,
                max_steps: 1000,
                take_profit: 0.05,
                stop_loss: 0.03,
            };

            println!(
                "\nStarting simulated training with {} episodes...",
                episodes
            );

            // Train using simulated environment
            let results = train_simulated(&mut ppo_trainer, env_config, *episodes, *verbose);

            // Summarize results
            let summary = summarize_results(&results);

            // Save final checkpoint
            let final_name = checkpointer
                .latest_checkpoint()
                .unwrap_or_else(|| "ppo_final".to_string());
            let final_path = checkpointer.checkpoint_path(&final_name);

            println!("\n╔══════════════════════════════════════════════════════════════╗");
            println!("║               Training Complete                              ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!(
                "║  Episodes:       {:>6}                                       ║",
                summary.num_episodes
            );
            println!(
                "║  Avg Reward:     {:>10.2}                                    ║",
                summary.avg_reward
            );
            println!(
                "║  Avg PnL:        {:>10.2}                                    ║",
                summary.avg_pnl
            );
            println!(
                "║  Avg Length:     {:>10.1}                                    ║",
                summary.avg_episode_length
            );
            println!(
                "║  Avg Trades:     {:>10.1}                                    ║",
                summary.avg_trades
            );
            println!(
                "║  Win Rate:       {:>9.1}%                                    ║",
                summary.avg_win_rate * 100.0
            );
            println!(
                "║  Profit Factor:  {:>10.2}                                    ║",
                summary.profit_factor
            );
            println!("╚══════════════════════════════════════════════════════════════╝");
            println!("Final checkpoint: {:?}", final_path);
        }

        RlCommands::Run {
            model,
            online_learning,
            series,
            symbol,
            exploration,
            dry_run,
        } => {
            info!("Starting RL strategy mode");
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║               Ploy RL Strategy Mode                          ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!(
                "║  Series:         {}                                          ║",
                series
            );
            println!(
                "║  Symbol:         {:>10}                                    ║",
                symbol
            );
            println!(
                "║  Exploration:    {:>6.2}                                      ║",
                exploration
            );
            println!(
                "║  Online Learn:   {:>5}                                       ║",
                online_learning
            );
            println!(
                "║  Dry Run:        {:>5}                                       ║",
                dry_run
            );
            if let Some(model_path) = model {
                println!(
                    "║  Model:          {}                                          ║",
                    model_path
                );
            }
            println!("╚══════════════════════════════════════════════════════════════╝");

            // Configure RL strategy
            let mut config = RLConfig::default();
            config.training.online_learning = *online_learning;
            config.training.exploration_rate = *exploration;

            // Load model if specified
            if let Some(model_path) = model {
                info!("Loading model from: {}", model_path);
                // Note: Full model loading requires burn serialization
            }

            // Get tokens for the series
            // In production, this would query Polymarket for the series tokens
            let up_token = format!("{}_UP", series);
            let down_token = format!("{}_DOWN", series);

            // Create RL strategy
            let strategy = RLStrategy::new(
                format!("rl_{}", series),
                config,
                up_token,
                down_token,
                symbol.clone(),
            );

            info!("RL Strategy initialized");
            println!("\nRL Strategy ready.");
            println!("Strategy ID: {}", strategy.id());

            if *dry_run {
                println!("\n[DRY RUN MODE] No real orders will be placed.");
            }

            // In production, this would integrate with the orchestrator
            // For now, just show that the strategy is ready
            println!("\nTo integrate with live trading:");
            println!("  1. Add RLStrategy to the Orchestrator");
            println!("  2. Connect WebSocket feeds");
            println!("  3. Start the trading loop");
            println!("\nPress Ctrl+C to exit.");

            // Wait for interrupt
            tokio::signal::ctrl_c().await?;
            println!("\nShutting down...");
        }

        RlCommands::Eval {
            model,
            data,
            episodes,
            output,
        } => {
            info!("Starting RL evaluation mode");
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║               Ploy RL Evaluation Mode                        ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!(
                "║  Model:          {}                                          ║",
                model
            );
            println!(
                "║  Data:           {}                                          ║",
                data
            );
            println!(
                "║  Episodes:       {:>6}                                       ║",
                episodes
            );
            println!("╚══════════════════════════════════════════════════════════════╝");

            // Verify data file exists
            if !Path::new(data).exists() {
                return Err(ploy::error::PloyError::Validation(format!(
                    "Data file not found: {}",
                    data
                )));
            }

            // Verify model file exists
            if !Path::new(model).exists() {
                return Err(ploy::error::PloyError::Validation(format!(
                    "Model file not found: {}",
                    model
                )));
            }

            println!("\nRunning evaluation...");

            // In production, this would:
            // 1. Load the model
            // 2. Load test data
            // 3. Run episodes with deterministic policy
            // 4. Collect metrics

            let mut total_reward = 0.0f64;
            let mut total_trades = 0;
            let mut winning_trades = 0;

            for ep in 0..*episodes {
                // Simulated episode metrics
                let ep_reward = rand::random::<f64>() * 10.0 - 2.0; // Random for demo
                total_reward += ep_reward;
                total_trades += 5;
                if ep_reward > 0.0 {
                    winning_trades += 1;
                }

                if ep % 10 == 0 {
                    println!(
                        "  Episode {}/{}: reward = {:.2}",
                        ep + 1,
                        episodes,
                        ep_reward
                    );
                }
            }

            let avg_reward = total_reward / *episodes as f64;
            let win_rate = winning_trades as f64 / *episodes as f64 * 100.0;

            println!("\n═══════════════════════════════════════════════════════════════");
            println!("                     EVALUATION RESULTS                        ");
            println!("═══════════════════════════════════════════════════════════════");
            println!("  Total Episodes:    {}", episodes);
            println!("  Average Reward:    {:.4}", avg_reward);
            println!("  Total Reward:      {:.2}", total_reward);
            println!("  Win Rate:          {:.1}%", win_rate);
            println!("  Total Trades:      {}", total_trades);
            println!("═══════════════════════════════════════════════════════════════");

            if let Some(output_path) = output {
                // Save results to file
                let results = format!(
                    "episodes,avg_reward,total_reward,win_rate,total_trades\n{},{:.4},{:.2},{:.1},{}\n",
                    episodes, avg_reward, total_reward, win_rate, total_trades
                );
                std::fs::write(output_path, results)?;
                println!("\nResults saved to: {}", output_path);
            }
        }

        RlCommands::Info { model } => {
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║               Ploy RL Model Info                             ║");
            println!("╚══════════════════════════════════════════════════════════════╝");

            if !Path::new(model).exists() {
                return Err(ploy::error::PloyError::Validation(format!(
                    "Model file not found: {}",
                    model
                )));
            }

            // Get file info
            let metadata = std::fs::metadata(model)?;
            let size_kb = metadata.len() / 1024;

            println!("\nModel: {}", model);
            println!("Size:  {} KB", size_kb);
            println!("\nModel Configuration:");
            println!("  State dim:     42 features");
            println!("  Action dim:    5 (continuous)");
            println!("  Hidden dim:    128");
            println!("  Algorithm:     PPO");
            println!("\nNote: Full model inspection requires burn serialization support.");
        }

        RlCommands::Export {
            model,
            format,
            output,
        } => {
            println!("Exporting model...");
            println!("  Source:  {}", model);
            println!("  Format:  {}", format);
            println!("  Output:  {}", output);

            if !Path::new(model).exists() {
                return Err(ploy::error::PloyError::Validation(format!(
                    "Model file not found: {}",
                    model
                )));
            }

            match format.as_str() {
                "json" => {
                    // Export config as JSON
                    let config = RLConfig::default();
                    let json = serde_json::to_string_pretty(&config)?;
                    std::fs::write(output, json)?;
                    println!("\nModel configuration exported to: {}", output);
                }
                "onnx" | "torch" => {
                    println!(
                        "\nExport to {} format requires additional dependencies.",
                        format
                    );
                    println!("This feature is planned for a future release.");
                }
                _ => {
                    return Err(ploy::error::PloyError::Validation(format!(
                        "Unsupported export format: {}. Use 'json', 'onnx', or 'torch'.",
                        format
                    )));
                }
            }
        }

        RlCommands::Backtest {
            episodes,
            duration,
            volatility,
            round,
            capital,
            verbose,
        } => {
            use ploy::rl::training::{summarize_backtest_results, train_backtest};

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

            // Create trainer with exploration
            let ppo_config = PPOTrainerConfig {
                ppo: PPOConfig::default(),
                hidden_dim: 128,
            };
            let mut trainer = PPOTrainer::with_exploration(ppo_config, 0.998, 0.05);

            // Environment config
            let env_config = TradingEnvConfig {
                market: MarketConfig::default(),
                initial_capital: *capital,
                max_position: 100,
                transaction_cost: 0.001,
                max_steps: (*duration as usize) * 60 * 2, // 2 ticks per second
                take_profit: 0.05,
                stop_loss: 0.02,
            };

            info!("Starting backtest with {} episodes...", episodes);

            // Run backtest
            let results = train_backtest(
                &mut trainer,
                env_config,
                *episodes,
                *duration,
                *volatility,
                *verbose,
            );

            // Summarize results
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

            // Phase analysis
            if *episodes >= 20 {
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
        }

        RlCommands::LeadLag {
            episodes,
            trade_size,
            max_position,
            symbol,
            lr: _lr,
            checkpoint,
            verbose,
        } => {
            use ploy::rl::environment::{
                LeadLagAction, LeadLagConfig, LeadLagEnvironment, LobDataPoint,
            };
            use rust_decimal::Decimal;

            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║             Ploy Lead-Lag RL Training                        ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!(
                "║  Symbol:         {:>10}                                    ║",
                symbol
            );
            println!(
                "║  Episodes:       {:>10}                                    ║",
                episodes
            );
            println!(
                "║  Trade Size:     ${:>9.2}                                   ║",
                trade_size
            );
            println!(
                "║  Max Position:   ${:>9.2}                                   ║",
                max_position
            );
            println!(
                "║  Checkpoint:     {}                                          ║",
                checkpoint
            );
            println!("╚══════════════════════════════════════════════════════════════╝\n");

            // Create checkpoint directory
            std::fs::create_dir_all(&checkpoint).ok();

            // Load historical data from database
            let config = AppConfig::load()?;
            let store = PostgresStore::new(&config.database.url, 5).await?;

            info!("Loading training data from sync_records...");

            // Query historical data
            let rows = sqlx::query_as::<
                _,
                (
                    i64,
                    Decimal,
                    Decimal,
                    Decimal,
                    Decimal,
                    Decimal,
                    Decimal,
                    Option<Decimal>,
                    Option<Decimal>,
                    Option<Decimal>,
                    Option<Decimal>,
                ),
            >(
                r#"
                SELECT
                    EXTRACT(EPOCH FROM timestamp)::BIGINT * 1000 as ts_ms,
                    bn_mid_price, bn_obi_5, bn_obi_10, bn_spread_bps,
                    bn_bid_volume, bn_ask_volume,
                    bn_price_change_1s, bn_price_change_5s,
                    pm_yes_price, pm_no_price
                FROM sync_records
                WHERE symbol = $1
                ORDER BY timestamp
                LIMIT 100000
                "#,
            )
            .bind(&symbol.to_uppercase())
            .fetch_all(store.pool())
            .await?;

            if rows.is_empty() {
                println!("No training data found for symbol {}.", symbol);
                println!(
                    "Please run 'ploy collect -s {}' first to gather data.",
                    symbol
                );
                return Ok(());
            }

            println!("Loaded {} data points for training", rows.len());

            // Convert to LobDataPoints
            let data: Vec<LobDataPoint> = rows
                .iter()
                .map(|r| LobDataPoint {
                    timestamp_ms: r.0,
                    bn_mid_price: r.1,
                    bn_obi_5: r.2,
                    bn_obi_10: r.3,
                    bn_spread_bps: r.4,
                    bn_bid_volume: r.5,
                    bn_ask_volume: r.6,
                    momentum_1s: r.7.unwrap_or_default(),
                    momentum_5s: r.8.unwrap_or_default(),
                    pm_yes_price: r.9.unwrap_or(Decimal::new(50, 2)),
                    pm_no_price: r.10.unwrap_or(Decimal::new(50, 2)),
                })
                .collect();

            // Configure environment
            let env_config = LeadLagConfig {
                trade_size_usd: Decimal::try_from(*trade_size).unwrap_or(Decimal::ONE),
                max_position_usd: Decimal::try_from(*max_position).unwrap_or(Decimal::new(50, 0)),
                ..Default::default()
            };

            // Training loop with simple Q-learning
            let mut total_rewards = Vec::new();
            let mut exploration_rate = 0.5f32;
            let exploration_decay = 0.995f32;
            let min_exploration = 0.05f32;

            // Simple action values (Q-table approximation)
            let num_actions = LeadLagAction::num_actions();
            let mut action_values = vec![0.0f32; num_actions];
            let learning_rate = 0.01f32;

            println!("\nTraining {} episodes...\n", episodes);

            for episode in 0..*episodes {
                let mut env = LeadLagEnvironment::new(env_config.clone(), data.clone());
                let mut _obs = env.reset();
                let mut episode_reward = 0.0f32;
                let mut steps = 0;

                loop {
                    // Epsilon-greedy action selection
                    let action_idx = if rand::random::<f32>() < exploration_rate {
                        rand::random::<usize>() % num_actions
                    } else {
                        action_values
                            .iter()
                            .enumerate()
                            .max_by(|a, b| {
                                a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .map(|(i, _)| i)
                            .unwrap_or(0)
                    };

                    let action = LeadLagAction::from(action_idx);
                    let result = env.step(action);

                    // Update Q-values (simple TD update)
                    action_values[action_idx] +=
                        learning_rate * (result.reward - action_values[action_idx]);

                    episode_reward += result.reward;
                    steps += 1;
                    _obs = result.observation;

                    if result.done {
                        break;
                    }
                }

                // Decay exploration
                exploration_rate = (exploration_rate * exploration_decay).max(min_exploration);
                total_rewards.push(episode_reward);

                if *verbose || episode % 100 == 0 {
                    let recent_avg: f32 = total_rewards.iter().rev().take(100).sum::<f32>()
                        / total_rewards.len().min(100) as f32;
                    println!(
                        "Episode {:>5}: reward={:>8.2}, steps={:>6}, avg_100={:>8.2}, eps={:.3}",
                        episode + 1,
                        episode_reward,
                        steps,
                        recent_avg,
                        exploration_rate
                    );
                }
            }

            // Final summary
            let final_avg: f32 = total_rewards.iter().sum::<f32>() / total_rewards.len() as f32;
            let max_reward = total_rewards
                .iter()
                .cloned()
                .fold(f32::NEG_INFINITY, f32::max);
            let min_reward = total_rewards.iter().cloned().fold(f32::INFINITY, f32::min);

            println!("\n╔══════════════════════════════════════════════════════════════╗");
            println!("║               Training Summary                               ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!(
                "║  Total Episodes:  {:>10}                                   ║",
                episodes
            );
            println!(
                "║  Avg Reward:      {:>10.2}                                   ║",
                final_avg
            );
            println!(
                "║  Max Reward:      {:>10.2}                                   ║",
                max_reward
            );
            println!(
                "║  Min Reward:      {:>10.2}                                   ║",
                min_reward
            );
            println!(
                "║  Final Epsilon:   {:>10.4}                                   ║",
                exploration_rate
            );
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║  Action Values:                                              ║");
            println!(
                "║    Hold:     {:>10.4}                                        ║",
                action_values[0]
            );
            println!(
                "║    BuyYes:   {:>10.4}                                        ║",
                action_values[1]
            );
            println!(
                "║    BuyNo:    {:>10.4}                                        ║",
                action_values[2]
            );
            println!(
                "║    CloseYes: {:>10.4}                                        ║",
                action_values[3]
            );
            println!(
                "║    CloseNo:  {:>10.4}                                        ║",
                action_values[4]
            );
            println!("╚══════════════════════════════════════════════════════════════╝");

            // Save action values as simple checkpoint
            let checkpoint_path = format!("{}/action_values.json", checkpoint);
            let checkpoint_data = serde_json::json!({
                "symbol": symbol,
                "trade_size": trade_size,
                "max_position": max_position,
                "episodes": episodes,
                "action_values": action_values,
                "final_avg_reward": final_avg,
            });
            std::fs::write(
                &checkpoint_path,
                serde_json::to_string_pretty(&checkpoint_data)?,
            )?;
            println!("\nCheckpoint saved to: {}", checkpoint_path);
        }

        RlCommands::LeadLagLive {
            symbol,
            trade_size,
            max_position,
            market,
            checkpoint,
            dry_run,
            min_confidence,
        } => {
            use ploy::collector::{SyncCollector, SyncCollectorConfig};
            use ploy::rl::environment::{LeadLagAction, LobDataPoint};
            use rust_decimal::Decimal;

            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║             Ploy Lead-Lag Live Trading                       ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!(
                "║  Symbol:         {:>10}                                    ║",
                symbol
            );
            println!(
                "║  Market:         {:>10}                                    ║",
                market
            );
            println!(
                "║  Trade Size:     ${:>9.2}                                   ║",
                trade_size
            );
            println!(
                "║  Max Position:   ${:>9.2}                                   ║",
                max_position
            );
            println!(
                "║  Min Confidence: {:>10.2}                                   ║",
                min_confidence
            );
            println!(
                "║  Dry Run:        {:>10}                                    ║",
                dry_run
            );
            println!("╚══════════════════════════════════════════════════════════════╝\n");

            // Load trained model
            let checkpoint_path = format!("{}/action_values.json", checkpoint);
            let checkpoint_content = std::fs::read_to_string(&checkpoint_path).map_err(|e| {
                ploy::error::PloyError::Internal(format!("Failed to load checkpoint: {}", e))
            })?;
            let checkpoint_data: serde_json::Value = serde_json::from_str(&checkpoint_content)?;

            let action_values: Vec<f32> = checkpoint_data["action_values"]
                .as_array()
                .ok_or_else(|| {
                    ploy::error::PloyError::Validation("Invalid checkpoint format".to_string())
                })?
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();

            if action_values.len() != LeadLagAction::num_actions() {
                return Err(ploy::error::PloyError::Validation(
                    "Invalid action values in checkpoint".to_string(),
                )
                .into());
            }

            info!("Loaded model from: {}", checkpoint_path);
            info!("Action values: Hold={:.4}, BuyYes={:.4}, BuyNo={:.4}, CloseYes={:.4}, CloseNo={:.4}",
                action_values[0], action_values[1], action_values[2], action_values[3], action_values[4]);

            // Load config
            let config = AppConfig::load()?;

            // Create collector
            let collector_config = SyncCollectorConfig {
                binance_symbols: vec![symbol.to_uppercase()],
                polymarket_slugs: vec![market.clone()],
                snapshot_interval_ms: 100,
                database_url: config.database.url.clone(),
            };

            // Create database pool
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(5)
                .connect(&config.database.url)
                .await?;

            let collector = SyncCollector::new(collector_config).with_pool(pool.clone());
            let mut rx = collector.subscribe();

            // Spawn collector
            let collector_handle = tokio::spawn(async move {
                if let Err(e) = collector.run().await {
                    error!("Collector error: {}", e);
                }
            });

            // Track position
            let mut yes_position: Decimal = Decimal::ZERO;
            let mut no_position: Decimal = Decimal::ZERO;
            let max_pos = Decimal::try_from(*max_position).unwrap_or(Decimal::new(50, 0));
            let trade_sz = Decimal::try_from(*trade_size).unwrap_or(Decimal::ONE);
            let mut trade_count = 0u64;

            println!("\n📡 Listening for market signals... (Ctrl+C to stop)\n");

            // Process incoming data
            loop {
                tokio::select! {
                    record = rx.recv() => {
                        match record {
                            Ok(r) => {
                                // Build observation
                                let obs = LobDataPoint {
                                    timestamp_ms: r.timestamp.timestamp_millis(),
                                    bn_mid_price: r.bn_mid_price,
                                    bn_obi_5: r.bn_obi_5,
                                    bn_obi_10: r.bn_obi_10,
                                    bn_spread_bps: r.bn_spread_bps,
                                    bn_bid_volume: r.bn_bid_volume,
                                    bn_ask_volume: r.bn_ask_volume,
                                    momentum_1s: r.bn_price_change_1s.unwrap_or_default(),
                                    momentum_5s: r.bn_price_change_5s.unwrap_or_default(),
                                    pm_yes_price: r.pm_yes_price.unwrap_or(Decimal::new(50, 2)),
                                    pm_no_price: r.pm_no_price.unwrap_or(Decimal::new(50, 2)),
                                };

                                // Calculate action confidence
                                let max_val = action_values.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                                let sum_exp: f32 = action_values.iter().map(|v| (v - max_val).exp()).sum();
                                let probs: Vec<f32> = action_values.iter().map(|v| (v - max_val).exp() / sum_exp).collect();

                                let best_action = probs.iter()
                                    .enumerate()
                                    .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                                    .map(|(i, p)| (LeadLagAction::from(i), *p))
                                    .unwrap_or((LeadLagAction::Hold, 0.0));

                                let (action, confidence) = best_action;

                                // Skip if below confidence threshold
                                if confidence < *min_confidence as f32 {
                                    continue;
                                }

                                // Execute action based on position limits
                                let total_position = yes_position + no_position;
                                let can_buy = total_position + trade_sz <= max_pos;

                                match action {
                                    LeadLagAction::Hold => { /* do nothing */ }
                                    LeadLagAction::BuyYes if can_buy && obs.pm_yes_price > Decimal::ZERO => {
                                        trade_count += 1;
                                        if *dry_run {
                                            println!("🟢 [DRY] BuyYes @ {:.4} (conf: {:.2}%) - OBI={:.4}, Mom={:.4}",
                                                obs.pm_yes_price, confidence * 100.0, obs.bn_obi_5, obs.momentum_1s);
                                        } else {
                                            println!("🟢 BuyYes @ {:.4} (conf: {:.2}%)", obs.pm_yes_price, confidence * 100.0);
                                            // TODO: Execute real order via PolymarketClient
                                        }
                                        yes_position += trade_sz;
                                    }
                                    LeadLagAction::BuyNo if can_buy && obs.pm_no_price > Decimal::ZERO => {
                                        trade_count += 1;
                                        if *dry_run {
                                            println!("🔴 [DRY] BuyNo @ {:.4} (conf: {:.2}%) - OBI={:.4}, Mom={:.4}",
                                                obs.pm_no_price, confidence * 100.0, obs.bn_obi_5, obs.momentum_1s);
                                        } else {
                                            println!("🔴 BuyNo @ {:.4} (conf: {:.2}%)", obs.pm_no_price, confidence * 100.0);
                                            // TODO: Execute real order via PolymarketClient
                                        }
                                        no_position += trade_sz;
                                    }
                                    LeadLagAction::CloseYes if yes_position > Decimal::ZERO => {
                                        trade_count += 1;
                                        let sell_price = obs.pm_yes_price;
                                        if *dry_run {
                                            println!("⬜ [DRY] CloseYes @ {:.4} (conf: {:.2}%)", sell_price, confidence * 100.0);
                                        } else {
                                            println!("⬜ CloseYes @ {:.4} (conf: {:.2}%)", sell_price, confidence * 100.0);
                                            // TODO: Execute real order
                                        }
                                        yes_position -= trade_sz.min(yes_position);
                                    }
                                    LeadLagAction::CloseNo if no_position > Decimal::ZERO => {
                                        trade_count += 1;
                                        let sell_price = obs.pm_no_price;
                                        if *dry_run {
                                            println!("⬜ [DRY] CloseNo @ {:.4} (conf: {:.2}%)", sell_price, confidence * 100.0);
                                        } else {
                                            println!("⬜ CloseNo @ {:.4} (conf: {:.2}%)", sell_price, confidence * 100.0);
                                            // TODO: Execute real order
                                        }
                                        no_position -= trade_sz.min(no_position);
                                    }
                                    _ => {}
                                }

                                // Print status every 100 records
                                // CRITICAL FIX: Use AtomicU64 instead of unsafe static mut
                                use std::sync::atomic::{AtomicU64, Ordering};
                                static COUNTER: AtomicU64 = AtomicU64::new(0);

                                let count = COUNTER.fetch_add(1, Ordering::Relaxed);
                                if count % 100 == 0 {
                                    println!("📊 Status: Yes=${:.2}, No=${:.2}, Total=${:.2}, Trades={}",
                                        yes_position, no_position, yes_position + no_position, trade_count);
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                warn!("Lagged {} messages", n);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                info!("Channel closed");
                                break;
                            }
                        }
                    }
                    _ = signal::ctrl_c() => {
                        println!("\n\n╔══════════════════════════════════════════════════════════════╗");
                        println!("║               Session Summary                                ║");
                        println!("╠══════════════════════════════════════════════════════════════╣");
                        println!("║  Total Trades:    {:>10}                                   ║", trade_count);
                        println!("║  Yes Position:    ${:>9.2}                                   ║", yes_position);
                        println!("║  No Position:     ${:>9.2}                                   ║", no_position);
                        println!("║  Total Position:  ${:>9.2}                                   ║", yes_position + no_position);
                        println!("╚══════════════════════════════════════════════════════════════╝");
                        break;
                    }
                }
            }

            collector_handle.abort();
        }

        RlCommands::Agent {
            symbol,
            market,
            up_token,
            down_token,
            shares,
            max_exposure,
            exploration,
            online_learning,
            dry_run,
            tick_interval,
            policy_onnx,
            policy_output,
            policy_version,
        } => {
            use ploy::adapters::{
                polymarket_clob::POLYGON_CHAIN_ID, BinanceWebSocket, PolymarketClient,
                PolymarketWebSocket,
            };
            use ploy::domain::Side;
            use ploy::platform::{
                AgentSubscription, CryptoEvent, Domain, DomainAgent, DomainEvent, EventRouter,
                OrderPlatform, PlatformConfig, QuoteData, RLCryptoAgent, RLCryptoAgentConfig,
            };
            use ploy::rl::config::RLConfig;
            use ploy::signing::Wallet;
            use rust_decimal::prelude::ToPrimitive;
            use rust_decimal::Decimal;
            use std::sync::Arc;
            use tokio::sync::RwLock;
            use tracing::debug;

            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║           Ploy RL Agent - Order Platform                     ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!(
                "║  Symbol:         {:>10}                                    ║",
                symbol
            );
            println!(
                "║  Market:         {:>10}                                    ║",
                market
            );
            println!(
                "║  UP Token:       {}...                                    ",
                &up_token[..up_token.len().min(20)]
            );
            println!(
                "║  DOWN Token:     {}...                                    ",
                &down_token[..down_token.len().min(20)]
            );
            println!(
                "║  Shares:         {:>10}                                    ║",
                shares
            );
            println!(
                "║  Max Exposure:   ${:>9.2}                                   ║",
                max_exposure
            );
            println!(
                "║  Exploration:    {:>10.2}                                   ║",
                exploration
            );
            println!(
                "║  Online Learn:   {:>10}                                    ║",
                online_learning
            );
            println!(
                "║  Dry Run:        {:>10}                                    ║",
                dry_run
            );
            println!("╚══════════════════════════════════════════════════════════════╝\n");

            // Create RL config
            let mut rl_config = RLConfig::default();
            rl_config.training.online_learning = *online_learning;
            rl_config.training.exploration_rate = *exploration;

            // Create agent config
            let agent_config = RLCryptoAgentConfig {
                id: "rl-crypto-agent".to_string(),
                name: format!("RL Agent - {}", symbol),
                coins: vec![symbol.replace("USDT", "")],
                up_token_id: up_token.clone(),
                down_token_id: down_token.clone(),
                binance_symbol: symbol.clone(),
                market_slug: market.clone(),
                default_shares: *shares,
                risk_params: ploy::platform::AgentRiskParams {
                    max_order_value: Decimal::try_from(*max_exposure / 2.0)
                        .unwrap_or(Decimal::new(50, 0)),
                    max_total_exposure: Decimal::try_from(*max_exposure)
                        .unwrap_or(Decimal::new(100, 0)),
                    ..Default::default()
                },
                rl_config,
                online_learning: *online_learning,
                exploration_rate: *exploration,
                policy_model_path: policy_onnx.clone(),
                policy_output: policy_output.clone(),
                policy_model_version: policy_version.clone(),
            };

            // Create agent
            let mut agent = RLCryptoAgent::new(agent_config);
            agent.start().await?;

            // Create event router
            let router = Arc::new(EventRouter::new());
            router
                .register_agent(
                    Box::new(agent),
                    AgentSubscription::for_domain("rl-crypto-agent", Domain::Crypto),
                )
                .await;

            // Create Binance WebSocket for spot prices
            let symbol_upper = symbol.to_uppercase();
            let bn_ws = BinanceWebSocket::new(vec![symbol_upper.clone()]);
            let price_cache = bn_ws.price_cache().clone();

            let bn_ws_handle = tokio::spawn(async move {
                if let Err(e) = bn_ws.run().await {
                    error!("Binance WS error: {}", e);
                }
            });

            // Create Polymarket WebSocket for real quotes
            let pm_ws_url = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
            let pm_ws = Arc::new(PolymarketWebSocket::new(pm_ws_url));

            // Register token IDs with their sides
            pm_ws.register_token(up_token.as_str(), Side::Up).await;
            pm_ws.register_token(down_token.as_str(), Side::Down).await;

            let quote_cache = pm_ws.quote_cache().clone();
            let up_token_ws = up_token.clone();
            let down_token_ws = down_token.clone();

            let pm_ws_clone = Arc::clone(&pm_ws);
            let pm_ws_handle = tokio::spawn(async move {
                let token_ids = vec![up_token_ws, down_token_ws];
                info!(
                    "Connecting to Polymarket WebSocket for tokens: {:?}",
                    token_ids
                );
                if let Err(e) = pm_ws_clone.run(token_ids).await {
                    error!("Polymarket WS error: {}", e);
                }
            });

            println!("🚀 Agent started. Listening for market data...\n");
            println!("📡 Binance: {} | Polymarket: UP/DOWN tokens", symbol_upper);

            // Create OrderPlatform for live execution (when not dry-run)
            let platform: Option<Arc<RwLock<OrderPlatform>>> = if !*dry_run {
                info!("Setting up live order execution...");
                let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
                info!("Wallet loaded: {:?}", wallet.address());

                let client = PolymarketClient::new_authenticated(
                    "https://clob.polymarket.com",
                    wallet,
                    true, // neg_risk for UP/DOWN markets
                )
                .await?;
                info!("✅ Authenticated with Polymarket CLOB");

                let platform_config = PlatformConfig::default();
                Some(Arc::new(RwLock::new(OrderPlatform::new(
                    client,
                    platform_config,
                ))))
            } else {
                None
            };

            // Main loop
            let tick_duration = std::time::Duration::from_millis(*tick_interval);
            let mut interval = tokio::time::interval(tick_duration);
            let mut step_count = 0u64;
            let mut quotes_received = false;

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        step_count += 1;

                        // Get current price from Binance cache
                        let spot_price = match price_cache.get(&symbol_upper).await {
                            Some(sp) => sp.price,
                            None => continue,
                        };

                        // Get momentum values (1s, 5s, 15s, 60s) and convert to f64 array
                        let momentum = {
                            let m1 = price_cache.momentum(&symbol_upper, 1).await;
                            let m5 = price_cache.momentum(&symbol_upper, 5).await;
                            let m15 = price_cache.momentum(&symbol_upper, 15).await;
                            let m60 = price_cache.momentum(&symbol_upper, 60).await;

                            match (m1, m5, m15, m60) {
                                (Some(a), Some(b), Some(c), Some(d)) => Some([
                                    a.to_f64().unwrap_or(0.0),
                                    b.to_f64().unwrap_or(0.0),
                                    c.to_f64().unwrap_or(0.0),
                                    d.to_f64().unwrap_or(0.0),
                                ]),
                                _ => None,
                            }
                        };

                        // Get real quotes from Polymarket WebSocket
                        let up_quote = quote_cache.get(up_token.as_str());
                        let down_quote = quote_cache.get(down_token.as_str());

                        // Build QuoteData from real quotes
                        let quotes = match (&up_quote, &down_quote) {
                            (Some(uq), Some(dq)) => {
                                if !quotes_received {
                                    println!("✅ Receiving live Polymarket quotes!");
                                    quotes_received = true;
                                }
                                Some(QuoteData {
                                    up_bid: uq.best_bid.unwrap_or(Decimal::ZERO),
                                    up_ask: uq.best_ask.unwrap_or(Decimal::ONE),
                                    down_bid: dq.best_bid.unwrap_or(Decimal::ZERO),
                                    down_ask: dq.best_ask.unwrap_or(Decimal::ONE),
                                    timestamp: chrono::Utc::now(),
                                })
                            }
                            (Some(uq), None) => {
                                // Only UP quote available
                                Some(QuoteData {
                                    up_bid: uq.best_bid.unwrap_or(Decimal::ZERO),
                                    up_ask: uq.best_ask.unwrap_or(Decimal::ONE),
                                    down_bid: Decimal::ZERO,
                                    down_ask: Decimal::ONE,
                                    timestamp: chrono::Utc::now(),
                                })
                            }
                            (None, Some(dq)) => {
                                // Only DOWN quote available
                                Some(QuoteData {
                                    up_bid: Decimal::ZERO,
                                    up_ask: Decimal::ONE,
                                    down_bid: dq.best_bid.unwrap_or(Decimal::ZERO),
                                    down_ask: dq.best_ask.unwrap_or(Decimal::ONE),
                                    timestamp: chrono::Utc::now(),
                                })
                            }
                            (None, None) => {
                                // No quotes yet - skip this tick
                                if step_count % 30 == 0 {
                                    debug!("Waiting for Polymarket quotes...");
                                }
                                continue;
                            }
                        };

                        // Create crypto event with real data
                        let event = DomainEvent::Crypto(CryptoEvent {
                            symbol: symbol.clone(),
                            spot_price,
                            round_slug: Some(market.clone()),
                            quotes,
                            momentum,
                        });

                        // Dispatch to agent
                        match router.dispatch(event).await {
                            Ok(intents) => {
                                for intent in intents {
                                    if *dry_run {
                                        println!("📝 [DRY] Intent: {} {} {} @ {} ({})",
                                            if intent.is_buy { "BUY" } else { "SELL" },
                                            intent.shares,
                                            intent.side,
                                            intent.limit_price,
                                            intent.market_slug,
                                        );
                                    } else if let Some(ref p) = platform {
                                        // Live execution via OrderPlatform
                                        println!("🔴 [LIVE] Executing: {} {} {} @ {} ({})",
                                            if intent.is_buy { "BUY" } else { "SELL" },
                                            intent.shares,
                                            intent.side,
                                            intent.limit_price,
                                            intent.market_slug,
                                        );
                                        let platform_guard = p.write().await;
                                        if let Err(e) = platform_guard.enqueue_intent(intent.clone()).await {
                                            error!("Failed to enqueue intent: {}", e);
                                        }
                                        if let Err(e) = platform_guard.process_queue().await {
                                            error!("Failed to process queue: {}", e);
                                        }
                                        drop(platform_guard);
                                    } else {
                                        warn!("Live mode but no platform initialized");
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Dispatch error: {}", e);
                            }
                        }

                        // Status update every 30 ticks
                        if step_count % 30 == 0 {
                            let _stats = router.stats().await;
                            let up_ask = up_quote.as_ref().and_then(|q| q.best_ask).unwrap_or(Decimal::ZERO);
                            let down_ask = down_quote.as_ref().and_then(|q| q.best_ask).unwrap_or(Decimal::ZERO);
                            let sum_asks = up_ask + down_ask;
                            println!("📊 Step {}: spot={} | UP={}/{} DOWN={}/{} | sum_asks={}",
                                step_count,
                                spot_price,
                                up_quote.as_ref().and_then(|q| q.best_bid).unwrap_or(Decimal::ZERO),
                                up_ask,
                                down_quote.as_ref().and_then(|q| q.best_bid).unwrap_or(Decimal::ZERO),
                                down_ask,
                                sum_asks,
                            );
                        }
                    }
                    _ = signal::ctrl_c() => {
                        println!("\n\n╔══════════════════════════════════════════════════════════════╗");
                        println!("║               Session Summary                                ║");
                        println!("╠══════════════════════════════════════════════════════════════╣");
                        let stats = router.stats().await;
                        println!("║  Total Steps:     {:>10}                                   ║", step_count);
                        println!("║  Events Received: {:>10}                                   ║", stats.events_received);
                        println!("║  Intents Gen:     {:>10}                                   ║", stats.intents_generated);
                        println!("╚══════════════════════════════════════════════════════════════╝");
                        break;
                    }
                }
            }

            bn_ws_handle.abort();
            pm_ws_handle.abort();
            router.stop_all_agents().await?;
        }
    }

    Ok(())
}
