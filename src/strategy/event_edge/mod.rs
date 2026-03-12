//! Automated mispricing scanner for "event-driven" markets where an external
//! public data source drives the final resolution.
//!
//! Initial implementation targets Arena (Chatbot Arena / arena.ai) text leaderboard
//! driven markets like "Which company has the best AI model end of February?".

pub mod core;
pub mod data_source;
pub mod strategy;

use crate::adapters::polymarket_clob::GAMMA_API_URL;
use crate::adapters::PolymarketClient;
use crate::error::{PloyError, Result};
use crate::strategy::event_models::arena_text::{
    fetch_arena_text_snapshot, scores_to_probabilities, ArenaTextSnapshot,
};
use crate::strategy::impls::{ExpectedValue, POLYMARKET_FEE_RATE};
use chrono::{DateTime, Utc};
use polymarket_client_sdk::gamma::types::request::SearchRequest;
use polymarket_client_sdk::gamma::Client as GammaClient;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::info;

pub use strategy::EventEdgeStrategy;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEdgeScan {
    pub event_id: String,
    pub event_title: String,
    pub end_time: DateTime<Utc>,
    /// Model confidence factor applied to `p_now` (0..1).
    pub confidence: f64,
    pub arena_last_updated: Option<chrono::NaiveDate>,
    pub arena_staleness_days: Option<f64>,
    pub rows: Vec<EdgeRow>,
}

