//! Integration test for Grok + Autonomous Agent
//!
//! Run with: cargo run --example test_grok_agent

use ploy::agent::{
    AutonomousAgent, AutonomousConfig, ClaudeAgentClient,
    GrokClient, GrokConfig,
};

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    println!("=== Testing Grok + Autonomous Agent Integration ===\n");

    // 1. Test Grok client directly
    println!("1. Testing Grok client...");
    let grok_config = GrokConfig::from_env();

    if !grok_config.is_configured() {
        println!("   ERROR: GROK_API_KEY not set in environment");
        println!("   Set it with: export GROK_API_KEY=your_key");
        return;
    }

    println!("   Model: {}", grok_config.model);
    println!("   Base URL: {}", grok_config.base_url);

    let grok = match GrokClient::new(grok_config) {
        Ok(g) => g,
        Err(e) => {
            println!("   ERROR: Failed to create Grok client: {}", e);
            return;
        }
    };

    // 2. Test search functionality
    println!("\n2. Testing Grok search (Bitcoin sentiment)...");
    match grok.search_market("Bitcoin", "1 hour").await {
        Ok(result) => {
            println!("   Query: {}", result.query);
            println!("   Sentiment: {:?}", result.sentiment);
            println!("   Key points: {}", result.key_points.len());
            for (i, point) in result.key_points.iter().take(3).enumerate() {
                println!("     {}. {}", i + 1, point);
            }
            println!("\n   Summary (first 200 chars):");
            let summary: String = result.summary.chars().take(200).collect();
            println!("   {}", summary);
        }
        Err(e) => {
            println!("   ERROR: Search failed: {}", e);
        }
    }

    // 3. Test sentiment analysis
    println!("\n3. Testing sentiment analysis (Ethereum)...");
    match grok.analyze_sentiment("Ethereum ETH").await {
        Ok(sentiment) => {
            println!("   Sentiment: {}", sentiment);
        }
        Err(e) => {
            println!("   ERROR: Sentiment analysis failed: {}", e);
        }
    }

    // 4. Test autonomous agent with Grok
    println!("\n4. Testing Autonomous Agent with Grok integration...");
    let claude_client = ClaudeAgentClient::new();
    let config = AutonomousConfig::conservative();

    // Recreate Grok client for agent
    let grok_for_agent = GrokClient::new(GrokConfig::from_env()).unwrap();

    let agent = AutonomousAgent::new(claude_client, config)
        .with_grok(grok_for_agent);

    println!("   Grok available: {}", agent.has_grok());
    println!("   Can trade: {}", agent.can_trade());
    println!("   Current exposure: ${}", agent.current_exposure().await);

    // 5. Test fetching realtime context
    println!("\n5. Testing realtime context fetch...");
    if let Some(context) = agent.fetch_realtime_context("bitcoin-btc").await {
        println!("   Got realtime context!");
        println!("   Sentiment: {:?}", context.sentiment);
        println!("   Key points: {}", context.key_points.len());
    } else {
        println!("   No realtime context (Grok may not be configured)");
    }

    println!("\n=== Integration Test Complete ===");
}
