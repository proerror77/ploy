use super::alerts::{TradeAlertConfig, TradeAlertState, ensure_clob_trade_alerts_table};
use super::trades::{collect_trades_for_market, ensure_clob_trade_ticks_table};
use super::{env_i64, env_u64, env_usize};
use crate::domain::Domain;
use crate::strategy::momentum::EventMatcher;
use chrono::Utc;
use futures_util::StreamExt;
use polymarket_client_sdk::data::Client as DataApiClient;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

#[derive(Clone, Copy, Debug)]
struct TradePersistenceRuntimeConfig {
    poll_secs: u64,
    page_limit: usize,
    max_pages: usize,
    overlap_secs: i64,
    max_concurrency: usize,
    end_grace_secs: i64,
    min_remaining_for_collection: i64,
}

impl TradePersistenceRuntimeConfig {
    fn from_env() -> Self {
        Self {
            poll_secs: env_u64("PM_TRADES_POLL_SECS", 10).max(1),
            page_limit: env_usize("PM_TRADES_PAGE_LIMIT", 200).clamp(1, 1000),
            max_pages: env_usize("PM_TRADES_MAX_PAGES", 10).clamp(1, 100),
            overlap_secs: env_i64("PM_TRADES_OVERLAP_SECS", 120).max(0),
            max_concurrency: env_usize("PM_TRADES_CONCURRENCY", 4).clamp(1, 32),
            end_grace_secs: env_i64("PM_TRADES_END_GRACE_SECS", 600).max(0),
            min_remaining_for_collection: env_i64("PM_TRADES_MIN_REMAINING_SECS", 0)
                .max(-86400)
                .min(86400),
        }
    }
}

#[derive(Clone)]
struct TradePersistenceRuntimeState {
    data_client: Arc<DataApiClient>,
    alert_cfg: TradeAlertConfig,
    alert_state: Option<Arc<tokio::sync::Mutex<TradeAlertState>>>,
    last_seen_by_market: Arc<tokio::sync::RwLock<HashMap<String, i64>>>,
}

async fn build_trade_persistence_runtime(
    pool: &PgPool,
    agent_label: &str,
) -> Option<TradePersistenceRuntimeState> {
    if let Err(e) = ensure_clob_trade_ticks_table(pool).await {
        warn!(
            agent = agent_label,
            error = %e,
            "failed to ensure clob_trade_ticks table; trade persistence disabled"
        );
        return None;
    }

    let mut alert_cfg = TradeAlertConfig::from_env();
    let mut alert_state = if alert_cfg.burst_enabled() {
        Some(Arc::new(
            tokio::sync::Mutex::new(TradeAlertState::default()),
        ))
    } else {
        None
    };

    if alert_cfg.enabled() {
        if let Err(e) = ensure_clob_trade_alerts_table(pool).await {
            warn!(
                agent = agent_label,
                error = %e,
                "failed to ensure clob_trade_alerts table; trade alerting disabled"
            );
            alert_cfg = TradeAlertConfig::disabled();
            alert_state = None;
        }
    }

    Some(TradePersistenceRuntimeState {
        data_client: Arc::new(DataApiClient::default()),
        alert_cfg,
        alert_state,
        last_seen_by_market: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
    })
}

async fn refresh_tracked_markets(
    event_matcher: &EventMatcher,
    coins: &[String],
    min_remaining_for_collection: i64,
    end_grace_secs: i64,
    tracked_markets: &mut HashMap<String, i64>,
) {
    for coin in coins {
        let symbol = format!("{}USDT", coin.to_uppercase());
        for event in event_matcher
            .get_events_with_min_remaining(&symbol, min_remaining_for_collection)
            .await
        {
            let condition_id = event.condition_id.trim();
            if condition_id.is_empty() {
                continue;
            }
            let expires_at = event.end_time.timestamp().saturating_add(end_grace_secs);
            tracked_markets.insert(condition_id.to_string(), expires_at);
        }
    }
}

async fn collect_tracked_markets(
    markets: Vec<String>,
    pool: PgPool,
    domain: String,
    runtime: &TradePersistenceRuntimeState,
    config: TradePersistenceRuntimeConfig,
) {
    let data_client = runtime.data_client.clone();
    let last_seen = runtime.last_seen_by_market.clone();
    let alert_cfg = runtime.alert_cfg.clone();
    let alert_state = runtime.alert_state.clone();

    futures_util::stream::iter(markets)
        .for_each_concurrent(config.max_concurrency, |condition_id| {
            let pool = pool.clone();
            let domain = domain.clone();
            let data_client = data_client.clone();
            let last_seen = last_seen.clone();
            let alert_cfg = alert_cfg.clone();
            let alert_state = alert_state.clone();
            async move {
                collect_trades_for_market(
                    data_client.as_ref(),
                    &pool,
                    &condition_id,
                    &domain,
                    config.page_limit,
                    config.max_pages,
                    config.overlap_secs,
                    &last_seen,
                    alert_cfg,
                    alert_state,
                )
                .await;
            }
        })
        .await;
}

async fn run_event_matcher_trade_persistence(
    event_matcher: Arc<EventMatcher>,
    pool: PgPool,
    coins: Vec<String>,
    domain: Domain,
    runtime: TradePersistenceRuntimeState,
    config: TradePersistenceRuntimeConfig,
) {
    let mut tracked_markets: HashMap<String, i64> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(config.poll_secs));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tick.tick().await;
        let now_unix = Utc::now().timestamp();
        refresh_tracked_markets(
            event_matcher.as_ref(),
            &coins,
            config.min_remaining_for_collection,
            config.end_grace_secs,
            &mut tracked_markets,
        )
        .await;

        tracked_markets.retain(|_, expires_at| *expires_at >= now_unix);
        let mut markets: Vec<String> = tracked_markets.keys().cloned().collect();
        markets.sort();
        if markets.is_empty() {
            continue;
        }

        collect_tracked_markets(markets, pool.clone(), domain.to_string(), &runtime, config).await;
    }
}

pub(crate) fn spawn_polymarket_trade_persistence(
    event_matcher: Arc<EventMatcher>,
    pool: PgPool,
    agent_id: String,
    coins: Vec<String>,
    domain: Domain,
) {
    tokio::spawn(async move {
        let Some(runtime) = build_trade_persistence_runtime(&pool, &agent_id).await else {
            return;
        };
        run_event_matcher_trade_persistence(
            event_matcher,
            pool,
            coins,
            domain,
            runtime,
            TradePersistenceRuntimeConfig::from_env(),
        )
        .await;
    });
}
