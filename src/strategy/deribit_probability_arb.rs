//! Deribit-implied probability arbitrage for Polymarket up/down binaries.
//!
//! Core idea:
//! - Use Deribit options to infer a short-horizon risk-neutral probability `P(UP)`.
//! - Compare to Polymarket YES/NO best asks.
//! - Execute buys only when net edge (after fee buffer) is positive and above threshold.

use crate::adapters::{GammaEventInfo, PolymarketClient};
use crate::domain::{OrderRequest, OrderSide, OrderType, Side, TimeInForce};
use crate::error::{PloyError, Result};
use crate::strategy::execution::executor::OrderExecutor;
use chrono::{DateTime, Utc};
#[cfg(test)]
use chrono::{Datelike, Timelike};
#[cfg(test)]
use ordered_float::OrderedFloat;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tracing::{debug, info, warn};

#[path = "deribit_probability_arb_support.rs"]
mod support;

#[cfg(test)]
use support::parse_deribit_expiry;
pub use support::{
    binary_call_prob_forward, interpolate_iv_linear, net_edge, norm_cdf, parse_polymarket_question,
    ParsedPolymarketQuestion, SurfacePoint, VolSurfaceSnapshot,
};
use support::{
    infer_yes_index, kelly_fraction_binary, normalize_symbol, parse_event_end, parse_string_array,
    spread_bps, symbol_to_deribit_currency, DeribitPublicClient, DERIBIT_PUBLIC_API,
    SECONDS_PER_YEAR,
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

#[derive(Debug, Clone)]
struct CandidateTrade {
    market: DiscoveredMarket,
    buy_yes: bool,
    ask_price: Decimal,
    side_probability: f64,
    model_yes_probability: f64,
    edge: Decimal,
    iv: f64,
    time_remaining_secs: u64,
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

    async fn process_market(
        &mut self,
        now: DateTime<Utc>,
        market: &DiscoveredMarket,
    ) -> Result<()> {
        let remaining = (market.end_time - now).num_seconds();
        if remaining <= 0 {
            return Ok(());
        }
        let remaining_secs = remaining as u64;

        if remaining_secs < self.config.min_time_remaining_secs
            || remaining_secs > self.config.max_time_remaining_secs
        {
            return Ok(());
        }

        if self
            .open_exposure_by_condition
            .contains_key(&market.condition_id)
        {
            return Ok(());
        }

        if let Some(last_trade) = self.last_trade_at.get(&market.condition_id) {
            if (now - *last_trade).num_seconds() < self.config.cooldown_secs as i64 {
                return Ok(());
            }
        }

        let Some(currency) = symbol_to_deribit_currency(&market.symbol) else {
            return Ok(());
        };
        let Some(cache) = self.surface_cache.get(currency) else {
            return Ok(());
        };

        let (yes_bid, yes_ask) = self.pm_client.get_best_prices(&market.yes_token_id).await?;
        let (no_bid, no_ask) = self.pm_client.get_best_prices(&market.no_token_id).await?;

        let Some(yes_ask) = yes_ask else {
            return Ok(());
        };
        let Some(no_ask) = no_ask else {
            return Ok(());
        };

        if spread_bps(yes_bid, Some(yes_ask)).is_some_and(|s| s > self.config.max_spread_bps)
            || spread_bps(no_bid, Some(no_ask)).is_some_and(|s| s > self.config.max_spread_bps)
        {
            return Ok(());
        }

        let t_years = (remaining_secs as f64) / SECONDS_PER_YEAR;
        let strike = market
            .strike
            .to_f64()
            .ok_or_else(|| PloyError::InvalidMarketData("invalid strike".to_string()))?;

        let iv = interpolate_iv_linear(&cache.surface, t_years, strike)
            .ok_or_else(|| PloyError::MarketDataUnavailable("cannot interpolate IV".to_string()))?;
        let p_yes =
            binary_call_prob_forward(cache.forward, strike, iv, t_years).ok_or_else(|| {
                PloyError::MarketDataUnavailable(
                    "cannot compute Deribit model probability".to_string(),
                )
            })?;

        let fee_buffer = self.config.fee_buffer.to_f64().unwrap_or(0.02);
        let yes_ask_f = yes_ask.to_f64().unwrap_or(0.0);
        let no_ask_f = no_ask.to_f64().unwrap_or(0.0);
        if yes_ask_f <= 0.0 || no_ask_f <= 0.0 {
            return Ok(());
        }

        let yes_edge_f = net_edge(p_yes, yes_ask_f, fee_buffer);
        let no_edge_f = net_edge(1.0 - p_yes, no_ask_f, fee_buffer);
        let min_edge_f = self.config.min_edge.to_f64().unwrap_or(0.03);

        let candidate = if yes_edge_f >= no_edge_f {
            if yes_edge_f < min_edge_f {
                return Ok(());
            }
            CandidateTrade {
                market: market.clone(),
                buy_yes: true,
                ask_price: yes_ask,
                side_probability: p_yes,
                model_yes_probability: p_yes,
                edge: Decimal::from_f64(yes_edge_f).unwrap_or(Decimal::ZERO),
                iv,
                time_remaining_secs: remaining_secs,
            }
        } else {
            if no_edge_f < min_edge_f {
                return Ok(());
            }
            CandidateTrade {
                market: market.clone(),
                buy_yes: false,
                ask_price: no_ask,
                side_probability: 1.0 - p_yes,
                model_yes_probability: p_yes,
                edge: Decimal::from_f64(no_edge_f).unwrap_or(Decimal::ZERO),
                iv,
                time_remaining_secs: remaining_secs,
            }
        };

        let Some(shares) = self.compute_shares(&candidate) else {
            return Ok(());
        };

        let request = self.build_order_request(&candidate, shares);
        self.last_trade_at.insert(market.condition_id.clone(), now);

        let order_res = self.executor.execute(&request).await?;
        let filled_shares = if order_res.filled_shares > 0 {
            order_res.filled_shares
        } else {
            0
        };

        if filled_shares > 0 {
            let notional = request.limit_price * Decimal::from(filled_shares);
            let symbol_key = normalize_symbol(&candidate.market.symbol);
            let cur = self
                .symbol_exposure_usd
                .get(&symbol_key)
                .copied()
                .unwrap_or(Decimal::ZERO);
            self.symbol_exposure_usd.insert(symbol_key, cur + notional);
            self.open_exposure_by_condition.insert(
                candidate.market.condition_id.clone(),
                OpenExposure {
                    symbol: candidate.market.symbol.clone(),
                    notional_usd: notional,
                    expires_at: candidate.market.end_time,
                },
            );
        }

        info!(
            condition_id = candidate.market.condition_id,
            symbol = candidate.market.symbol,
            strike = %candidate.market.strike,
            side = if candidate.buy_yes { "YES" } else { "NO" },
            ask = %candidate.ask_price,
            shares,
            edge = %candidate.edge,
            p_yes = candidate.model_yes_probability,
            side_prob = candidate.side_probability,
            iv = candidate.iv,
            t_sec = candidate.time_remaining_secs,
            order_id = order_res.order_id,
            status = ?order_res.status,
            filled_shares = order_res.filled_shares,
            "executed Deribit probability arbitrage candidate"
        );

        Ok(())
    }

    fn compute_shares(&self, candidate: &CandidateTrade) -> Option<u64> {
        let entry = candidate.ask_price.to_f64()?;
        if !(0.0..1.0).contains(&entry) {
            return None;
        }

        let raw_kelly = kelly_fraction_binary(candidate.side_probability, entry);
        let allocation =
            (raw_kelly * self.config.kelly_fraction).clamp(self.config.min_kelly_allocation, 1.0);

        let symbol_key = normalize_symbol(&candidate.market.symbol);
        let used = self
            .symbol_exposure_usd
            .get(&symbol_key)
            .copied()
            .unwrap_or(Decimal::ZERO);
        let available_symbol = (self.config.max_symbol_exposure_usd - used).max(Decimal::ZERO);
        if available_symbol <= Decimal::ZERO {
            return None;
        }

        let max_notional = self.config.max_trade_usd.min(available_symbol);
        if max_notional < self.config.min_trade_usd {
            return None;
        }

        let target_notional = Decimal::from_f64(max_notional.to_f64()? * allocation)?;
        if target_notional < self.config.min_trade_usd {
            return None;
        }

        let shares = (target_notional / candidate.ask_price).floor().to_u64()?;
        if shares < self.config.min_shares {
            return None;
        }

        Some(shares)
    }

    fn build_order_request(&self, candidate: &CandidateTrade, shares: u64) -> OrderRequest {
        let now_ms = Utc::now().timestamp_millis();
        let condition = &candidate.market.condition_id;
        OrderRequest {
            client_order_id: format!("intent:deribit-prob:{}:{}", condition, now_ms),
            idempotency_key: Some(format!("deribit-prob:{}:{}", condition, now_ms)),
            token_id: if candidate.buy_yes {
                candidate.market.yes_token_id.clone()
            } else {
                candidate.market.no_token_id.clone()
            },
            market_side: if candidate.buy_yes {
                Side::Up
            } else {
                Side::Down
            },
            order_side: OrderSide::Buy,
            shares,
            limit_price: candidate.ask_price,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::IOC,
        }
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
