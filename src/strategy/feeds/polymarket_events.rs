use super::{DataFeedManager, MAX_EVENTS_PER_SERIES, POLYMARKET_REFRESH_SECS};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::adapters::{polymarket_clob::GammaEventInfo, PolymarketClient, PolymarketWebSocket};
use crate::domain::Side;
use crate::error::Result;
use crate::strategy::manager::StrategyManager;
use crate::strategy::traits::MarketUpdate;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(super) struct EventMapping {
    pub(super) event_id: String,
    pub(super) series_id: String,
    pub(super) is_up_token: bool,
}

#[derive(Debug, Clone)]
pub(super) struct DiscoveredEvent {
    pub(super) event_id: String,
    pub(super) series_id: String,
    pub(super) up_token: String,
    pub(super) down_token: String,
    pub(super) end_time: DateTime<Utc>,
    pub(super) price_to_beat: Option<rust_decimal::Decimal>,
    pub(super) title: Option<String>,
    pub(super) condition_id: Option<String>,
}

struct SeriesDiscoveryBatch {
    total_events: usize,
    discovered: HashMap<String, DiscoveredEvent>,
}

fn infer_symbol_from_text(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("bitcoin") || lower.contains("btc") {
        Some("BTCUSDT")
    } else if lower.contains("ethereum") || lower.contains("eth") {
        Some("ETHUSDT")
    } else if lower.contains("solana") || lower.contains("sol") {
        Some("SOLUSDT")
    } else if lower.contains("ripple") || lower.contains("xrp") {
        Some("XRPUSDT")
    } else {
        None
    }
}

fn infer_horizon_from_text(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("15m")
        || lower.contains("15-minute")
        || lower.contains("15 minute")
        || lower.contains("15min")
        || lower.contains("15 min")
    {
        Some("15m")
    } else if lower.contains("5m")
        || lower.contains("5-minute")
        || lower.contains("5 minute")
        || lower.contains("5min")
        || lower.contains("5 min")
    {
        Some("5m")
    } else {
        None
    }
}

fn apply_dimension_candidate(
    text: &str,
    symbol: &mut Option<String>,
    horizon: &mut Option<String>,
) {
    if symbol.is_none() {
        if let Some(symbol_candidate) = infer_symbol_from_text(text) {
            *symbol = Some(symbol_candidate.to_string());
        }
    }

    if horizon.is_none() {
        if let Some(horizon_candidate) = infer_horizon_from_text(text) {
            *horizon = Some(horizon_candidate.to_string());
        }
    }
}

fn infer_symbol_horizon_from_event(details: &GammaEventInfo) -> (Option<String>, Option<String>) {
    let mut symbol: Option<String> = None;
    let mut horizon: Option<String> = None;

    if let Some(slug) = details.slug.as_deref() {
        apply_dimension_candidate(slug, &mut symbol, &mut horizon);
    }
    if let Some(title) = details.title.as_deref() {
        apply_dimension_candidate(title, &mut symbol, &mut horizon);
    }

    for market in &details.markets {
        if let Some(group_title) = market.group_item_title.as_deref() {
            apply_dimension_candidate(group_title, &mut symbol, &mut horizon);
        }
        if let Some(question) = market.question.as_deref() {
            apply_dimension_candidate(question, &mut symbol, &mut horizon);
        }
        if symbol.is_some() && horizon.is_some() {
            break;
        }
    }

    (symbol, horizon)
}

