use sqlx::PgPool;
use std::sync::Arc;
use tracing::warn;

use crate::adapters::{PolymarketClient, PolymarketWebSocket};
use crate::error::Result;
use crate::platform::PlatformDataPlane;
use crate::strategy::{DataFeed, DataFeedManager, StrategyManager};

pub(super) async fn ensure_managed_runtime_observability(
    strategy_label: &str,
    observability_pool: Option<&PgPool>,
    is_managed_staggered_arb_label: bool,
) {
    if !is_managed_staggered_arb_label {
        return;
    }

    if let Some(pool) = observability_pool {
        if let Err(error) = crate::persistence::ensure_strategy_observability_tables(pool).await {
            warn!(
                strategy = strategy_label,
                error = %error,
                "failed to ensure strategy observability tables for managed runtime"
            );
        }
    }
}

pub(super) fn build_managed_feed_manager(
    required_feeds: &[DataFeed],
    data_plane: Option<Arc<PlatformDataPlane>>,
    manager: Arc<StrategyManager>,
    pm_client: &PolymarketClient,
    pm_ws_url: &str,
) -> DataFeedManager {
    if let Some(data_plane) = data_plane {
        return DataFeedManager::from_data_plane(data_plane, manager).with_pm_client(pm_client.clone());
    }

    let mut binance_spot_symbols: Vec<String> = Vec::new();
    let mut binance_kline_symbols: Vec<String> = Vec::new();
    let mut binance_kline_intervals: Vec<String> = Vec::new();
    let mut binance_kline_closed_only = true;

    for feed in required_feeds {
        match feed {
            DataFeed::BinanceSpot { symbols } => {
                binance_spot_symbols.extend(symbols.clone());
            }
            DataFeed::BinanceKlines {
                symbols,
                intervals,
                closed_only,
            } => {
                binance_kline_symbols.extend(symbols.clone());
                binance_kline_intervals.extend(intervals.clone());
                if !*closed_only {
                    binance_kline_closed_only = false;
                }
            }
            _ => {}
        }
    }

    binance_spot_symbols.sort();
    binance_spot_symbols.dedup();
    binance_kline_symbols.sort();
    binance_kline_symbols.dedup();
    binance_kline_intervals.sort();
    binance_kline_intervals.dedup();

    let mut feed_manager = DataFeedManager::new(manager);
    if !binance_spot_symbols.is_empty() {
        feed_manager = feed_manager.with_binance(binance_spot_symbols);
    }

    if !binance_kline_symbols.is_empty() && !binance_kline_intervals.is_empty() {
        let backfill_limit = std::env::var("PLOY_BINANCE_KLINE_BACKFILL_LIMIT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(300);
        feed_manager = feed_manager.with_binance_klines(
            binance_kline_symbols,
            binance_kline_intervals,
            binance_kline_closed_only,
            backfill_limit,
        );
    }

    let has_polymarket_feed = required_feeds.iter().any(|feed| {
        matches!(
            feed,
            DataFeed::PolymarketEvents { .. } | DataFeed::PolymarketQuotes { .. }
        )
    });
    if has_polymarket_feed {
        let pm_ws = PolymarketWebSocket::new(pm_ws_url);
        feed_manager = feed_manager.with_polymarket(pm_ws, pm_client.clone());
    }

    feed_manager
}

pub(super) async fn start_account_claimer_daemon_if_needed(
    strategy_label: &str,
    agent_id: &str,
    dry_run: bool,
) -> Result<()> {
    #[cfg(feature = "claimer_daemon")]
    {
        if !dry_run {
            if let Err(error) = crate::strategy::ensure_account_claimer_daemon().await {
                warn!(
                    strategy = strategy_label,
                    agent_id = agent_id,
                    error = %error,
                    "failed to start account-level auto-claimer daemon"
                );
            }
        }
    }

    #[cfg(not(feature = "claimer_daemon"))]
    let _ = (strategy_label, agent_id, dry_run);

    Ok(())
}
