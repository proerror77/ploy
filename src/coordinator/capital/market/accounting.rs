use rust_decimal::Decimal;
use std::collections::HashMap;
use uuid::Uuid;

use crate::coordinator::command::{AllocatorLedgerSnapshot, DeploymentLedgerSnapshot};
use crate::coordinator::OrderIntent;

use super::{
    intent_deployment_scope, intent_market_identity, sell_release_reference_price,
    MarketCapitalAllocator,
};

#[derive(Debug, Clone)]
pub(super) struct MarketIntentDimensions {
    pub(super) market_key: String,
    pub(super) deployment_scope: String,
    pub(super) position_key: String,
}

impl MarketIntentDimensions {
    pub(super) fn from_intent(intent: &OrderIntent) -> Self {
        let market_key = intent_market_identity(intent);
        let deployment_scope = intent_deployment_scope(intent);
        let position_key = format!(
            "{}|{}|{}|{}",
            deployment_scope,
            market_key,
            intent.token_id,
            intent.side.as_str()
        );
        Self {
            market_key,
            deployment_scope,
            position_key,
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::coordinator::capital) struct MarketPositionExposure {
    pub(in crate::coordinator::capital) market_key: String,
    pub(in crate::coordinator::capital) deployment_scope: String,
    pub(in crate::coordinator::capital) amount: Decimal,
}

#[derive(Debug, Default)]
pub(in crate::coordinator::capital) struct MarketExposureBook {
    pub(in crate::coordinator::capital) total: Decimal,
    pub(in crate::coordinator::capital) by_market: HashMap<String, Decimal>,
    pub(in crate::coordinator::capital) by_position: HashMap<String, MarketPositionExposure>,
}

impl MarketExposureBook {
    pub(super) fn value_for_market(&self, market_key: &str) -> Decimal {
        self.by_market
            .get(market_key)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    pub(super) fn add(&mut self, dims: &MarketIntentDimensions, amount: Decimal) {
        if amount <= Decimal::ZERO {
            return;
        }

        self.total += amount;
        *self
            .by_market
            .entry(dims.market_key.clone())
            .or_insert(Decimal::ZERO) += amount;
        self.by_position
            .entry(dims.position_key.clone())
            .and_modify(|pos| {
                pos.amount += amount;
                pos.market_key = dims.market_key.clone();
                pos.deployment_scope = dims.deployment_scope.clone();
            })
            .or_insert_with(|| MarketPositionExposure {
                market_key: dims.market_key.clone(),
                deployment_scope: dims.deployment_scope.clone(),
                amount,
            });
    }

    pub(super) fn subtract_from_position_key(
        &mut self,
        position_key: &str,
        amount: Decimal,
    ) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let mut removed = Decimal::ZERO;
        let mut market_key = None;
        let mut delete_key = false;

        if let Some(pos) = self.by_position.get_mut(position_key) {
            removed = amount.min(pos.amount);
            if removed > Decimal::ZERO {
                pos.amount -= removed;
                market_key = Some(pos.market_key.clone());
                delete_key = pos.amount <= Decimal::ZERO;
            }
        }

        if delete_key {
            self.by_position.remove(position_key);
        }

        if removed <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        self.total = (self.total - removed).max(Decimal::ZERO);
        if let Some(market) = market_key {
            if let Some(v) = self.by_market.get_mut(&market) {
                *v = (*v - removed).max(Decimal::ZERO);
                if *v == Decimal::ZERO {
                    self.by_market.remove(&market);
                }
            }
        }

        removed
    }

    pub(super) fn subtract_matching_market(
        &mut self,
        deployment_scope: &str,
        market_key: &str,
        amount: Decimal,
    ) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let mut remaining = amount;
        let keys: Vec<String> = self
            .by_position
            .iter()
            .filter(|(_, p)| p.deployment_scope == deployment_scope && p.market_key == market_key)
            .map(|(k, _)| k.clone())
            .collect();

        for key in keys {
            if remaining <= Decimal::ZERO {
                break;
            }
            let removed = self.subtract_from_position_key(&key, remaining);
            remaining -= removed;
        }

        amount - remaining
    }
}

#[derive(Debug, Clone)]
pub(super) struct PendingMarketIntent {
    pub(super) dims: MarketIntentDimensions,
    pub(super) requested_notional: Decimal,
}

impl MarketCapitalAllocator {
    pub(in crate::coordinator) fn reserve_buy(
        &mut self,
        intent: &OrderIntent,
    ) -> std::result::Result<(), String> {
        if !self.enabled || intent.domain != self.domain || !intent.is_buy {
            return Ok(());
        }

        if self.total_cap <= Decimal::ZERO {
            return Err(format!(
                "{} allocator cap is 0; buy intent blocked",
                self.domain_label
            ));
        }

        let requested = intent.notional_value();
        if requested <= Decimal::ZERO {
            return Err(format!(
                "{} buy intent has non-positive notional",
                self.domain_label
            ));
        }

        let dims = MarketIntentDimensions::from_intent(intent);

        let projected_total = self.open.total + self.pending.total + requested;
        if projected_total > self.total_cap {
            return Err(format!(
                "{} total cap exceeded: projected={} cap={}",
                self.domain_label, projected_total, self.total_cap
            ));
        }

        let market_cap = self.market_cap_for(&dims.market_key);
        let projected_market = self.open.value_for_market(&dims.market_key)
            + self.pending.value_for_market(&dims.market_key)
            + requested;
        if projected_market > market_cap {
            return Err(format!(
                "{} market cap exceeded: market={} projected={} cap={}",
                self.domain_label, dims.market_key, projected_market, market_cap
            ));
        }

        self.pending.add(&dims, requested);
        self.pending_by_intent.insert(
            intent.intent_id,
            PendingMarketIntent {
                dims,
                requested_notional: requested,
            },
        );

        Ok(())
    }

