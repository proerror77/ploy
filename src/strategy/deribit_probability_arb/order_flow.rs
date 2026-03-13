//! Order evaluation, sizing, and execution flow for Deribit probability arbitrage.

use super::{
    DeribitProbabilityArbConfig, DeribitProbabilityArbRunner, DiscoveredMarket, OpenExposure,
    OrderExecutor, PloyError, Result, SECONDS_PER_YEAR, SurfaceCacheEntry,
    binary_call_prob_forward, interpolate_iv_linear, kelly_fraction_binary, net_edge,
    normalize_symbol, spread_bps, symbol_to_deribit_currency,
};
use crate::domain::{OrderRequest, OrderSide, OrderType, Side, TimeInForce};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use tracing::info;

#[cfg(test)]
use crate::adapters::PolymarketClient;
#[cfg(test)]
use crate::config::ExecutionConfig;

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

impl DeribitProbabilityArbRunner {
    pub(super) async fn process_market(
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

        let Some(candidate) =
            self.build_candidate(market, remaining_secs, cache, yes_bid, yes_ask, no_bid, no_ask)?
        else {
            return Ok(());
        };

        let Some(shares) = self.compute_shares(&candidate) else {
            return Ok(());
        };

        let request = self.build_order_request(&candidate, shares);
        self.last_trade_at.insert(market.condition_id.clone(), now);

        let order_res = self.executor.execute(&request).await?;
        let filled_shares = order_res.filled_shares.max(0);

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

    fn build_candidate(
        &self,
        market: &DiscoveredMarket,
        remaining_secs: u64,
        cache: &SurfaceCacheEntry,
        yes_bid: Option<Decimal>,
        yes_ask: Option<Decimal>,
        no_bid: Option<Decimal>,
        no_ask: Option<Decimal>,
    ) -> Result<Option<CandidateTrade>> {
        let Some(yes_ask) = yes_ask else {
            return Ok(None);
        };
        let Some(no_ask) = no_ask else {
            return Ok(None);
        };

        if spread_bps(yes_bid, Some(yes_ask)).is_some_and(|s| s > self.config.max_spread_bps)
            || spread_bps(no_bid, Some(no_ask)).is_some_and(|s| s > self.config.max_spread_bps)
        {
            return Ok(None);
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
            return Ok(None);
        }

        let yes_edge_f = net_edge(p_yes, yes_ask_f, fee_buffer);
        let no_edge_f = net_edge(1.0 - p_yes, no_ask_f, fee_buffer);
        let min_edge_f = self.config.min_edge.to_f64().unwrap_or(0.03);

        let candidate = if yes_edge_f >= no_edge_f {
            if yes_edge_f < min_edge_f {
                return Ok(None);
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
                return Ok(None);
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

        Ok(Some(candidate))
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

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn test_runner() -> DeribitProbabilityArbRunner {
        let client = PolymarketClient::new("https://clob.polymarket.com", true)
            .expect("dry-run Polymarket client");
        let executor = OrderExecutor::new(client.clone(), ExecutionConfig::default());
        DeribitProbabilityArbRunner::new(DeribitProbabilityArbConfig::default(), client, executor)
    }

    fn sample_market() -> DiscoveredMarket {
        DiscoveredMarket {
            event_id: "event-1".to_string(),
            condition_id: "condition-1".to_string(),
            symbol: "BTC".to_string(),
            strike: Decimal::from(100_000u64),
            yes_token_id: "yes-token".to_string(),
            no_token_id: "no-token".to_string(),
            end_time: Utc::now() + chrono::Duration::minutes(5),
            question: "Will Bitcoin be above $100,000 at 12:45 PM ET?".to_string(),
        }
    }

    fn sample_candidate() -> CandidateTrade {
        CandidateTrade {
            market: sample_market(),
            buy_yes: true,
            ask_price: dec!(0.45),
            side_probability: 0.60,
            model_yes_probability: 0.60,
            edge: dec!(0.13),
            iv: 0.52,
            time_remaining_secs: 300,
        }
    }

    #[test]
    fn compute_shares_respects_symbol_exposure_budget() {
        let mut runner = test_runner();
        runner
            .symbol_exposure_usd
            .insert("BTC".to_string(), dec!(149));

        assert_eq!(runner.compute_shares(&sample_candidate()), None);
    }

    #[test]
    fn build_order_request_uses_canonical_deribit_prob_fields() {
        let runner = test_runner();
        let request = runner.build_order_request(&sample_candidate(), 12);

        assert!(request.client_order_id.starts_with("intent:deribit-prob:condition-1:"));
        assert!(
            request
                .idempotency_key
                .as_deref()
                .is_some_and(|key| key.starts_with("deribit-prob:condition-1:"))
        );
        assert_eq!(request.token_id, "yes-token");
        assert_eq!(request.market_side, Side::Up);
        assert_eq!(request.order_side, OrderSide::Buy);
        assert_eq!(request.shares, 12);
        assert_eq!(request.limit_price, dec!(0.45));
        assert_eq!(request.order_type, OrderType::Limit);
        assert_eq!(request.time_in_force, TimeInForce::IOC);
    }
}
