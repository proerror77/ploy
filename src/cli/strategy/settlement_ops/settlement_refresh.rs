use anyhow::Context;
use chrono::Utc;
use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
use std::collections::{HashMap, HashSet};
use tracing::warn;

use crate::adapters::PolymarketClient;

use super::{Result, is_market_resolved};

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SettlementRefreshSummary {
    pub requested_tokens: usize,
    pub refreshed_markets: usize,
    pub refreshed_tokens: usize,
}

pub(super) async fn refresh_pm_token_settlements_for_tokens(
    pool: &PgPool,
    token_ids: &[String],
    max_refresh: usize,
) -> Result<SettlementRefreshSummary> {
    let existing = sqlx::query(
        r#"
        SELECT token_id, resolved
        FROM pm_token_settlements
        WHERE token_id = ANY($1)
        "#,
    )
    .bind(token_ids)
    .fetch_all(pool)
    .await
    .context("Failed to query pm_token_settlements")?;

    let mut resolved_map: HashMap<String, bool> = HashMap::new();
    for row in existing {
        let token_id: String = row.get("token_id");
        let resolved: bool = row.get("resolved");
        resolved_map.insert(token_id, resolved);
    }

    let mut to_refresh: Vec<String> = token_ids
        .iter()
        .filter(|token_id| !resolved_map.get(*token_id).copied().unwrap_or(false))
        .cloned()
        .collect();

    if to_refresh.len() > max_refresh {
        to_refresh.truncate(max_refresh);
    }

    let requested_tokens = to_refresh.len();
    if requested_tokens == 0 {
        return Ok(SettlementRefreshSummary::default());
    }

    let pm = PolymarketClient::new("https://clob.polymarket.com", true)
        .context("Failed to create Polymarket client")?;

    let mut refreshed_markets = 0usize;
    let mut refreshed_tokens = 0usize;
    let mut seen_conditions: HashSet<String> = HashSet::new();

    for token_id in to_refresh {
        let market = match pm.get_gamma_market_by_token_id(&token_id).await {
            Ok(market) => market,
            Err(error) => {
                warn!(token_id = %token_id, error = %error, "failed to fetch gamma market for token");
                continue;
            }
        };

        if let Some(ref condition_id) = market.condition_id {
            let condition_id = condition_id.to_string();
            if !seen_conditions.insert(condition_id) {
                continue;
            }
        }

        let clob_ids: Vec<String> = market
            .clob_token_ids
            .as_ref()
            .map(|ids| ids.iter().map(|id| id.to_string()).collect())
            .unwrap_or_default();
        let outcomes: Vec<String> = market.outcomes.clone().unwrap_or_default();
        let price_strs: Vec<String> = market
            .outcome_prices
            .as_ref()
            .map(|prices| prices.iter().map(|price| price.to_string()).collect())
            .unwrap_or_default();

        if clob_ids.is_empty() || price_strs.is_empty() {
            tracing::debug!(
                token_id = %token_id,
                market_id = %market.id,
                "gamma market missing clob_token_ids or outcome_prices; skipping"
            );
            continue;
        }

        let mut prices: Vec<Decimal> = Vec::new();
        for price in &price_strs {
            if let Ok(price) = price.parse::<Decimal>() {
                prices.push(price);
            }
        }

        let resolved = market.closed.unwrap_or(false) && is_market_resolved(&prices);
        let resolved_at = resolved.then(Utc::now);
        let raw_market = serde_json::to_value(&market).unwrap_or(serde_json::json!({}));

        let market_slug = market.slug.clone();
        let condition_id = market.condition_id.map(|value| value.to_string());

        for (index, token_id) in clob_ids.iter().enumerate() {
            let outcome = outcomes.get(index).cloned();
            let settled_price = price_strs
                .get(index)
                .and_then(|price| price.parse::<Decimal>().ok());

            sqlx::query(
                r#"
                INSERT INTO pm_token_settlements (
                    token_id,
                    condition_id,
                    market_id,
                    market_slug,
                    outcome,
                    settled_price,
                    resolved,
                    resolved_at,
                    fetched_at,
                    raw_market
                )
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8,NOW(),$9)
                ON CONFLICT (token_id) DO UPDATE SET
                    condition_id = EXCLUDED.condition_id,
                    market_id = EXCLUDED.market_id,
                    market_slug = EXCLUDED.market_slug,
                    outcome = EXCLUDED.outcome,
                    settled_price = EXCLUDED.settled_price,
                    resolved = EXCLUDED.resolved,
                    resolved_at = COALESCE(pm_token_settlements.resolved_at, EXCLUDED.resolved_at),
                    fetched_at = NOW(),
                    raw_market = EXCLUDED.raw_market
                "#,
            )
            .bind(token_id)
            .bind(condition_id.as_deref())
            .bind(&market.id)
            .bind(market_slug.as_deref())
            .bind(outcome.as_deref())
            .bind(settled_price)
            .bind(resolved)
            .bind(resolved_at)
            .bind(sqlx::types::Json(raw_market.clone()))
            .execute(pool)
            .await
            .context("Failed to upsert pm_token_settlements row")?;

            refreshed_tokens += 1;
        }

        refreshed_markets += 1;
    }

    Ok(SettlementRefreshSummary {
        requested_tokens,
        refreshed_markets,
        refreshed_tokens,
    })
}
