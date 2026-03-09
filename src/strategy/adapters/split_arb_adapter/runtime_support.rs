use super::*;
use rust_decimal_macros::dec;

/// Spawn a CTF merge transaction to convert YES+NO token pair back to USDC.
///
/// This is a best-effort operation. If it fails, the claimer daemon will
/// pick up the unmerged positions later during its periodic scan.
#[cfg(feature = "pm_ctf")]
async fn spawn_ctf_merge(condition_id: &str, shares: u64) -> std::result::Result<String, String> {
    use alloy::primitives::{B256, U256};
    use polymarket_client_sdk::ctf::types::MergePositionsRequest;
    use std::str::FromStr;

    let usdc: alloy::primitives::Address = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174"
        .parse()
        .map_err(|e| format!("{e}"))?;
    let cid = B256::from_str(condition_id).map_err(|e| format!("invalid condition_id: {e}"))?;
    let amount = U256::from(shares) * U256::from(1_000_000u64);

    let pk = std::env::var("POLYMARKET_PRIVATE_KEY")
        .map_err(|_| "POLYMARKET_PRIVATE_KEY not set".to_string())?;

    let signer: alloy::signers::local::PrivateKeySigner = pk
        .parse()
        .map_err(|e| format!("invalid private key: {e}"))?;
    let wallet = alloy::network::EthereumWallet::from(signer);

    let rpc_url =
        std::env::var("POLYGON_RPC_URL").unwrap_or_else(|_| "https://polygon-rpc.com".to_string());

    let provider = alloy::providers::ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url.parse().map_err(|e| format!("bad RPC URL: {e}"))?);

    let client = polymarket_client_sdk::ctf::Client::new(provider, polymarket_client_sdk::POLYGON)
        .map_err(|e| format!("CTF client init: {e}"))?;

    let request = MergePositionsRequest::for_binary_market(usdc, cid, amount);
    let resp = client
        .merge_positions(&request)
        .await
        .map_err(|e| format!("merge tx failed: {e}"))?;

    Ok(format!("{:?}", resp.transaction_hash))
}

#[cfg(not(feature = "pm_ctf"))]
async fn spawn_ctf_merge(
    _condition_id: &str,
    _shares: u64,
) -> std::result::Result<String, String> {
    Err("CTF merge not available (pm_ctf feature disabled)".to_string())
}

impl SplitArbStrategyAdapter {
    pub(super) async fn check_opportunity(&self, market_id: &str) -> Option<(Side, Decimal)> {
        let markets = self.markets.read().await;
        let market = markets.get(market_id)?;

        let prices = self.prices.read().await;
        let (_yes_bid, yes_ask) = prices.get(&market.yes_token_id)?;
        let (_no_bid, no_ask) = prices.get(&market.no_token_id)?;

        let yes_ask = (*yes_ask)?;
        let no_ask = (*no_ask)?;

        let total_cost = yes_ask + no_ask;
        let fee_cost = total_cost * self.config.fee_rate;
        if total_cost + fee_cost < dec!(1.0)
            && (dec!(1.0) - total_cost - fee_cost) >= self.config.min_profit_margin
        {
            if yes_ask <= no_ask && yes_ask <= self.config.max_entry_price {
                return Some((Side::Up, yes_ask));
            } else if no_ask <= self.config.max_entry_price {
                return Some((Side::Down, no_ask));
            }
        }

        None
    }

    pub(super) async fn generate_first_leg(
        &self,
        market_id: &str,
        side: Side,
        price: Decimal,
    ) -> Option<StrategyAction> {
        let markets = self.markets.read().await;
        let market = markets.get(market_id)?;

        let token_id = match side {
            Side::Up => market.yes_token_id.clone(),
            Side::Down => market.no_token_id.clone(),
        };

        let client_order_id = format!(
            "{}_leg1_{}_{}",
            self.id,
            market_id,
            Utc::now().timestamp_millis()
        );

        let shares = if let Some(amount_usd) = self.fixed_amount_usd {
            let price_f64 = price.to_string().parse::<f64>().unwrap_or(0.5);
            if price_f64 > 0.0 {
                (amount_usd / price_f64).floor() as u64
            } else {
                self.config.shares_per_trade
            }
        } else {
            self.config.shares_per_trade
        };

        info!(
            "[{}] First leg entry: {} @ {:.2}¢ ({} shares, ${:.2})",
            self.id,
            if side == Side::Up { "YES" } else { "NO" },
            price * dec!(100),
            shares,
            price.to_string().parse::<f64>().unwrap_or(0.0) * shares as f64,
        );

        {
            let mut map = self.order_market_map.write().await;
            map.insert(client_order_id.clone(), (market_id.to_string(), side));
        }

        Some(super::crypto_submit_intent(
            client_order_id,
            market_id.to_string(),
            token_id,
            side,
            true,
            shares,
            price,
            10,
        ))
    }

