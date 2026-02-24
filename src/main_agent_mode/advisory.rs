use ploy::error::Result;

pub(super) async fn run_advisory_mode(market: Option<&str>, chat: bool) -> Result<()> {
    use ploy::agent::{protocol::MarketSnapshot, AdvisoryAgent, ClaudeAgentClient};
    use std::io::{self, BufRead, Write};

    let client = ClaudeAgentClient::new();
    let advisor = AdvisoryAgent::new(client);

    if chat {
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
        return Ok(());
    }

    if let Some(market_id) = market {
        println!("\nAnalyzing market: {}", market_id);
        println!("Fetching market data from Polymarket...");

        let market_snapshot = match super::fetch_market_snapshot(market_id).await {
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

        return Ok(());
    }

    println!("\nUsage:");
    println!("  ploy agent --mode advisory --market <EVENT_ID>  # Analyze a market");
    println!("  ploy agent --mode advisory --chat               # Interactive chat");
    Ok(())
}