    pub(in crate::coordinator) fn release_buy_reservation(&mut self, intent_id: Uuid) {
        let Some(reservation) = self.pending_by_intent.remove(&intent_id) else {
            return;
        };
        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );
    }

    pub(in crate::coordinator) fn settle_buy_execution(
        &mut self,
        intent: &OrderIntent,
        filled_shares: u64,
        fill_price: Decimal,
    ) {
        if !self.enabled || intent.domain != self.domain || !intent.is_buy {
            return;
        }

        let reservation = self
            .pending_by_intent
            .remove(&intent.intent_id)
            .unwrap_or_else(|| PendingMarketIntent {
                dims: MarketIntentDimensions::from_intent(intent),
                requested_notional: intent.notional_value(),
            });

        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );

        if filled_shares == 0 || fill_price <= Decimal::ZERO {
            return;
        }

        let actual_notional = fill_price * Decimal::from(filled_shares);
        self.open.add(&reservation.dims, actual_notional);
    }

    pub(in crate::coordinator) fn settle_sell_execution(
        &mut self,
        intent: &OrderIntent,
        filled_shares: u64,
        execution_price: Decimal,
    ) {
        if !self.enabled || intent.domain != self.domain || intent.is_buy || filled_shares == 0 {
            return;
        }

        let dims = MarketIntentDimensions::from_intent(intent);
        let Some((reference_price, has_explicit_entry_price)) =
            sell_release_reference_price(intent, execution_price)
        else {
            return;
        };

        if reference_price <= Decimal::ZERO {
            return;
        }

        let requested_release = Decimal::from(filled_shares) * reference_price;
        let removed_by_key = self
            .open
            .subtract_from_position_key(&dims.position_key, requested_release);
        if has_explicit_entry_price && removed_by_key < requested_release {
            let remaining = requested_release - removed_by_key;
            self.open
                .subtract_matching_market(&dims.deployment_scope, &dims.market_key, remaining);
        }
    }

    fn market_cap_for(&self, market_key: &str) -> Decimal {
        let fixed_cap = self.total_cap * self.market_cap_pct;
        if !self.auto_split_by_active_markets {
            return fixed_cap;
        }

        let mut market_count = self
            .open
            .by_market
            .values()
            .filter(|value| **value > Decimal::ZERO)
            .count();

        for (pending_market, pending_amount) in &self.pending.by_market {
            if *pending_amount <= Decimal::ZERO {
                continue;
            }

            let has_open_exposure = self
                .open
                .by_market
                .get(pending_market)
                .copied()
                .unwrap_or(Decimal::ZERO)
                > Decimal::ZERO;
            if !has_open_exposure {
                market_count += 1;
            }
        }

        if !market_key.is_empty() {
            let has_existing_exposure = self
                .open
                .by_market
                .get(market_key)
                .copied()
                .unwrap_or(Decimal::ZERO)
                > Decimal::ZERO
                || self
                    .pending
                    .by_market
                    .get(market_key)
                    .copied()
                    .unwrap_or(Decimal::ZERO)
                    > Decimal::ZERO;
            if !has_existing_exposure {
                market_count += 1;
            }
        }

        let market_count = market_count.max(1) as u64;
        let dynamic_cap = self.total_cap / Decimal::from(market_count);
        dynamic_cap.min(fixed_cap)
    }

    pub(in crate::coordinator) fn ledger_snapshot(&self) -> AllocatorLedgerSnapshot {
        let open_notional_usd = self.open.total;
        let pending_notional_usd = self.pending.total;
        let used = open_notional_usd + pending_notional_usd;
        let available_notional_usd = (self.total_cap - used).max(Decimal::ZERO);
        AllocatorLedgerSnapshot {
            domain: self.domain_label.to_string(),
            enabled: self.enabled,
            cap_notional_usd: self.total_cap,
            open_notional_usd,
            pending_notional_usd,
            available_notional_usd,
        }
    }

    pub(in crate::coordinator) fn deployment_ledger_snapshot(
        &self,
    ) -> Vec<DeploymentLedgerSnapshot> {
        let mut by_deployment: HashMap<String, (Decimal, Decimal)> = HashMap::new();

        for position in self.open.by_position.values() {
            let entry = by_deployment
                .entry(position.deployment_scope.clone())
                .or_insert((Decimal::ZERO, Decimal::ZERO));
            entry.0 += position.amount;
        }

        for position in self.pending.by_position.values() {
            let entry = by_deployment
                .entry(position.deployment_scope.clone())
                .or_insert((Decimal::ZERO, Decimal::ZERO));
            entry.1 += position.amount;
        }

        let mut rows = by_deployment
            .into_iter()
            .map(
                |(deployment_id, (open_notional_usd, pending_notional_usd))| {
                    DeploymentLedgerSnapshot {
                        deployment_id,
                        domain: self.domain_label.to_string(),
                        open_notional_usd,
                        pending_notional_usd,
                        total_notional_usd: open_notional_usd + pending_notional_usd,
                    }
                },
            )
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| a.deployment_id.cmp(&b.deployment_id));
        rows
    }

    pub(in crate::coordinator) fn available_notional_for(
        &self,
        intent: &OrderIntent,
    ) -> Option<Decimal> {
        if !self.enabled || intent.domain != self.domain || !intent.is_buy {
            return None;
        }

        if self.total_cap <= Decimal::ZERO {
            return Some(Decimal::ZERO);
        }

        let dims = MarketIntentDimensions::from_intent(intent);
        let remaining_total =
            (self.total_cap - self.open.total - self.pending.total).max(Decimal::ZERO);

        let market_cap = self.market_cap_for(&dims.market_key);
        let projected_market = self.open.value_for_market(&dims.market_key)
            + self.pending.value_for_market(&dims.market_key);
        let remaining_market = (market_cap - projected_market).max(Decimal::ZERO);

        Some(remaining_total.min(remaining_market))
    }
}