    pub(super) async fn handle_order_update(
        &mut self,
        update: &OrderUpdate,
    ) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        match update.status {
            crate::domain::OrderStatus::Filled => {
                info!(
                    "[{}] Order filled: {} @ {:?}",
                    self.id, update.order_id, update.avg_fill_price
                );

                let order_key = update
                    .client_order_id
                    .clone()
                    .unwrap_or_else(|| update.order_id.clone());
                let mapping = {
                    let map = self.order_market_map.read().await;
                    map.get(&order_key).cloned()
                };

                if let Some((market_id, side)) = mapping {
                    let fill_price = update.avg_fill_price.unwrap_or(Decimal::ZERO);
                    let has_partial = {
                        let partials = self.partial_positions.read().await;
                        partials.contains_key(&market_id)
                    };

                    if !has_partial {
                        let markets = self.markets.read().await;
                        let token_id = markets
                            .get(&market_id)
                            .map(|m| match side {
                                Side::Up => m.yes_token_id.clone(),
                                Side::Down => m.no_token_id.clone(),
                            })
                            .unwrap_or_default();
                        drop(markets);

                        let partial = SplitPosition {
                            market_id: market_id.clone(),
                            first_side: side,
                            first_token_id: token_id,
                            shares: update.filled_qty,
                            entry_price: fill_price,
                            opened_at: Utc::now(),
                            order_id: Some(order_key.clone()),
                            hedge_retries: 0,
                        };

                        let mut partials = self.partial_positions.write().await;
                        partials.insert(market_id.clone(), partial);

                        self.pending_leg1_markets.write().await.remove(&market_id);

                        let mut stats = self.stats.write().await;
                        stats.first_leg_entries += 1;

                        info!(
                            "[{}] First leg tracked: {} {} @ {:.2}c",
                            self.id,
                            market_id,
                            if side == Side::Up { "YES" } else { "NO" },
                            fill_price * dec!(100)
                        );
                    } else {
                        let mut partials = self.partial_positions.write().await;
                        if let Some(partial) = partials.remove(&market_id) {
                            let total_cost = partial.entry_price + fill_price;
                            let fee_cost = total_cost * self.config.fee_rate;
                            let profit = dec!(1.0) - total_cost - fee_cost;

                            let markets = self.markets.read().await;
                            let (yes_token, no_token, yes_price, no_price) =
                                if let Some(m) = markets.get(&market_id) {
                                    match partial.first_side {
                                        Side::Up => (
                                            m.yes_token_id.clone(),
                                            m.no_token_id.clone(),
                                            partial.entry_price,
                                            fill_price,
                                        ),
                                        Side::Down => (
                                            m.yes_token_id.clone(),
                                            m.no_token_id.clone(),
                                            fill_price,
                                            partial.entry_price,
                                        ),
                                    }
                                } else {
                                    (
                                        String::new(),
                                        String::new(),
                                        partial.entry_price,
                                        fill_price,
                                    )
                                };
                            drop(markets);

                            let hedged = HedgedSplitPosition {
                                market_id: market_id.clone(),
                                yes_token_id: yes_token,
                                no_token_id: no_token,
                                shares: partial.shares,
                                yes_price,
                                no_price,
                                total_cost,
                                profit_locked: profit,
                                opened_at: partial.opened_at,
                            };

                            let mut hedged_positions = self.hedged_positions.write().await;
                            hedged_positions.push(hedged);

                            let mut stats = self.stats.write().await;
                            stats.hedges_completed += 1;
                            stats.total_profit += profit * Decimal::from(partial.shares);

                            info!(
                                "[{}] Hedge complete: {} cost={:.2}c profit={:.2}c/share ({} shares)",
                                self.id, market_id,
                                total_cost * dec!(100),
                                profit * dec!(100),
                                partial.shares,
                            );

                            let markets = self.markets.read().await;
                            let merge_condition_id =
                                markets.get(&market_id).and_then(|m| m.condition_id.clone());
                            drop(markets);

                            if let Some(cid) = merge_condition_id {
                                let merge_shares = partial.shares;
                                let merge_market_id = market_id.clone();
                                let strategy_id = self.id.clone();
                                let dry_run = self.dry_run;
                                tokio::spawn(async move {
                                    if dry_run {
                                        info!(
                                            "[{}] CTF merge skipped (dry-run): {} shares for {}",
                                            strategy_id, merge_shares, merge_market_id
                                        );
                                        return;
                                    }
                                    match spawn_ctf_merge(&cid, merge_shares).await {
                                        Ok(tx_hash) => {
                                            info!(
                                                "[{}] CTF merge successful: {} shares for {} (tx: {})",
                                                strategy_id, merge_shares, merge_market_id, tx_hash
                                            );
                                        }
                                        Err(e) => {
                                            warn!(
                                                "[{}] CTF merge failed for {} ({} shares): {} — claimer will pick up later",
                                                strategy_id, merge_market_id, merge_shares, e
                                            );
                                        }
                                    }
                                });
                            } else {
                                warn!(
                                    "[{}] No condition_id for market {} — skipping auto-merge, claimer will handle",
                                    self.id, market_id
                                );
                            }
                        }
                    }

                    let mut map = self.order_market_map.write().await;
                    map.remove(&order_key);
                }

                actions.push(StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::OrderFilled,
                        format!("Split arb leg filled: {}", update.order_id),
                    ),
                });
            }
            crate::domain::OrderStatus::Cancelled | crate::domain::OrderStatus::Failed => {
                warn!(
                    "[{}] Order {} - {:?}",
                    self.id, update.order_id, update.error
                );
                let order_key = update
                    .client_order_id
                    .clone()
                    .unwrap_or_else(|| update.order_id.clone());
                let mapping = self.order_market_map.read().await.get(&order_key).cloned();

                if let Some((market_id, _side)) = mapping {
                    let is_hedge_failure = {
                        let partials = self.partial_positions.read().await;
                        partials.contains_key(&market_id)
                    };

                    if is_hedge_failure {
                        const MAX_HEDGE_RETRIES: u32 = 3;
                        let mut partials = self.partial_positions.write().await;
                        let should_exit = if let Some(pos) = partials.get_mut(&market_id) {
                            pos.hedge_retries += 1;
                            warn!(
                                "[{}] Hedge retry {}/{} for {}",
                                self.id, pos.hedge_retries, MAX_HEDGE_RETRIES, market_id
                            );
                            pos.hedge_retries >= MAX_HEDGE_RETRIES
                        } else {
                            false
                        };

                        if should_exit {
                            if let Some(pos) = partials.remove(&market_id) {
                                warn!(
                                    "[{}] Removing orphaned partial for {} after {} hedge failures",
                                    self.id, market_id, MAX_HEDGE_RETRIES
                                );

                                let urgency_buffer = dec!(0.01);
                                let exit_price = pos.entry_price - urgency_buffer;
                                let exit_price = if exit_price < dec!(0.01) {
                                    dec!(0.01)
                                } else {
                                    exit_price
                                };

                                let client_order_id = format!(
                                    "{}_orphan_exit_{}_{}",
                                    self.id,
                                    market_id,
                                    Utc::now().timestamp_millis()
                                );

                                actions.push(super::crypto_submit_intent(
                                    client_order_id,
                                    market_id.clone(),
                                    pos.first_token_id.clone(),
                                    pos.first_side,
                                    false,
                                    pos.shares,
                                    exit_price,
                                    15,
                                ));

                                let mut stats = self.stats.write().await;
                                stats.unhedged_exits += 1;
                            }
                        }
                    } else {
                        self.pending_leg1_markets.write().await.remove(&market_id);
                    }
                }

                self.order_market_map.write().await.remove(&order_key);
            }
            _ => {}
        }

        Ok(actions)
    }

    pub(super) async fn handle_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();
        let mut timed_out = Vec::new();
        {
            let partials = self.partial_positions.read().await;
            for (market_id, pos) in partials.iter() {
                let elapsed = (now - pos.opened_at).num_seconds() as u64;
                if elapsed > self.config.max_hedge_wait_secs {
                    timed_out.push(market_id.clone());
                }
            }
        }

        for market_id in timed_out {
            warn!(
                "[{}] Hedge timeout for {}, exiting unhedged",
                self.id, market_id
            );

            let mut partials = self.partial_positions.write().await;
            if let Some(pos) = partials.remove(&market_id) {
                let mut stats = self.stats.write().await;
                stats.unhedged_exits += 1;

                let urgency_buffer = dec!(0.01);
                let exit_price = pos.entry_price - urgency_buffer;
                let exit_price = if exit_price < dec!(0.01) {
                    dec!(0.01)
                } else {
                    exit_price
                };

                let client_order_id =
                    format!("{}_exit_{}_{}", self.id, market_id, now.timestamp_millis());

                info!(
                    "[{}] Unhedged exit: {} {} @ {:.2}c ({} shares)",
                    self.id,
                    market_id,
                    if pos.first_side == Side::Up { "YES" } else { "NO" },
                    exit_price * dec!(100),
                    pos.shares,
                );

                actions.push(super::crypto_submit_intent(
                    client_order_id,
                    market_id.clone(),
                    pos.first_token_id.clone(),
                    pos.first_side,
                    false,
                    pos.shares,
                    exit_price,
                    8,
                ));

                actions.push(StrategyAction::Alert {
                    level: AlertLevel::Warning,
                    message: format!("Unhedged exit: {}", market_id),
                });
            }
        }

        Ok(actions)
    }
}