fn parse_rfc3339_utc(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

async fn upsert_pm_market_metadata(
    pool: Option<&PgPool>,
    details: &GammaEventInfo,
    price_to_beat: Option<rust_decimal::Decimal>,
    end_time: DateTime<Utc>,
) -> Result<()> {
    let Some(pool) = pool else {
        return Ok(());
    };

    let market_slug = details.slug.clone().unwrap_or_else(|| details.id.clone());
    let start_time = parse_rfc3339_utc(details.start_time.as_deref());
    let (symbol, horizon) = infer_symbol_horizon_from_event(details);
    let raw_market: Value = serde_json::to_value(details).unwrap_or_else(|_| Value::Null);

    let (Some(symbol), Some(horizon)) = (symbol, horizon) else {
        return Ok(());
    };

    sqlx::query(
        r#"
        INSERT INTO pm_market_metadata (
            market_slug, price_to_beat, start_time, end_time, horizon, symbol, raw_market, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
        ON CONFLICT (market_slug) DO UPDATE SET
            price_to_beat = EXCLUDED.price_to_beat,
            start_time = COALESCE(EXCLUDED.start_time, pm_market_metadata.start_time),
            end_time = COALESCE(EXCLUDED.end_time, pm_market_metadata.end_time),
            horizon = COALESCE(EXCLUDED.horizon, pm_market_metadata.horizon),
            symbol = COALESCE(EXCLUDED.symbol, pm_market_metadata.symbol),
            raw_market = COALESCE(EXCLUDED.raw_market, pm_market_metadata.raw_market),
            updated_at = NOW()
        "#,
    )
    .bind(market_slug)
    .bind(price_to_beat)
    .bind(start_time)
    .bind(end_time)
    .bind(horizon)
    .bind(symbol)
    .bind(raw_market)
    .execute(pool)
    .await?;

    Ok(())
}

fn parse_price_from_question(question: &str) -> Option<rust_decimal::Decimal> {
    let marker_idx = question.char_indices().find_map(|(i, c)| match c {
        '$' | '↑' | '↓' => Some(i + c.len_utf8()),
        _ => None,
    })?;

    let tail = &question[marker_idx..];
    let cleaned: String = tail
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
        .filter(|c| *c != ',')
        .collect();

    if cleaned.is_empty() {
        return None;
    }

    cleaned.parse::<rust_decimal::Decimal>().ok()
}

fn collect_candidate_event_ids(
    series_id: &str,
    events: &[GammaEventInfo],
    log_filter_summary: bool,
) -> Vec<String> {
    let now = Utc::now();
    let min_end_time = now + chrono::Duration::seconds(30);
    let max_end_time = now + chrono::Duration::minutes(60);

    let mut candidates: Vec<(DateTime<Utc>, String)> = Vec::new();
    let mut no_end_date = 0usize;
    let mut parse_fail = 0usize;
    let mut out_of_range = 0usize;

    for event in events {
        let Some(end_str) = event.end_date.as_ref() else {
            no_end_date += 1;
            continue;
        };
        let Ok(end) =
            chrono::DateTime::parse_from_rfc3339(end_str).map(|dt| dt.with_timezone(&Utc))
        else {
            parse_fail += 1;
            continue;
        };
        if end <= min_end_time || end > max_end_time {
            out_of_range += 1;
            continue;
        }
        candidates.push((end, event.id.clone()));
    }

    if log_filter_summary {
        debug!(
            "Series {} filter: no_end_date={} parse_fail={} out_of_range={} candidates={}",
            series_id,
            no_end_date,
            parse_fail,
            out_of_range,
            candidates.len()
        );
    }

    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates
        .into_iter()
        .take(MAX_EVENTS_PER_SERIES)
        .map(|(_, event_id)| event_id)
        .collect()
}

fn build_discovered_event(details: &GammaEventInfo, series_id: &str) -> Option<DiscoveredEvent> {
    let end_time = details
        .end_date
        .as_ref()
        .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);

    let mut up_token: Option<String> = None;
    let mut down_token: Option<String> = None;
    let mut condition_id: Option<String> = None;

    let mut price_to_beat: Option<rust_decimal::Decimal> = details
        .title
        .as_ref()
        .and_then(|title| parse_price_from_question(title));
    let mut title: Option<String> = details.title.clone();

    for market in &details.markets {
        if price_to_beat.is_none() {
            if let Some(group_title) = market.group_item_title.as_ref() {
                price_to_beat = parse_price_from_question(group_title);
            }
        }
        if price_to_beat.is_none() {
            if let Some(question) = market.question.as_ref() {
                price_to_beat = parse_price_from_question(question);
                if title.is_none() {
                    title = Some(question.clone());
                }
            }
        }

        if let Some(ids_str) = market.clob_token_ids.as_ref() {
            if let Ok(ids) = serde_json::from_str::<Vec<String>>(ids_str) {
                if ids.len() >= 2 {
                    up_token = Some(ids[0].clone());
                    down_token = Some(ids[1].clone());
                    condition_id = market.condition_id.clone();
                    break;
                }
            }
        }

        if up_token.is_none() || down_token.is_none() {
            if let Some(tokens) = market.tokens.as_ref() {
                let up = tokens.iter().find(|token| {
                    let outcome = token.outcome.to_lowercase();
                    outcome.contains("up") || outcome == "yes" || outcome.starts_with("↑")
                });
                let down = tokens.iter().find(|token| {
                    let outcome = token.outcome.to_lowercase();
                    outcome.contains("down") || outcome == "no" || outcome.starts_with("↓")
                });
                if let (Some(up), Some(down)) = (up, down) {
                    up_token = Some(up.token_id.clone());
                    down_token = Some(down.token_id.clone());
                    condition_id = market.condition_id.clone();
                    break;
                }
            }
        }
    }

    let (Some(up_token), Some(down_token)) = (up_token, down_token) else {
        return None;
    };

    Some(DiscoveredEvent {
        event_id: details.id.clone(),
        series_id: series_id.to_string(),
        up_token,
        down_token,
        end_time,
        price_to_beat,
        title,
        condition_id,
    })
}

async fn load_series_discovery_batch(
    client: &PolymarketClient,
    pm_ws: Option<&Arc<PolymarketWebSocket>>,
    metadata_pool: Option<&PgPool>,
    series_id: &str,
    log_filter_summary: bool,
) -> Result<SeriesDiscoveryBatch> {
    let events = client.get_all_active_events(series_id).await?;
    let candidate_ids = collect_candidate_event_ids(series_id, &events, log_filter_summary);
    let mut discovered: HashMap<String, DiscoveredEvent> = HashMap::new();

    for event_id in candidate_ids {
        let details = match client.get_event_details(&event_id).await {
            Ok(details) => details,
            Err(error) => {
                debug!(
                    "Failed to fetch event details for {} (series {}): {}",
                    event_id, series_id, error
                );
                continue;
            }
        };

        let Some(event) = build_discovered_event(&details, series_id) else {
            continue;
        };

        if let Err(error) =
            upsert_pm_market_metadata(metadata_pool, &details, event.price_to_beat, event.end_time)
                .await
        {
            warn!(
                series_id = %series_id,
                event_id = %details.id,
                error = %error,
                "failed to upsert pm_market_metadata"
            );
        }

        if let Some(pm_ws) = pm_ws {
            pm_ws.register_token(&event.up_token, Side::Up).await;
            pm_ws.register_token(&event.down_token, Side::Down).await;
        }

        discovered.insert(details.id.clone(), event);
    }

    Ok(SeriesDiscoveryBatch {
        total_events: events.len(),
        discovered,
    })
}

async fn apply_discovery_diff(
    manager: &Arc<StrategyManager>,
    series_events: &Arc<RwLock<HashMap<String, HashMap<String, DiscoveredEvent>>>>,
    series_id: &str,
    discovered: HashMap<String, DiscoveredEvent>,
) -> bool {
    let mut changed = false;
    let mut series_events_guard = series_events.write().await;
    let previous = series_events_guard
        .entry(series_id.to_string())
        .or_default();

    let removed: Vec<String> = previous
        .keys()
        .filter(|event_id| !discovered.contains_key(*event_id))
        .cloned()
        .collect();
    if !removed.is_empty() {
        changed = true;
    }

    for event_id in removed {
        previous.remove(&event_id);
        manager.send_market_update(MarketUpdate::EventExpired { event_id });
    }

    for (event_id, event) in discovered {
        let should_send = match previous.get(&event_id) {
            None => true,
            Some(old) => {
                old.up_token != event.up_token
                    || old.down_token != event.down_token
                    || old.end_time != event.end_time
                    || old.price_to_beat != event.price_to_beat
            }
        };

        if should_send {
            changed = true;
            manager.send_market_update(MarketUpdate::EventDiscovered {
                event_id: event.event_id.clone(),
                series_id: event.series_id.clone(),
                up_token: event.up_token.clone(),
                down_token: event.down_token.clone(),
                end_time: event.end_time,
                price_to_beat: event.price_to_beat,
                title: event.title.clone(),
                condition_id: event.condition_id.clone(),
            });
        }

        previous.insert(event_id, event);
    }

    changed
}

async fn desired_polymarket_token_sides(
    series_events: &Arc<RwLock<HashMap<String, HashMap<String, DiscoveredEvent>>>>,
) -> HashMap<String, Side> {
    let guard = series_events.read().await;
    let mut desired = HashMap::new();
    for per_series in guard.values() {
        for event in per_series.values() {
            desired.insert(event.up_token.clone(), Side::Up);
            desired.insert(event.down_token.clone(), Side::Down);
        }
    }
    desired
}

impl DataFeedManager {
    pub async fn discover_series_events(&self, series_id: &str) -> Result<Vec<String>> {
        let Some(client) = self.pm_client.as_ref() else {
            return Ok(Vec::new());
        };

        match load_series_discovery_batch(
            client.as_ref(),
            self.polymarket_ws.as_ref(),
            self.metadata_pool.as_deref(),
            series_id,
            true,
        )
        .await
        {
            Ok(batch) => {
                let kept = batch.discovered.len();
                let token_ids: Vec<String> = batch
                    .discovered
                    .values()
                    .flat_map(|event| [event.up_token.clone(), event.down_token.clone()])
                    .collect();

                apply_discovery_diff(
                    &self.manager,
                    &self.series_events,
                    series_id,
                    batch.discovered,
                )
                .await;

                info!(
                    "Series {}: active={} kept={} subscribed_tokens={}",
                    series_id,
                    batch.total_events,
                    kept,
                    token_ids.len()
                );

                Ok(token_ids)
            }
            Err(error) => {
                warn!("Failed to fetch events for series {}: {}", series_id, error);
                Ok(Vec::new())
            }
        }
    }

    pub(super) async fn spawn_polymarket_refresh(&self, series_ids: Vec<String>) {
        let Some(pm_client) = self.pm_client.clone() else {
            return;
        };
        let Some(pm_ws) = self.polymarket_ws.clone() else {
            return;
        };

        let manager = self.manager.clone();
        let series_events = self.series_events.clone();
        let metadata_pool = self.metadata_pool.clone();
        let use_data_plane = self.data_plane.is_some();

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(POLYMARKET_REFRESH_SECS));
            loop {
                ticker.tick().await;
                let mut refresh_changed = false;

                for series_id in &series_ids {
                    let Ok(batch) = load_series_discovery_batch(
                        pm_client.as_ref(),
                        Some(&pm_ws),
                        metadata_pool.as_deref(),
                        series_id,
                        false,
                    )
                    .await
                    else {
                        continue;
                    };

                    refresh_changed |=
                        apply_discovery_diff(&manager, &series_events, series_id, batch.discovered)
                            .await;
                }

                if use_data_plane {
                    // Shared crypto collection keeps the broader raw token superset alive; the
                    // strategy feed may add tokens and request a resubscribe, but must not prune
                    // collector-owned subscriptions down to the strategy's narrower window set.
                    if refresh_changed {
                        debug!(
                            "Polymarket refresh changed token set; requesting PlatformDataPlane resubscribe"
                        );
                        pm_ws.request_resubscribe();
                    }
                    continue;
                }

                let desired = desired_polymarket_token_sides(&series_events).await;
                let (added, removed, updated, total) = pm_ws.reconcile_token_sides(&desired).await;
                if (added + removed + updated) > 0 {
                    info!(
                        "Polymarket WS token reconcile: added={} removed={} updated={} total={}",
                        added, removed, updated, total
                    );
                    pm_ws.request_resubscribe();
                }
            }
        });
    }
}
