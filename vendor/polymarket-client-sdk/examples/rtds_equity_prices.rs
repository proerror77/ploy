//! RTDS `equity_prices` example for Pyth-backed reference feeds.
//!
//! Logs both the initial subscribe snapshot and subsequent live updates for
//! two symbols across asset classes. Payload symbols are always lowercase.
//!
//! Run with tracing enabled:
//! ```sh
//! RUST_LOG=info cargo run --example rtds_equity_prices --features rtds,tracing
//! ```

use std::time::Duration;

use futures::StreamExt as _;
use polymarket_client_sdk::rtds::{Client, EquityPriceMessage};
use tokio::time::timeout;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    for symbol in ["AAPL", "XAUUSD"] {
        let client = Client::default();
        let stream = client.subscribe_equity_prices(Some(symbol.to_string()), true)?;
        let mut stream = Box::pin(stream);
        let mut seen = 0_u64;

        info!(symbol, "Subscribed to equity_prices");

        while let Ok(Some(result)) = timeout(Duration::from_secs(5), stream.next()).await {
            match result? {
                EquityPriceMessage::Snapshot(snapshot) => {
                    info!(
                        symbol = %snapshot.symbol,
                        points = snapshot.data.len(),
                        "Received initial equity_prices snapshot"
                    );
                }
                EquityPriceMessage::Update(update) => {
                    info!(
                        symbol = %update.symbol,
                        value = %update.value,
                        full_accuracy_value = %update.full_accuracy_value,
                        carried_forward = update.is_carried_forward,
                        "Received live equity_prices update"
                    );
                    seen += 1;
                    if seen >= 3 {
                        break;
                    }
                }
                _ => {}
            }
        }

        if seen == 0 {
            warn!(
                symbol,
                "No live equity_prices updates received within timeout"
            );
        }
    }

    Ok(())
}
