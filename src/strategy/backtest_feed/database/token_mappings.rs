use super::symbol_filter;
use alloy::primitives::U256;
use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;

#[derive(Default)]
pub(super) struct TokenMappings {
    pub(super) token_to_symbol: HashMap<String, String>,
    pub(super) token_to_slug: HashMap<String, String>,
    pub(super) slug_to_symbol: HashMap<String, String>,
}

impl TokenMappings {
    pub(super) fn known_token_ids(&self) -> Vec<String> {
        self.token_to_slug.keys().cloned().collect()
    }

    pub(super) fn resolve_symbol(&self, token_id: &str, event_slug: &str) -> Option<String> {
        self.token_to_symbol
            .get(token_id)
            .cloned()
            .or_else(|| self.slug_to_symbol.get(event_slug).cloned())
            .or_else(|| infer_symbol_from_slug(event_slug))
    }
}

pub(super) async fn build_token_mappings(
    pool: &PgPool,
    symbols: &[String],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    sync_records_exists: bool,
    pm_market_metadata_exists: bool,
    pm_token_settlements_exists: bool,
) -> TokenMappings {
    let mut mappings = TokenMappings::default();

    if sync_records_exists {
        let sync_map_rows: Result<Vec<(String, String, Option<String>, Option<String>)>> =
            sqlx::query_as(
                r#"
                    SELECT DISTINCT pm_market_slug, symbol, pm_yes_token_id, pm_no_token_id
                    FROM sync_records
                    WHERE pm_market_slug IS NOT NULL
                      AND ($1::text[] IS NULL OR symbol = ANY($1))
                      AND ($2::timestamptz IS NULL OR timestamp >= $2)
                      AND ($3::timestamptz IS NULL OR timestamp <= $3)
                    "#,
            )
            .bind(symbol_filter(symbols))
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await
            .map_err(Into::into);

        match sync_map_rows {
            Ok(rows) => {
                for (slug, sym, yes_token_id, no_token_id) in rows {
                    if !slug.is_empty() && !sym.is_empty() {
                        mappings.slug_to_symbol.insert(slug.clone(), sym.clone());
                    }
                    if let Some(t) = yes_token_id {
                        mappings.token_to_slug.insert(t.clone(), slug.clone());
                        if !sym.is_empty() {
                            mappings.token_to_symbol.insert(t, sym.clone());
                        }
                    }
                    if let Some(t) = no_token_id {
                        mappings.token_to_slug.insert(t.clone(), slug.clone());
                        if !sym.is_empty() {
                            mappings.token_to_symbol.insert(t, sym.clone());
                        }
                    }
                }
                info!(
                    "Built token mapping from sync_records: {} tokens, {} slugs",
                    mappings.token_to_slug.len(),
                    mappings.slug_to_symbol.len()
                );
            }
            Err(e) => {
                info!("sync_records mapping query failed (older schema?): {e}");
            }
        }
    }

    if pm_token_settlements_exists {
        let settlement_map_rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            r#"
                SELECT token_id, market_slug, outcome
                FROM pm_token_settlements
                WHERE market_slug IS NOT NULL AND market_slug != ''
                  AND ($1::timestamptz IS NULL OR fetched_at >= $1)
                  AND ($2::timestamptz IS NULL OR fetched_at <= $2)
                "#,
        )
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        for (token_id, market_slug, outcome) in settlement_map_rows {
            let Some(slug) = market_slug else { continue };
            mappings
                .token_to_slug
                .entry(token_id.clone())
                .or_insert_with(|| slug.clone());
            if let Some(sym) = mappings
                .slug_to_symbol
                .get(&slug)
                .cloned()
                .or_else(|| infer_symbol_from_slug(&slug))
            {
                mappings.token_to_symbol.entry(token_id).or_insert(sym);
            }
            if !mappings.slug_to_symbol.contains_key(&slug) {
                if let Some(sym) = infer_symbol_from_slug(&slug) {
                    mappings.slug_to_symbol.insert(slug.clone(), sym);
                }
            }
            let _ = outcome;
        }
        info!(
            "Built token mapping from pm_token_settlements: {} tokens",
            mappings.token_to_slug.len()
        );
    }

    if pm_market_metadata_exists {
        let before = mappings.token_to_slug.len();
        let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(
            r#"
                SELECT DISTINCT
                    market_slug,
                    symbol,
                    jsonb_array_elements_text((raw_market->>'clobTokenIds')::jsonb) AS token_id
                FROM pm_market_metadata
                WHERE raw_market IS NOT NULL
                  AND raw_market ? 'clobTokenIds'
                  AND ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR end_time >= $2)
                  AND ($3::timestamptz IS NULL OR start_time <= $3)
                "#,
        )
        .bind(symbol_filter(symbols))
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        for (slug, sym, token_id) in rows {
            if slug.is_empty() || token_id.is_empty() {
                continue;
            }
            let Some(token_id_norm) = normalize_clob_token_id(&token_id) else {
                continue;
            };

            mappings
                .token_to_slug
                .entry(token_id_norm.clone())
                .or_insert_with(|| slug.clone());

            let symbol = sym
                .filter(|s| !s.is_empty())
                .or_else(|| mappings.slug_to_symbol.get(&slug).cloned())
                .or_else(|| infer_symbol_from_slug(&slug));

            if let Some(symbol) = symbol {
                if !symbol.is_empty() {
                    mappings
                        .token_to_symbol
                        .entry(token_id_norm)
                        .or_insert(symbol.clone());
                    mappings.slug_to_symbol.entry(slug).or_insert(symbol);
                }
            }
        }

        let after = mappings.token_to_slug.len();
        if after > before {
            info!(
                "Supplemented token mapping from pm_market_metadata.raw_market.clobTokenIds: +{} tokens (now {})",
                after - before,
                after
            );
        }
    }

    mappings
}

pub(super) fn infer_symbol_from_slug(slug: &str) -> Option<String> {
    let s = slug.to_ascii_lowercase();
    if s.starts_with("btc-") || s.starts_with("bitcoin-") {
        return Some("BTCUSDT".to_string());
    }
    if s.starts_with("eth-") || s.starts_with("ethereum-") {
        return Some("ETHUSDT".to_string());
    }
    if s.starts_with("sol-") || s.starts_with("solana-") {
        return Some("SOLUSDT".to_string());
    }
    None
}

pub(super) fn normalize_clob_token_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return U256::from_str_radix(hex, 16).ok().map(|u| u.to_string());
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return Some(s.to_string());
    }
    U256::from_str_radix(s, 16).ok().map(|u| u.to_string())
}
