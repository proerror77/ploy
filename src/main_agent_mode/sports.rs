use ploy::error::Result;

pub(super) async fn run_sports_mode(sports_url: Option<&str>) -> Result<()> {
    use ploy::ai_clients::sports_analyst::TradeAction;
    use ploy::ai_clients::SportsAnalyst;

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

    let analyst = match SportsAnalyst::from_env() {
        Ok(a) => a,
        Err(e) => {
            println!("\x1b[31mFailed to initialize sports analyst: {}\x1b[0m", e);
            println!("Make sure GROK_API_KEY is set in your environment");
            return Ok(());
        }
    };
    println!("\x1b[32m✓ Grok + Claude initialized\x1b[0m\n");

    println!("\x1b[36mAnalyzing event...\x1b[0m");
    match analyst.analyze_event(&event_url).await {
        Ok(analysis) => {
            println!(
                "\n\x1b[33m═══════════════════════════════════════════════════════════════\x1b[0m"
            );
            println!(
                "\x1b[33m                    SPORTS ANALYSIS RESULTS                     \x1b[0m"
            );
            println!(
                "\x1b[33m═══════════════════════════════════════════════════════════════\x1b[0m\n"
            );

            println!(
                "\x1b[36mMatchup:\x1b[0m {} vs {}",
                analysis.teams.0, analysis.teams.1
            );

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

            println!(
                "\n\x1b[33m═══════════════════════════════════════════════════════════════\x1b[0m"
            );
        }
        Err(e) => {
            println!("\x1b[31mAnalysis failed: {}\x1b[0m", e);
        }
    }

    Ok(())
}
