use ploy::error::Result;
use tracing::info;

mod advisory;
mod autonomous;
mod sports;

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
    use ploy::agent::ClaudeAgentClient;

    println!("\x1b[36m");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║           PLOY - Claude AI Trading Assistant                 ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("\x1b[0m");

    let check_client = ClaudeAgentClient::new();
    if !check_client.check_availability().await? {
        println!("\x1b[31m✗ Claude CLI not found. Please install it first:\x1b[0m");
        println!("  npm install -g @anthropic-ai/claude-code");
        return Ok(());
    }
    println!("\x1b[32m✓ Claude CLI available\x1b[0m");

    match mode {
        "advisory" => advisory::run_advisory_mode(market, chat).await?,
        "autonomous" => {
            autonomous::run_autonomous_mode(market, max_trade, max_exposure, enable_trading).await?
        }
        "sports" => sports::run_sports_mode(sports_url).await?,
        _ => {
            println!("\x1b[31mUnknown mode: {}\x1b[0m", mode);
            println!("Available modes: advisory, autonomous, sports");
        }
    }

    info!("Agent mode completed");
    Ok(())
}

/// Fetch market data from Polymarket and create a populated MarketSnapshot
async fn fetch_market_snapshot(market_slug: &str) -> Result<ploy::agent::protocol::MarketSnapshot> {
    use chrono::Utc;
    use ploy::adapters::polymarket_clob::GAMMA_API_URL;
    use ploy::agent::protocol::MarketSnapshot;
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

    snapshot.description = event.title.clone();

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

    if let Some(markets) = event.markets.as_ref() {
        let mut sum_yes_asks = Decimal::ZERO;
        let mut sum_no_bids = Decimal::ZERO;
        let mut first_market = true;

        for market in markets {
            let clob_token_ids = parse_json_array(market.clob_token_ids.as_deref());
            let outcome_prices = parse_json_array(market.outcome_prices.as_deref());

            if clob_token_ids.len() >= 2 && outcome_prices.len() >= 2 {
                let yes_price = Decimal::from_str(&outcome_prices[0]).ok();
                let no_price = Decimal::from_str(&outcome_prices[1]).ok();

                if first_market {
                    snapshot.yes_token_id = Some(clob_token_ids[0].clone());
                    snapshot.no_token_id = Some(clob_token_ids[1].clone());
                    snapshot.yes_bid = yes_price;
                    snapshot.yes_ask = yes_price;
                    snapshot.no_bid = no_price;
                    snapshot.no_ask = no_price;

                    first_market = false;
                }

                if let Some(price) = yes_price {
                    sum_yes_asks += price;
                }
                if let Some(price) = no_price {
                    sum_no_bids += price;
                }
            }
        }

        if sum_yes_asks > Decimal::ZERO {
            snapshot.sum_asks = Some(sum_yes_asks);
        }
        if sum_no_bids > Decimal::ZERO {
            snapshot.sum_bids = Some(sum_no_bids);
        }
    }

    Ok(snapshot)
}
