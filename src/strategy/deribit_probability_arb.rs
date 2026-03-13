//! Deribit-implied probability arbitrage for Polymarket up/down binaries.
//!
//! Core idea:
//! - Use Deribit options to infer a short-horizon risk-neutral probability `P(UP)`.
//! - Compare to Polymarket YES/NO best asks.
//! - Execute buys only when net edge (after fee buffer) is positive and above threshold.

use crate::adapters::{GammaEventInfo, PolymarketClient};
use crate::error::{PloyError, Result};
use crate::strategy::execution::executor::OrderExecutor;
use chrono::{DateTime, Utc};
#[cfg(test)]
use chrono::{Datelike, Timelike};
#[cfg(test)]
use ordered_float::OrderedFloat;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tracing::{debug, info, warn};

#[path = "deribit_probability_arb_support.rs"]
mod support;
mod order_flow;

#[cfg(test)]
use support::parse_deribit_expiry;
use support::{
    DERIBIT_PUBLIC_API, DeribitPublicClient, SECONDS_PER_YEAR, infer_yes_index,
    kelly_fraction_binary, normalize_symbol, parse_event_end, parse_string_array, spread_bps,
    symbol_to_deribit_currency,
};
pub use support::{
    ParsedPolymarketQuestion, SurfacePoint, VolSurfaceSnapshot, binary_call_prob_forward,
    interpolate_iv_linear, net_edge, norm_cdf, parse_polymarket_question,
};

/// Runtime configuration for Deribit probability arbitrage.
#[derive(Debug, Clone)]
pub struct DeribitProbabilityArbConfig {
    /// Symbols to trade (BTC, ETH, BTCUSDT, ETHUSDT are accepted forms).
    pub symbols: Vec<String>,
    /// Polymarket series IDs to scan.
    pub series_ids: Vec<String>,
    /// Main loop cadence.
    pub poll_interval_ms: u64,
    /// Refresh market discovery every N seconds.
    pub discovery_refresh_secs: u64,
    /// Max events pulled per series per refresh.
    pub max_events_per_series: usize,
    /// Refresh Deribit surface every N seconds.
    pub surface_refresh_secs: u64,
    /// Min/Max seconds to event expiry to allow new entries.
    pub min_time_remaining_secs: u64,
    pub max_time_remaining_secs: u64,
    /// Minimum net edge after fee buffer.
    pub min_edge: Decimal,
    /// Fee/slippage buffer deducted from model edge.
    pub fee_buffer: Decimal,
    /// Maximum acceptable bid/ask spread.
    pub max_spread_bps: u32,
    /// Risk sizing.
    pub max_trade_usd: Decimal,
    pub min_trade_usd: Decimal,
    pub max_symbol_exposure_usd: Decimal,
    pub kelly_fraction: f64,
    pub min_kelly_allocation: f64,
    pub min_shares: u64,
    /// Cooldown per condition after attempted trade.
    pub cooldown_secs: u64,
    /// Hard cap on simultaneously tracked markets.
    pub max_markets: usize,
}

impl Default for DeribitProbabilityArbConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTC".to_string(), "ETH".to_string()],
            // BTC/ETH 5m + 15m recurring series IDs.
            series_ids: vec![
                "10684".to_string(), // BTC 5m
                "10192".to_string(), // BTC 15m
                "10683".to_string(), // ETH 5m
                "10191".to_string(), // ETH 15m
            ],
            poll_interval_ms: 2_000,
            discovery_refresh_secs: 30,
            max_events_per_series: 8,
            surface_refresh_secs: 5,
            min_time_remaining_secs: 45,
            max_time_remaining_secs: 900,
            min_edge: dec!(0.03),
            fee_buffer: dec!(0.02),
            max_spread_bps: 250,
            max_trade_usd: dec!(50),
            min_trade_usd: dec!(5),
            max_symbol_exposure_usd: dec!(150),
            kelly_fraction: 0.25,
            min_kelly_allocation: 0.05,
            min_shares: 5,
            cooldown_secs: 45,
            max_markets: 16,
        }
    }
}

#[derive(Debug, Clone)]
struct DiscoveredMarket {
    #[allow(dead_code)]
    event_id: String,
    condition_id: String,
    symbol: String,
    strike: Decimal,
    yes_token_id: String,
    no_token_id: String,
    end_time: DateTime<Utc>,
    question: String,
}

