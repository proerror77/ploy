#[cfg(feature = "rl")]
use ploy::error::Result;
#[cfg(feature = "rl")]
use tokio::signal;
#[cfg(feature = "rl")]
use tracing::{error, info, warn};

#[cfg(feature = "rl")]
pub(super) async fn run_lead_lag(
    episodes: usize,
    trade_size: f64,
    max_position: f64,
    symbol: &str,
    checkpoint: &str,
    verbose: bool,
) -> Result<()> {
    use ploy::adapters::PostgresStore;
    use ploy::config::AppConfig;
    use ploy::rl::environment::{LeadLagAction, LeadLagConfig, LeadLagEnvironment, LobDataPoint};
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

    std::fs::create_dir_all(checkpoint).ok();

    let config = AppConfig::load()?;
    let store = PostgresStore::new(&config.database.url, 5).await?;

    info!("Loading training data from sync_records...");

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
    .bind(symbol.to_uppercase())
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

    let env_config = LeadLagConfig {
        trade_size_usd: Decimal::try_from(trade_size).unwrap_or(Decimal::ONE),
        max_position_usd: Decimal::try_from(max_position).unwrap_or(Decimal::new(50, 0)),
        ..Default::default()
    };

    let mut total_rewards = Vec::new();
    let mut exploration_rate = 0.5f32;
    let exploration_decay = 0.995f32;
    let min_exploration = 0.05f32;

    let num_actions = LeadLagAction::num_actions();
    let mut action_values = vec![0.0f32; num_actions];
    let learning_rate = 0.01f32;

    println!("\nTraining {} episodes...\n", episodes);

    for episode in 0..episodes {
        let mut env = LeadLagEnvironment::new(env_config.clone(), data.clone());
        let mut _obs = env.reset();
        let mut episode_reward = 0.0f32;
        let mut steps = 0;

        loop {
            let action_idx = if rand::random::<f32>() < exploration_rate {
                rand::random::<usize>() % num_actions
            } else {
                action_values
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            };

            let action = LeadLagAction::from(action_idx);
            let result = env.step(action);

            action_values[action_idx] +=
                learning_rate * (result.reward - action_values[action_idx]);

            episode_reward += result.reward;
            steps += 1;
            _obs = result.observation;

            if result.done {
                break;
            }
        }

        exploration_rate = (exploration_rate * exploration_decay).max(min_exploration);
        total_rewards.push(episode_reward);

        if verbose || episode % 100 == 0 {
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
    Ok(())
}

#[cfg(feature = "rl")]
pub(super) async fn run_lead_lag_live(
    symbol: &str,
    trade_size: f64,
    max_position: f64,
    market: &str,
    checkpoint: &str,
    dry_run: bool,
    min_confidence: f64,
) -> Result<()> {
    use ploy::collector::{SyncCollector, SyncCollectorConfig};
    use ploy::config::AppConfig;
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

    let checkpoint_path = format!("{}/action_values.json", checkpoint);
    let checkpoint_content = std::fs::read_to_string(&checkpoint_path).map_err(|e| {
        ploy::error::PloyError::Internal(format!("Failed to load checkpoint: {}", e))
    })?;
    let checkpoint_data: serde_json::Value = serde_json::from_str(&checkpoint_content)?;

    let action_values: Vec<f32> = checkpoint_data["action_values"]
        .as_array()
        .ok_or_else(|| ploy::error::PloyError::Validation("Invalid checkpoint format".to_string()))?
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
    info!(
        "Action values: Hold={:.4}, BuyYes={:.4}, BuyNo={:.4}, CloseYes={:.4}, CloseNo={:.4}",
        action_values[0], action_values[1], action_values[2], action_values[3], action_values[4]
    );

    let config = AppConfig::load()?;

    let collector_config = SyncCollectorConfig {
        binance_symbols: vec![symbol.to_uppercase()],
        polymarket_slugs: vec![market.to_string()],
        snapshot_interval_ms: 100,
        database_url: config.database.url.clone(),
    };

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database.url)
        .await?;

    let collector = SyncCollector::new(collector_config).with_pool(pool.clone());
    let mut rx = collector.subscribe();

    let collector_handle = tokio::spawn(async move {
        if let Err(e) = collector.run().await {
            error!("Collector error: {}", e);
        }
    });

    let mut yes_position: Decimal = Decimal::ZERO;
    let mut no_position: Decimal = Decimal::ZERO;
    let max_pos = Decimal::try_from(max_position).unwrap_or(Decimal::new(50, 0));
    let trade_sz = Decimal::try_from(trade_size).unwrap_or(Decimal::ONE);
    let mut trade_count = 0u64;

    println!("\n📡 Listening for market signals... (Ctrl+C to stop)\n");

    loop {
        tokio::select! {
            record = rx.recv() => {
                match record {
                    Ok(r) => {
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

                        let max_val = action_values
                            .iter()
                            .cloned()
                            .fold(f32::NEG_INFINITY, f32::max);
                        let sum_exp: f32 = action_values.iter().map(|v| (v - max_val).exp()).sum();
                        let probs: Vec<f32> = action_values
                            .iter()
                            .map(|v| (v - max_val).exp() / sum_exp)
                            .collect();

                        let best_action = probs
                            .iter()
                            .enumerate()
                            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                            .map(|(i, p)| (LeadLagAction::from(i), *p))
                            .unwrap_or((LeadLagAction::Hold, 0.0));

                        let (action, confidence) = best_action;
                        if confidence < min_confidence as f32 {
                            continue;
                        }

                        let total_position = yes_position + no_position;
                        let can_buy = total_position + trade_sz <= max_pos;

                        match action {
                            LeadLagAction::Hold => {}
                            LeadLagAction::BuyYes if can_buy && obs.pm_yes_price > Decimal::ZERO => {
                                trade_count += 1;
                                if dry_run {
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
                                if dry_run {
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
                                if dry_run {
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
                                if dry_run {
                                    println!("⬜ [DRY] CloseNo @ {:.4} (conf: {:.2}%)", sell_price, confidence * 100.0);
                                } else {
                                    println!("⬜ CloseNo @ {:.4} (conf: {:.2}%)", sell_price, confidence * 100.0);
                                    // TODO: Execute real order
                                }
                                no_position -= trade_sz.min(no_position);
                            }
                            _ => {}
                        }

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
    Ok(())
}
