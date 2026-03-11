use super::trades::{
    collect_trades_for_market, ensure_clob_trade_alerts_table, ensure_clob_trade_ticks_table,
    TradeAlertConfig, TradeAlertState,
};
use super::{env_i64, env_u64, env_usize};
use crate::platform::Domain;
use futures_util::StreamExt;
use polymarket_client_sdk::data::Client as DataApiClient;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

pub(crate) fn spawn_polymarket_trade_persistence_from_collector_targets(
    pool: PgPool,
    agent_id: String,
    domain: Domain,
) {
    tokio::spawn(async move {
        let agent_label = agent_id.clone();

        if let Err(e) = ensure_clob_trade_ticks_table(&pool).await {
            warn!(
                agent = agent_label,
                error = %e,
                "failed to ensure clob_trade_ticks table; trade persistence disabled"
            );
            return;
        }

        let data_client = Arc::new(DataApiClient::default());
        let poll_secs = env_u64("PM_TRADES_POLL_SECS", 10).max(1);
        let page_limit = env_usize("PM_TRADES_PAGE_LIMIT", 200).clamp(1, 1000);
        let max_pages = env_usize("PM_TRADES_MAX_PAGES", 10).clamp(1, 100);
        let overlap_secs = env_i64("PM_TRADES_OVERLAP_SECS", 120).max(0);
        let max_concurrency = env_usize("PM_TRADES_CONCURRENCY", 4).clamp(1, 32);
        let targets_limit = env_usize("PM_TRADES_TARGETS_LIMIT", 200).clamp(1, 5000);

        let mut alert_cfg = TradeAlertConfig::from_env();
        let mut alert_state: Option<Arc<tokio::sync::Mutex<TradeAlertState>>> =
            if alert_cfg.burst_enabled() {
                Some(Arc::new(
                    tokio::sync::Mutex::new(TradeAlertState::default()),
                ))
            } else {
                None
            };

        if alert_cfg.enabled() {
            if let Err(e) = ensure_clob_trade_alerts_table(&pool).await {
                warn!(
                    agent = agent_label,
                    error = %e,
                    "failed to ensure clob_trade_alerts table; trade alerting disabled"
                );
                alert_cfg = TradeAlertConfig::disabled();
                alert_state = None;
            }
        }

        let last_seen_by_market: Arc<tokio::sync::RwLock<HashMap<String, i64>>> =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        let mut tick = tokio::time::interval(Duration::from_secs(poll_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tick.tick().await;

            let markets: Vec<String> = match sqlx::query_scalar::<_, String>(
                r#"
                SELECT DISTINCT metadata->>'condition_id'
                FROM collector_token_targets
                WHERE domain = 'SPORTS_NBA'
                  AND target_date BETWEEN (CURRENT_DATE - 1) AND (CURRENT_DATE + 1)
                  AND (expires_at IS NULL OR expires_at > NOW())
                  AND (metadata ? 'condition_id')
                  AND COALESCE(metadata->>'condition_id','') <> ''
                ORDER BY 1
                LIMIT $1
                "#,
            )
            .bind(targets_limit as i64)
            .fetch_all(&pool)
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        agent = agent_label,
                        error = %e,
                        "failed to query sports trade targets from collector_token_targets"
                    );
                    continue;
                }
            };

            if markets.is_empty() {
                continue;
            }

            let domain_str = domain.to_string();
            let pool_ref = pool.clone();
            let data_client_ref = data_client.clone();
            let last_seen = last_seen_by_market.clone();
            let alert_cfg_ref = alert_cfg.clone();
            let alert_state_ref = alert_state.clone();

            futures_util::stream::iter(markets)
                .for_each_concurrent(max_concurrency, |condition_id| {
                    let pool = pool_ref.clone();
                    let data_client = data_client_ref.clone();
                    let domain = domain_str.clone();
                    let last_seen = last_seen.clone();
                    let alert_cfg = alert_cfg_ref.clone();
                    let alert_state = alert_state_ref.clone();
                    async move {
                        collect_trades_for_market(
                            data_client.as_ref(),
                            &pool,
                            &condition_id,
                            &domain,
                            page_limit,
                            max_pages,
                            overlap_secs,
                            &last_seen,
                            alert_cfg,
                            alert_state,
                        )
                        .await;
                    }
                })
                .await;
        }
    });
}