#[derive(Debug, Clone)]
struct SurfaceCacheEntry {
    forward: f64,
    surface: VolSurfaceSnapshot,
    fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct OpenExposure {
    symbol: String,
    notional_usd: Decimal,
    expires_at: DateTime<Utc>,
}

/// Live runner for Deribit-implied probability arbitrage.
pub struct DeribitProbabilityArbRunner {
    config: DeribitProbabilityArbConfig,
    pm_client: PolymarketClient,
    executor: OrderExecutor,
    deribit: DeribitPublicClient,
    tracked_markets: Vec<DiscoveredMarket>,
    last_discovery_at: Option<DateTime<Utc>>,
    surface_cache: HashMap<String, SurfaceCacheEntry>, // currency -> cache
    last_trade_at: HashMap<String, DateTime<Utc>>,     // condition_id -> ts
    open_exposure_by_condition: HashMap<String, OpenExposure>,
    symbol_exposure_usd: HashMap<String, Decimal>, // symbol -> active notional
}

impl DeribitProbabilityArbRunner {
    pub fn new(
        config: DeribitProbabilityArbConfig,
        pm_client: PolymarketClient,
        executor: OrderExecutor,
    ) -> Self {
        Self {
            config,
            pm_client,
            executor,
            deribit: DeribitPublicClient::new(DERIBIT_PUBLIC_API.to_string()),
            tracked_markets: Vec::new(),
            last_discovery_at: None,
            surface_cache: HashMap::new(),
            last_trade_at: HashMap::new(),
            open_exposure_by_condition: HashMap::new(),
            symbol_exposure_usd: HashMap::new(),
        }
    }

    pub async fn run(mut self) -> Result<()> {
        info!(
            symbols = ?self.config.symbols,
            series_ids = ?self.config.series_ids,
            min_edge = %self.config.min_edge,
            fee_buffer = %self.config.fee_buffer,
            "Starting Deribit probability arbitrage runner"
        );

        loop {
            let now = Utc::now();
            self.purge_expired_exposure(now);

            if self.should_refresh_discovery(now) {
                if let Err(e) = self.refresh_markets().await {
                    warn!("market discovery refresh failed: {}", e);
                }
            }

            if let Err(e) = self.refresh_deribit_surfaces(now).await {
                warn!("Deribit surface refresh failed: {}", e);
            }

            let markets = self.tracked_markets.clone();
            for market in &markets {
                if let Err(e) = self.process_market(now, market).await {
                    warn!(
                        condition_id = market.condition_id,
                        market = market.question,
                        "market processing failed: {}",
                        e
                    );
                }
            }

            tokio::time::sleep(Duration::from_millis(self.config.poll_interval_ms)).await;
        }
    }

    fn should_refresh_discovery(&self, now: DateTime<Utc>) -> bool {
        match self.last_discovery_at {
            None => true,
            Some(ts) => (now - ts).num_seconds() >= self.config.discovery_refresh_secs as i64,
        }
    }

    fn purge_expired_exposure(&mut self, now: DateTime<Utc>) {
        let expired: Vec<String> = self
            .open_exposure_by_condition
            .iter()
            .filter_map(|(condition, exp)| {
                if exp.expires_at <= now {
                    Some(condition.clone())
                } else {
                    None
                }
            })
            .collect();

        for condition in expired {
            if let Some(exp) = self.open_exposure_by_condition.remove(&condition) {
                let symbol_key = normalize_symbol(&exp.symbol);
                let current = self
                    .symbol_exposure_usd
                    .get(&symbol_key)
                    .copied()
                    .unwrap_or(Decimal::ZERO);
                let next = (current - exp.notional_usd).max(Decimal::ZERO);
                self.symbol_exposure_usd.insert(symbol_key, next);
            }
        }
    }

    async fn refresh_markets(&mut self) -> Result<()> {
        let allowed_symbols: HashSet<String> = self
            .config
            .symbols
            .iter()
            .map(|s| normalize_symbol(s))
            .collect();

        let mut discovered: Vec<DiscoveredMarket> = Vec::new();
        let mut seen_conditions = HashSet::<String>::new();

        for series_id in &self.config.series_ids {
            if discovered.len() >= self.config.max_markets {
                break;
            }

            let events = self.pm_client.get_all_active_events(series_id).await?;
            for event in events.into_iter().take(self.config.max_events_per_series) {
                if discovered.len() >= self.config.max_markets {
                    break;
                }

                let details = self.pm_client.get_event_details(&event.id).await?;
                let mut markets = extract_markets_from_event(&details, &allowed_symbols);
                markets.retain(|m| symbol_to_deribit_currency(&m.symbol).is_some());

                for m in markets {
                    if seen_conditions.insert(m.condition_id.clone()) {
                        discovered.push(m);
                    }
                    if discovered.len() >= self.config.max_markets {
                        break;
                    }
                }
            }
        }

        self.tracked_markets = discovered;
        self.last_discovery_at = Some(Utc::now());

        info!(
            tracked_markets = self.tracked_markets.len(),
            "refreshed Polymarket market set for Deribit probability arbitrage"
        );
        Ok(())
    }

