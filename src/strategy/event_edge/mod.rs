//! Automated mispricing scanner for "event-driven" markets where an external
//! public data source drives the final resolution.
//!
//! Initial implementation targets Arena (Chatbot Arena / arena.ai) text leaderboard
//! driven markets like "Which company has the best AI model end of February?".

pub mod core;
pub mod data_source;

use crate::adapters::polymarket_clob::GAMMA_API_URL;
use crate::adapters::PolymarketClient;
use crate::domain::{OrderRequest, Side};
use crate::error::{PloyError, Result};
use crate::strategy::event_models::arena_text::{
    fetch_arena_text_snapshot, scores_to_probabilities, ArenaTextSnapshot,
};
use crate::strategy::{ExpectedValue, POLYMARKET_FEE_RATE};
use chrono::{DateTime, Utc};
use polymarket_client_sdk::gamma::types::request::SearchRequest;
use polymarket_client_sdk::gamma::Client as GammaClient;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEdgeConfig {
    /// Polymarket event id (preferred) OR a title to search for.
    pub event_id: Option<String>,
    pub title: Option<String>,

    /// Minimum edge (p_true - entry_price) required to trade.
    pub min_edge: Decimal,
    /// Do not buy above this entry price.
    pub max_entry: Decimal,
    /// Shares to buy per trade.
    pub shares: u64,

    /// Poll interval for watch mode.
    pub interval: Duration,
    /// If true, keep scanning; else run once.
    pub watch: bool,
    /// If true, attempt to place trades.
    pub trade: bool,
    /// If true, do not send real orders (but still print what would happen).
    pub dry_run: bool,
}

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

impl Default for EventEdgeConfig {
    fn default() -> Self {
        Self {
            event_id: None,
            title: None,
            min_edge: dec!(0.08),
            max_entry: dec!(0.75),
            shares: 100,
            interval: Duration::from_secs(30),
            watch: false,
            trade: false,
            dry_run: true,
        }
    }
}

#[derive(Debug, Clone)]
struct OutcomeMarket {
    name: String,
    yes_token_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeRow {
    pub outcome: String,
    pub yes_token_id: String,
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

pub async fn run_event_edge(client: &PolymarketClient, cfg: EventEdgeConfig) -> Result<()> {
    let mut last_trade_at: HashMap<String, DateTime<Utc>> = HashMap::new();

    loop {
        let event_id = match (&cfg.event_id, &cfg.title) {
            (Some(id), _) => id.clone(),
            (None, Some(title)) => discover_best_event_id_by_title(title).await?,
            (None, None) => {
                return Err(PloyError::Internal(
                    "EventEdge requires --event or --title".to_string(),
                ))
            }
        };

        let now = Utc::now();
        let arena = fetch_arena_text_snapshot().await?;
        let scan = scan_event_edge_once(client, &event_id, Some(arena.clone())).await?;

        info!(
            "EventEdge: event={} title=\"{}\" end={} arena_last_updated={:?} conf={:.2}",
            scan.event_id,
            scan.event_title,
            scan.end_time.to_rfc3339(),
            scan.arena_last_updated,
            scan.confidence
        );

        if let Some(top) = arena.top_org() {
            info!("Arena Text current top org: {}", top);
        }

        for r in scan.rows.iter().take(10) {
            let ask = r
                .market_ask
                .map(|v| format!("{:.2}¢", v * dec!(100)))
                .unwrap_or("-".into());
            let p = format!("{:.1}%", r.p_true * dec!(100));
            let edge = r
                .edge
                .map(|v| format!("{:.1}pp", v * dec!(100)))
                .unwrap_or("-".into());
            let ev =
                r.ev.as_ref()
                    .map(|v| format!("EV={:.4}", v.net_ev))
                    .unwrap_or("-".into());
            info!(
                "  {} | ask={} | p_true={} | edge={} | {}",
                r.outcome, ask, p, edge, ev
            );
        }

        if cfg.trade {
            // Trade the best +EV row that clears thresholds and isn't on cooldown.
            for r in &scan.rows {
                let Some(ask) = r.market_ask else { continue };
                let Some(edge) = r.edge else { continue };
                let Some(ev) = &r.ev else { continue };

                if ask > cfg.max_entry {
                    continue;
                }
                if edge < cfg.min_edge {
                    continue;
                }
                if !ev.is_positive_ev {
                    continue;
                }

                // Cooldown: don't re-buy the same token too frequently.
                let cooldown_secs = (cfg.interval.as_secs() * 2).max(30);
                if let Some(last) = last_trade_at.get(&r.yes_token_id) {
                    if (now - *last).num_seconds() < cooldown_secs as i64 {
                        continue;
                    }
                }

                let order =
                    OrderRequest::buy_limit(r.yes_token_id.clone(), Side::Up, cfg.shares, ask);

                if cfg.dry_run {
                    warn!(
                        "DRY RUN: would BUY {} shares of {} @ {:.2}¢ (edge {:.1}pp)",
                        cfg.shares,
                        r.outcome,
                        ask * dec!(100),
                        edge * dec!(100)
                    );
                } else {
                    info!(
                        "Placing BUY {} shares of {} @ {:.2}¢ (edge {:.1}pp)",
                        cfg.shares,
                        r.outcome,
                        ask * dec!(100),
                        edge * dec!(100)
                    );
                    let resp = client.submit_order(&order).await?;
                    info!("Order submitted: id={} status={}", resp.id, resp.status);
                }

                last_trade_at.insert(r.yes_token_id.clone(), now);
                break;
            }
        }

        if !cfg.watch {
            break;
        }
        tokio::time::sleep(cfg.interval).await;
    }

    Ok(())
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
