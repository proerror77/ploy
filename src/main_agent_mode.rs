use ploy::error::Result;
use tokio::signal;
use tracing::{error, info};

/// Claude AI agent mode for trading assistance
pub(crate) async fn run_agent_mode(
    mode: &str,
    market: Option<&str>,
    sports_url: Option<&str>,
    max_trade: f64,
    max_exposure: f64,
    enable_trading: bool,
    chat: bool,
) -> Result<()> {
    use ploy::ai_agents::autonomous::AutonomyLevel;
    use ploy::ai_agents::{
        protocol::{AgentContext, DailyStats, MarketSnapshot},
        sports_analyst::TradeAction,
        AdvisoryAgent, AutonomousAgent, AutonomousConfig, ClaudeAgentClient, SportsAnalyst,
    };
    use ploy::domain::{RiskState, StrategyState};
    use rust_decimal::Decimal;
    use std::io::{self, BufRead, Write};

    println!("\x1b[36m");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║           PLOY - Claude AI Trading Assistant                 ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("\x1b[0m");

    // Check claude CLI availability
    let check_client = ClaudeAgentClient::new();
    if !check_client.check_availability().await? {
        println!("\x1b[31m✗ Claude CLI not found. Please install it first:\x1b[0m");
        println!("  npm install -g @anthropic-ai/claude-code");
        return Ok(());
    }
    println!("\x1b[32m✓ Claude CLI available\x1b[0m");

    match mode {
        "advisory" => {
            let client = ClaudeAgentClient::new(); // Default 2-minute timeout
            let advisor = AdvisoryAgent::new(client);

            if chat {
                // Interactive chat mode
                println!("\n\x1b[33mInteractive Chat Mode\x1b[0m");
                println!("Type your questions, or 'exit' to quit.\n");

                let stdin = io::stdin();
                loop {
                    print!("\x1b[36mYou:\x1b[0m ");
                    io::stdout().flush()?;

                    let mut line = String::new();
                    stdin.lock().read_line(&mut line)?;
                    let line = line.trim();

                    if line.eq_ignore_ascii_case("exit") || line.eq_ignore_ascii_case("quit") {
                        break;
                    }

                    if line.is_empty() {
                        continue;
                    }

                    println!("\x1b[33mClaude:\x1b[0m Analyzing...");

                    match advisor.chat(line, None).await {
                        Ok(response) => {
                            println!("\n{}\n", response);
                        }
                        Err(e) => {
                            println!("\x1b[31mError: {}\x1b[0m\n", e);
                        }
                    }
                }
            } else if let Some(market_id) = market {
                // Analyze specific market
                println!("\nAnalyzing market: {}", market_id);
                println!("Fetching market data from Polymarket...");

                // Fetch market data and populate snapshot
                let market_snapshot = match fetch_market_snapshot(market_id).await {
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
                        snapshot
                    }
                    Err(e) => {
                        println!("\x1b[33m⚠ Could not fetch market data: {}\x1b[0m", e);
                        println!("  Proceeding with limited analysis...");
                        MarketSnapshot::new(market_id.to_string())
                    }
                };

                match advisor.analyze_market(&market_snapshot).await {
                    Ok(response) => {
                        println!("\n\x1b[33m=== Analysis Results ===\x1b[0m\n");
                        println!("Confidence: {:.0}%", response.confidence * 100.0);
                        println!("\nReasoning:\n{}", response.reasoning);
                        println!("\nRecommended Actions:");
                        for action in &response.recommended_actions {
                            println!("  • {:?}", action);
                        }
                        println!("\nSummary: {}", response.summary);
                    }
                    Err(e) => {
                        println!("\x1b[31mAnalysis failed: {}\x1b[0m", e);
                    }
                }
            } else {
                println!("\nUsage:");
                println!("  ploy agent --mode advisory --market <EVENT_ID>  # Analyze a market");
                println!("  ploy agent --mode advisory --chat               # Interactive chat");
            }
        }
        "autonomous" => {
            let config = AutonomousConfig {
                autonomy_level: if enable_trading {
                    AutonomyLevel::LimitedAutonomy
                } else {
                    AutonomyLevel::AdvisoryOnly
                },
                max_trade_size: Decimal::from_f64_retain(max_trade).unwrap_or(Decimal::from(50)),
                max_total_exposure: Decimal::from_f64_retain(max_exposure)
                    .unwrap_or(Decimal::from(200)),
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
                println!("\n\x1b[33m⚠️  Trading is disabled. Use --enable-trading to execute trades.\x1b[0m");
            }

            // Use longer timeout for autonomous mode (3 minutes)
            use ploy::ai_agents::AgentClientConfig;
            let client = ClaudeAgentClient::with_config(AgentClientConfig::for_autonomous());
            let mut agent = AutonomousAgent::new(client, config);

            if enable_trading {
                println!("  Trading backend: initializing authenticated Polymarket client...");
                let trading_client =
                    crate::create_pm_client("https://clob.polymarket.com", false).await?;
                println!("  Trading backend: \x1b[32m✓ Ready (live orders enabled)\x1b[0m");
                agent = agent.with_trading_client(trading_client);
            } else {
                println!("  Trading backend: \x1b[33mdisabled\x1b[0m");
            }

            // Add Grok for real-time search if configured
            use ploy::ai_agents::{GrokClient, GrokConfig};
            if let Ok(grok) = GrokClient::new(GrokConfig::from_env()) {
                if grok.is_configured() {
                    println!("  Grok: \x1b[32m✓ Enabled\x1b[0m (real-time market intelligence)");
                    agent = agent.with_grok(grok);
                } else {
                    println!("  Grok: \x1b[33m⚠ Not configured\x1b[0m (set GROK_API_KEY for real-time search)");
                }
            }

            // Get market slug for context provider
            let market_slug = market
                .map(|s| s.to_string())
                .unwrap_or_else(|| "demo-market".to_string());
            let market_slug_clone = market_slug.clone();

            // Fetch initial market data
            println!("\nFetching market data for: {}", market_slug);
            let _initial_snapshot = match fetch_market_snapshot(&market_slug).await {
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

            // Context provider that fetches fresh market data each cycle
            let context_provider = move || {
                let slug = market_slug_clone.clone();
                async move {
                    // Fetch fresh market data
                    let market_snapshot = match fetch_market_snapshot(&slug).await {
                        Ok(snapshot) => snapshot,
                        Err(_) => MarketSnapshot::new(slug),
                    };

                    let ctx =
                        AgentContext::new(market_snapshot, StrategyState::Idle, RiskState::Normal)
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

            // Subscribe to actions for logging
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
        }
        "sports" => {
            // Sports event analysis mode: Grok (player data + sentiment) -> Claude Opus (prediction)
            let event_url = match sports_url {
                Some(url) => url.to_string(),
                None => {
                    println!("\x1b[31mError: --sports-url is required for sports mode\x1b[0m");
                    println!("Example: ploy agent --mode sports --sports-url https://polymarket.com/event/nba-phi-dal-2026-01-01");
                    return Ok(());
                }
            };

            println!("\n\x1b[33mSports Analysis Mode\x1b[0m");
            println!("Event URL: {}", event_url);
            println!("\nWorkflow:");
            println!("  1. Fetch market odds from Polymarket");
            println!("  2. Search player stats & injuries (Grok)");
            println!("  3. Analyze public sentiment (Grok)");
            println!("  4. Predict win probability (Claude Opus)");
            println!("  5. Generate trade recommendation\n");

            // Create sports analyst
            let analyst = match SportsAnalyst::from_env() {
                Ok(a) => a,
                Err(e) => {
                    println!("\x1b[31mFailed to initialize sports analyst: {}\x1b[0m", e);
                    println!("Make sure GROK_API_KEY is set in your environment");
                    return Ok(());
                }
            };
            println!("\x1b[32m✓ Grok + Claude initialized\x1b[0m\n");

            // Run analysis
            println!("\x1b[36mAnalyzing event...\x1b[0m");
            match analyst.analyze_event(&event_url).await {
                Ok(analysis) => {
                    println!("\n\x1b[33m═══════════════════════════════════════════════════════════════\x1b[0m");
                    println!("\x1b[33m                    SPORTS ANALYSIS RESULTS                     \x1b[0m");
                    println!("\x1b[33m═══════════════════════════════════════════════════════════════\x1b[0m\n");

                    // Teams
                    println!(
                        "\x1b[36mMatchup:\x1b[0m {} vs {}",
                        analysis.teams.0, analysis.teams.1
                    );

                    // Market odds
                    println!("\n\x1b[36mMarket Odds (Polymarket):\x1b[0m");
                    println!(
                        "  {} YES: {:.1}%",
                        analysis.teams.0,
                        analysis
                            .market_odds
                            .team1_yes_price
                            .to_string()
                            .parse::<f64>()
                            .unwrap_or(0.0)
                            * 100.0
                    );
                    if let Some(p) = analysis.market_odds.team2_yes_price {
                        println!(
                            "  {} YES: {:.1}%",
                            analysis.teams.1,
                            p.to_string().parse::<f64>().unwrap_or(0.0) * 100.0
                        );
                    }

                    // Structured data (from Grok)
                    if let Some(ref data) = analysis.structured_data {
                        let sentiment = &data.sentiment;
                        println!("\n\x1b[36mPublic Sentiment (Grok):\x1b[0m");
                        println!("  Expert pick: {}", sentiment.expert_pick);
                        println!(
                            "  Expert confidence: {:.0}%",
                            sentiment.expert_confidence * 100.0
                        );
                        println!("  Public bet: {:.0}%", sentiment.public_bet_percentage);
                        println!("  Sharp money: {}", sentiment.sharp_money_side);
                        println!("  Social sentiment: {}", sentiment.social_sentiment);
                        if !sentiment.key_narratives.is_empty() {
                            println!("  Key narratives:");
                            for narrative in sentiment.key_narratives.iter().take(3) {
                                println!("    • {}", narrative);
                            }
                        }
                    }

                    // Claude prediction
                    println!("\n\x1b[36mClaude Opus Prediction:\x1b[0m");
                    println!(
                        "  {} win probability: \x1b[32m{:.1}%\x1b[0m",
                        analysis.teams.0,
                        analysis.prediction.team1_win_prob * 100.0
                    );
                    println!(
                        "  {} win probability: \x1b[32m{:.1}%\x1b[0m",
                        analysis.teams.1,
                        analysis.prediction.team2_win_prob * 100.0
                    );
                    println!(
                        "  Confidence: {:.0}%",
                        analysis.prediction.confidence * 100.0
                    );
                    println!("\n  Reasoning: {}", analysis.prediction.reasoning);
                    if !analysis.prediction.key_factors.is_empty() {
                        println!("\n  Key factors:");
                        for factor in &analysis.prediction.key_factors {
                            println!("    • {}", factor);
                        }
                    }

                    // Trade recommendation
                    println!("\n\x1b[36mTrade Recommendation:\x1b[0m");
                    let action_color = match analysis.recommendation.action {
                        TradeAction::Buy => "\x1b[32m",
                        TradeAction::Sell => "\x1b[31m",
                        TradeAction::Hold => "\x1b[33m",
                        TradeAction::Avoid => "\x1b[31m",
                    };
                    println!(
                        "  Action: {}{:?}\x1b[0m",
                        action_color, analysis.recommendation.action
                    );
                    println!("  Side: {}", analysis.recommendation.side);
                    println!("  Edge: {:.1}%", analysis.recommendation.edge);
                    println!(
                        "  Suggested size: {}% of bankroll",
                        analysis.recommendation.suggested_size
                    );
                    println!("  Reasoning: {}", analysis.recommendation.reasoning);

                    println!("\n\x1b[33m═══════════════════════════════════════════════════════════════\x1b[0m");
                }
                Err(e) => {
                    println!("\x1b[31mAnalysis failed: {}\x1b[0m", e);
                }
            }
        }
        _ => {
            println!("\x1b[31mUnknown mode: {}\x1b[0m", mode);
            println!("Available modes: advisory, autonomous, sports");
        }
    }

    info!("Agent mode completed");
    Ok(())
}

/// Fetch market data from Polymarket and create a populated MarketSnapshot
async fn fetch_market_snapshot(
    market_slug: &str,
) -> Result<ploy::ai_agents::protocol::MarketSnapshot> {
    use chrono::Utc;
    use ploy::adapters::polymarket_clob::GAMMA_API_URL;
    use ploy::ai_agents::protocol::MarketSnapshot;
    use ploy::error::PloyError;
    use polymarket_client_sdk::gamma::types::request::SearchRequest;
    use polymarket_client_sdk::gamma::Client as GammaClient;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    let gamma = GammaClient::new(GAMMA_API_URL).map_err(|e| {
        PloyError::MarketDataUnavailable(format!("Failed to create Gamma client: {}", e))
    })?;
    let req = SearchRequest::builder().q(market_slug).build();
    let search = gamma.search(&req).await.map_err(|e| {
        PloyError::MarketDataUnavailable(format!("Gamma search failed for {}: {}", market_slug, e))
    })?;
    let events = search.events.unwrap_or_default();
    let normalized = market_slug.trim_matches('/');
    let event = events
        .iter()
        .find(|e| {
            e.slug.as_deref().is_some_and(|slug| {
                let slug = slug.trim_matches('/');
                slug == normalized || slug.ends_with(&format!("/{}", normalized))
            })
        })
        .cloned()
        .or_else(|| events.into_iter().next())
        .ok_or_else(|| {
            PloyError::MarketDataUnavailable(format!("No event found for slug: {}", market_slug))
        })?;

    let mut snapshot = MarketSnapshot::new(market_slug.to_string());

    // Set description from event title
    snapshot.description = event.title.clone();

    // Parse end date
    if let Some(end_utc) = event.end_date {
        snapshot.end_time = Some(end_utc);
        let now = Utc::now();
        let duration = end_utc.signed_duration_since(now);
        snapshot.minutes_remaining = Some(duration.num_minutes());
    }

    let parse_json_array = |raw: Option<&str>| -> Vec<String> {
        let Some(raw) = raw else { return vec![] };
        if let Ok(v) = serde_json::from_str::<Vec<String>>(raw) {
            return v;
        }
        if let Ok(v) = serde_json::from_str::<Vec<serde_json::Value>>(raw) {
            return v
                .into_iter()
                .map(|x| {
                    x.as_str()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| x.to_string())
                })
                .collect();
        }
        vec![]
    };

    // Get markets from the event
    if let Some(markets) = event.markets.as_ref() {
        let mut sum_yes_asks = Decimal::ZERO;
        let mut sum_no_bids = Decimal::ZERO;
        let mut first_market = true;

        for market in markets {
            // Get CLOB token IDs
            let clob_token_ids = parse_json_array(market.clob_token_ids.as_deref());

            // Get outcome prices from market data
            let outcome_prices = parse_json_array(market.outcome_prices.as_deref());

            if clob_token_ids.len() >= 2 && outcome_prices.len() >= 2 {
                // Parse YES price (first token)
                let yes_price = Decimal::from_str(&outcome_prices[0]).ok();
                // Parse NO price (second token)
                let no_price = Decimal::from_str(&outcome_prices[1]).ok();

                // For first market, set as primary prices
                if first_market {
                    snapshot.yes_token_id = Some(clob_token_ids[0].clone());
                    snapshot.no_token_id = Some(clob_token_ids[1].clone());
                    // Use outcome prices as approximate bid/ask
                    snapshot.yes_bid = yes_price;
                    snapshot.yes_ask = yes_price;
                    snapshot.no_bid = no_price;
                    snapshot.no_ask = no_price;

                    first_market = false;
                }

                // Accumulate for sum calculations (multi-outcome markets)
                if let Some(price) = yes_price {
                    sum_yes_asks += price;
                }
                if let Some(price) = no_price {
                    sum_no_bids += price;
                }
            }
        }

        // Set sum values for arbitrage detection
        if sum_yes_asks > Decimal::ZERO {
            snapshot.sum_asks = Some(sum_yes_asks);
        }
        if sum_no_bids > Decimal::ZERO {
            snapshot.sum_bids = Some(sum_no_bids);
        }
    }

    Ok(snapshot)
}