    async fn refresh_deribit_surfaces(&mut self, now: DateTime<Utc>) -> Result<()> {
        let needed_currencies: HashSet<String> = self
            .tracked_markets
            .iter()
            .filter_map(|m| symbol_to_deribit_currency(&m.symbol))
            .map(|c| c.to_string())
            .collect();

        for currency in needed_currencies {
            let is_stale = self
                .surface_cache
                .get(&currency)
                .map(|cached| {
                    (now - cached.fetched_at).num_seconds()
                        >= self.config.surface_refresh_secs as i64
                })
                .unwrap_or(true);

            if !is_stale {
                continue;
            }

            let forward = self.deribit.fetch_forward_price(&currency).await?;
            let surface = self.deribit.fetch_surface(&currency, now).await?;
            self.surface_cache.insert(
                currency.clone(),
                SurfaceCacheEntry {
                    forward,
                    surface,
                    fetched_at: now,
                },
            );

            debug!(
                currency,
                forward, "refreshed Deribit forward + volatility surface"
            );
        }

        Ok(())
    }

}

fn extract_markets_from_event(
    event: &GammaEventInfo,
    allowed_symbols: &HashSet<String>,
) -> Vec<DiscoveredMarket> {
    let mut out = Vec::new();
    let end_time = parse_event_end(event.end_date.as_ref()).unwrap_or_else(Utc::now);

    for market in &event.markets {
        let Some(condition_id) = market.condition_id.clone() else {
            continue;
        };

        let question = market
            .question
            .clone()
            .or_else(|| market.group_item_title.clone())
            .or_else(|| event.title.clone())
            .unwrap_or_default();
        if question.is_empty() {
            continue;
        }

        let parsed = parse_polymarket_question(&question).or_else(|| {
            event
                .title
                .as_ref()
                .and_then(|s| parse_polymarket_question(s))
        });
        let Some(parsed) = parsed else {
            continue;
        };

        let norm_symbol = normalize_symbol(&parsed.symbol);
        if !allowed_symbols.contains(&norm_symbol) {
            continue;
        }

        let token_ids = parse_string_array(market.clob_token_ids.as_ref());
        if token_ids.len() < 2 {
            continue;
        }
        let outcomes = parse_string_array(market.outcomes.as_ref());
        let yes_idx = infer_yes_index(&outcomes).min(token_ids.len() - 1);
        let no_idx = if yes_idx == 0 { 1 } else { 0 };
        if no_idx >= token_ids.len() {
            continue;
        }

        out.push(DiscoveredMarket {
            event_id: event.id.clone(),
            condition_id,
            symbol: norm_symbol,
            strike: parsed.strike,
            yes_token_id: token_ids[yes_idx].clone(),
            no_token_id: token_ids[no_idx].clone(),
            end_time,
            question: question.clone(),
        });
    }

    out
}

/// Run Deribit-implied probability arbitrage with automatic order execution.
pub async fn run_deribit_probability_arb(
    pm_client: PolymarketClient,
    executor: OrderExecutor,
    config: DeribitProbabilityArbConfig,
) -> Result<()> {
    let runner = DeribitProbabilityArbRunner::new(config, pm_client, executor);
    runner.run().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deribit_probability_arb_core_model() {
        let p = binary_call_prob_forward(100000.0, 98000.0, 0.55, 15.0 / 525600.0)
            .expect("probability should be computable");
        assert!(p > 0.5);
    }

    #[test]
    fn parse_polymarket_question_extracts_symbol_and_strike() {
        let parsed = parse_polymarket_question("Will Bitcoin be above $98,500 at 12:45 PM ET?")
            .expect("question should parse");
        assert_eq!(parsed.symbol, "BTC");
        assert_eq!(parsed.strike, Decimal::from(98_500u64));
    }

    #[test]
    fn interpolate_iv_linear_blends_variance_by_maturity() {
        let mut surface = VolSurfaceSnapshot {
            by_maturity: BTreeMap::new(),
            asof: Utc::now(),
        };
        surface
            .by_maturity
            .insert(OrderedFloat(0.01), vec![(100000.0, 0.50), (110000.0, 0.52)]);
        surface
            .by_maturity
            .insert(OrderedFloat(0.02), vec![(100000.0, 0.60), (110000.0, 0.62)]);

        let iv = interpolate_iv_linear(&surface, 0.015, 105000.0).expect("iv should interpolate");
        assert!(iv > 0.52 && iv < 0.60);
    }

    #[test]
    fn net_edge_is_model_minus_price_minus_fee() {
        let e = net_edge(0.62, 0.54, 0.02);
        assert!((e - 0.06).abs() < 1e-9);
    }

    #[test]
    fn parse_deribit_expiry_supports_standard_code() {
        let dt = parse_deribit_expiry("29MAR24").expect("expiry should parse");
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 3);
        assert_eq!(dt.day(), 29);
        assert_eq!(dt.hour(), 8);
    }
}