#[derive(Debug, Clone)]
struct OutcomeMarket {
    name: String,
    yes_token_id: String,
    condition_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeRow {
    pub outcome: String,
    pub yes_token_id: String,
    pub condition_id: Option<String>,
    pub market_ask: Option<Decimal>,
    pub market_mid: Option<Decimal>,
    pub p_true: Decimal,
    pub edge: Option<Decimal>,
    pub ev: Option<ExpectedValue>,
}

fn normalize_outcome_company(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    if n.contains("anthropic") {
        return Some("Anthropic");
    }
    if n.contains("google") || n.contains("deepmind") || n.contains("gemini") {
        return Some("Google");
    }
    if n.contains("openai") || n.contains("chatgpt") || n.contains("gpt") {
        return Some("OpenAI");
    }
    if n.contains("xai") || n.contains("x.ai") || n.contains("grok") {
        return Some("xAI");
    }
    None
}

fn confidence_factor(time_to_end_days: f64, arena_staleness_days: Option<f64>) -> f64 {
    // Tunables: smaller tau => confidence rises faster as settlement nears.
    let tau_days = 14.0;
    let tau_stale_days = 3.0;

    let time_conf = (-time_to_end_days.max(0.0) / tau_days).exp();
    let stale = arena_staleness_days.unwrap_or(0.0).max(0.0);
    let stale_conf = (-stale / tau_stale_days).exp();

    (time_conf * stale_conf).clamp(0.0, 1.0)
}

fn blend_with_uniform(p_now: &HashMap<String, Decimal>, conf: f64) -> HashMap<String, Decimal> {
    let mut out = HashMap::new();
    if p_now.is_empty() {
        return out;
    }
    let n = p_now.len() as f64;
    let u = Decimal::from_f64(1.0 / n).unwrap_or(dec!(0));
    let conf_d = Decimal::from_f64(conf).unwrap_or(dec!(0));
    let one_minus = Decimal::ONE - conf_d;

    for (k, p) in p_now {
        let blended = *p * conf_d + u * one_minus;
        out.insert(k.clone(), blended);
    }
    out
}

fn extract_org_scores_for_options(
    snapshot: &ArenaTextSnapshot,
    orgs: &[String],
) -> HashMap<String, i32> {
    let best = snapshot.best_score_by_org();
    let mut scores = HashMap::new();
    for org in orgs {
        if let Some(s) = best.get(org) {
            scores.insert(org.clone(), *s);
        }
    }
    scores
}

async fn load_event_outcomes(
    client: &PolymarketClient,
    event_id: &str,
) -> Result<(String, DateTime<Utc>, Vec<OutcomeMarket>)> {
    let event = client.get_event_details(event_id).await?;
    let title = event.title.unwrap_or_else(|| event_id.to_string());
    let end_time = event
        .end_date
        .as_ref()
        .and_then(|d| d.parse().ok())
        .unwrap_or_else(Utc::now);

    let mut outcomes = Vec::new();
    for market in &event.markets {
        let outcome_name = market
            .group_item_title
            .clone()
            .or_else(|| market.question.clone())
            .unwrap_or_else(|| "Unknown".to_string());

        let Some(clob_ids_str) = &market.clob_token_ids else {
            continue;
        };
        let Ok(token_ids) = serde_json::from_str::<Vec<String>>(clob_ids_str) else {
            continue;
        };
        let Some(yes_token_id) = token_ids.first() else {
            continue;
        };

        outcomes.push(OutcomeMarket {
            name: outcome_name,
            yes_token_id: yes_token_id.clone(),
            condition_id: market.condition_id.clone(),
        });
    }

    if outcomes.is_empty() {
        return Err(PloyError::Internal(
            "No outcomes found in event (missing clobTokenIds?)".into(),
        ));
    }

    Ok((title, end_time, outcomes))
}

pub async fn scan_event_edge_once(
    client: &PolymarketClient,
    event_id: &str,
    arena: Option<ArenaTextSnapshot>,
) -> Result<EventEdgeScan> {
    let (event_title, end_time, outcomes) = load_event_outcomes(client, event_id).await?;
    let arena = match arena {
        Some(a) => a,
        None => fetch_arena_text_snapshot().await?,
    };

    let now = Utc::now();
    let time_to_end_days = (end_time - now).num_seconds().max(0) as f64 / 86_400.0;
    let conf = confidence_factor(time_to_end_days, arena.staleness_days());

    let mut orgs: Vec<String> = outcomes
        .iter()
        .filter_map(|o| normalize_outcome_company(&o.name).map(|s| s.to_string()))
        .collect();
    orgs.sort();
    orgs.dedup();

    let org_scores = extract_org_scores_for_options(&arena, &orgs);
    let p_now = scores_to_probabilities(&org_scores, 20.0);
    let p_true = blend_with_uniform(&p_now, conf);

    let mut rows: Vec<EdgeRow> = Vec::new();
    for o in &outcomes {
        let Some(org) = normalize_outcome_company(&o.name) else {
            continue;
        };
        let p = p_true.get(org).copied().unwrap_or_else(|| {
            Decimal::from_f64(1.0 / (orgs.len().max(1) as f64)).unwrap_or(dec!(0))
        });

        let (bid, ask) = client
            .get_best_prices(&o.yes_token_id)
            .await
            .unwrap_or((None, None));
        let mid = match (bid, ask) {
            (Some(b), Some(a)) => Some((a + b) / dec!(2)),
            (Some(b), None) => Some(b),
            (None, Some(a)) => Some(a),
            _ => None,
        };

        let edge = ask.map(|a| p - a);
        let ev = ask.map(|a| ExpectedValue::calculate(a, p, Some(POLYMARKET_FEE_RATE)));

        rows.push(EdgeRow {
            outcome: o.name.clone(),
            yes_token_id: o.yes_token_id.clone(),
            condition_id: o.condition_id.clone(),
            market_ask: ask,
            market_mid: mid,
            p_true: p,
            edge,
            ev,
        });
    }

    rows.sort_by(|a, b| {
        let ae = a.ev.as_ref().map(|e| e.net_ev).unwrap_or(Decimal::ZERO);
        let be = b.ev.as_ref().map(|e| e.net_ev).unwrap_or(Decimal::ZERO);
        be.cmp(&ae)
    });

    Ok(EventEdgeScan {
        event_id: event_id.to_string(),
        event_title,
        end_time,
        confidence: conf,
        arena_last_updated: arena.last_updated,
        arena_staleness_days: arena.staleness_days(),
        rows,
    })
}

// =============================================================================
// Polymarket event discovery by title (Gamma API)
// =============================================================================

fn title_match_score(query: &str, candidate: &str) -> i32 {
    let q = query.to_lowercase();
    let c = candidate.to_lowercase();
    if c == q {
        return 1_000;
    }
    if c.contains(&q) {
        return 800;
    }
    // Token overlap scoring.
    let q_tokens: Vec<&str> = q.split_whitespace().collect();
    let mut score = 0i32;
    for t in q_tokens {
        if t.len() < 3 {
            continue;
        }
        if c.contains(t) {
            score += 10;
        }
    }
    score
}

pub async fn discover_best_event_id_by_title(title: &str) -> Result<String> {
    let gamma = GammaClient::new(GAMMA_API_URL)
        .map_err(|e| PloyError::Internal(format!("Failed to create Gamma client: {e}")))?;
    let req = SearchRequest::builder().q(title).build();
    let search = tokio::time::timeout(Duration::from_secs(15), gamma.search(&req))
        .await
        .map_err(|_| PloyError::Internal("Gamma search timed out".to_string()))?
        .map_err(|e| PloyError::Internal(format!("Gamma search failed: {e}")))?;
    let resp = search.events.unwrap_or_default();

    let mut best: Option<(i32, String, String)> = None;
    for ev in resp.into_iter().filter(|e| !e.closed.unwrap_or(false)) {
        let Some(t) = ev.title.clone() else { continue };
        let score = title_match_score(title, &t);
        match &best {
            None => best = Some((score, ev.id, t)),
            Some((best_score, _, _)) if score > *best_score => best = Some((score, ev.id, t)),
            _ => {}
        }
    }

    match best {
        Some((_score, id, t)) => {
            info!("Discovered Polymarket event: {} (title=\"{}\")", id, t);
            Ok(id)
        }
        None => Err(PloyError::Internal(
            "No Polymarket events matched title_contains query".to_string(),
        )),
    }
}
