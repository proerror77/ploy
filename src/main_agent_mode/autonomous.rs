use ploy::error::Result;
use tokio::signal;
use tracing::{error, info};

pub(super) async fn run_autonomous_mode(
    market: Option<&str>,
    max_trade: f64,
    max_exposure: f64,
    enable_trading: bool,
) -> Result<()> {
    use ploy::agent::autonomous::AutonomyLevel;
    use ploy::agent::protocol::{AgentContext, DailyStats, MarketSnapshot};
    use ploy::agent::{AutonomousAgent, AutonomousConfig, ClaudeAgentClient};
    use ploy::domain::{RiskState, StrategyState};
    use rust_decimal::Decimal;

    let config = AutonomousConfig {
        autonomy_level: if enable_trading {
            AutonomyLevel::LimitedAutonomy
        } else {
            AutonomyLevel::AdvisoryOnly
        },
        max_trade_size: Decimal::from_f64_retain(max_trade).unwrap_or(Decimal::from(50)),
        max_total_exposure: Decimal::from_f64_retain(max_exposure).unwrap_or(Decimal::from(200)),
        min_confidence: 0.75,
        trading_enabled: enable_trading,
        analysis_interval_secs: 30,
        allowed_strategies: vec!["arbitrage".to_string()],
        require_exit_confirmation: true,
    };

    println!("\n\x1b[33mAutonomous Mode Configuration:\x1b[0m");
    println!("  Autonomy Level: {:?}", config.autonomy_level);
    println!("  Max Trade Size: ${}", config.max_trade_size);
    println!("  Max Exposure: ${}", config.max_total_exposure);
    println!("  Trading Enabled: {}", config.trading_enabled);
    println!(
        "  Min Confidence: {}%",
        (config.min_confidence * 100.0) as u32
    );

    if !enable_trading {
        println!(
            "\n\x1b[33m⚠️  Trading is disabled. Use --enable-trading to execute trades.\x1b[0m"
        );
    }

    use ploy::agent::AgentClientConfig;
    let client = ClaudeAgentClient::with_config(AgentClientConfig::for_autonomous());
    let mut agent = AutonomousAgent::new(client, config);

    if enable_trading {
        println!("  Trading backend: initializing authenticated Polymarket client...");
        let trading_client =
            crate::main_runtime::create_pm_client("https://clob.polymarket.com", false).await?;
        println!("  Trading backend: \x1b[32m✓ Ready (live orders enabled)\x1b[0m");
        agent = agent.with_trading_client(trading_client);
    } else {
        println!("  Trading backend: \x1b[33mdisabled\x1b[0m");
    }

    use ploy::agent::{GrokClient, GrokConfig};
    if let Ok(grok) = GrokClient::new(GrokConfig::from_env()) {
        if grok.is_configured() {
            println!("  Grok: \x1b[32m✓ Enabled\x1b[0m (real-time market intelligence)");
            agent = agent.with_grok(grok);
        } else {
            println!(
                "  Grok: \x1b[33m⚠ Not configured\x1b[0m (set GROK_API_KEY for real-time search)"
            );
        }
    }

    let market_slug = market
        .map(ToString::to_string)
        .unwrap_or_else(|| "demo-market".to_string());
    let market_slug_clone = market_slug.clone();

    println!("\nFetching market data for: {}", market_slug);
    let _initial_snapshot = match super::fetch_market_snapshot(&market_slug).await {
        Ok(snapshot) => {
            println!("\x1b[32m✓ Market data loaded\x1b[0m");
            if let Some(ref desc) = snapshot.description {
                println!("  Title: {}", desc);
            }
            if let (Some(bid), Some(ask)) = (snapshot.yes_bid, snapshot.yes_ask) {
                println!("  YES: Bid {:.3} / Ask {:.3}", bid, ask);
            }
            if let (Some(bid), Some(ask)) = (snapshot.no_bid, snapshot.no_ask) {
                println!("  NO:  Bid {:.3} / Ask {:.3}", bid, ask);
            }
            if let Some(mins) = snapshot.minutes_remaining {
                println!("  Time remaining: {} minutes", mins);
            }
            Some(snapshot)
        }
        Err(e) => {
            println!("\x1b[33m⚠ Could not fetch market data: {}\x1b[0m", e);
            None
        }
    };

    let context_provider = move || {
        let slug = market_slug_clone.clone();
        async move {
            let market_snapshot = match super::fetch_market_snapshot(&slug).await {
                Ok(snapshot) => snapshot,
                Err(_) => MarketSnapshot::new(slug),
            };

            let ctx = AgentContext::new(market_snapshot, StrategyState::Idle, RiskState::Normal)
                .with_daily_stats(DailyStats {
                    realized_pnl: Decimal::ZERO,
                    trade_count: 0,
                    cycle_count: 0,
                    win_rate: None,
                    avg_profit: None,
                });
            Ok(ctx)
        }
    };

    println!("\n\x1b[32mStarting autonomous agent...\x1b[0m");
    println!("Press Ctrl+C to stop.\n");

    let mut action_rx = agent.subscribe_actions();
    let action_logger = tokio::spawn(async move {
        while let Ok(action) = action_rx.recv().await {
            info!("Agent action: {:?}", action);
        }
    });

    tokio::select! {
        result = agent.run(context_provider) => {
            if let Err(e) = result {
                error!("Autonomous agent error: {}", e);
            }
        }
        _ = signal::ctrl_c() => {
            info!("Received shutdown signal");
            agent.shutdown().await;
        }
    }

    action_logger.abort();
    Ok(())
}
