use std::collections::HashMap;

use rust_decimal::Decimal;
use uuid::Uuid;

use crate::coordinator::command::{AllocatorLedgerSnapshot, DeploymentLedgerSnapshot};
use crate::coordinator::OrderIntent;
use crate::platform::Domain;

use super::super::sell_release_reference_price;
use super::{CryptoCapitalAllocator, CryptoHorizon, CryptoIntentDimensions};

#[derive(Debug, Clone)]
pub(in crate::coordinator::capital) struct PositionExposure {
    deployment_scope: String,
    coin: String,
    horizon: CryptoHorizon,
    amount: Decimal,
}

#[derive(Debug, Default)]
pub(in crate::coordinator::capital) struct ExposureBook {
    pub(in crate::coordinator::capital) total: Decimal,
    pub(in crate::coordinator::capital) by_coin: HashMap<String, Decimal>,
    pub(in crate::coordinator::capital) by_horizon: HashMap<CryptoHorizon, Decimal>,
    pub(in crate::coordinator::capital) by_position: HashMap<String, PositionExposure>,
}

impl ExposureBook {
    pub(super) fn value_for_coin(&self, coin: &str) -> Decimal {
        self.by_coin.get(coin).copied().unwrap_or(Decimal::ZERO)
    }

    pub(super) fn value_for_horizon(&self, horizon: CryptoHorizon) -> Decimal {
        self.by_horizon
            .get(&horizon)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    pub(super) fn add(&mut self, dims: &CryptoIntentDimensions, amount: Decimal) {
        if amount <= Decimal::ZERO {
            return;
        }

        self.total += amount;
        *self
            .by_coin
            .entry(dims.coin.clone())
            .or_insert(Decimal::ZERO) += amount;
        *self.by_horizon.entry(dims.horizon).or_insert(Decimal::ZERO) += amount;
        self.by_position
            .entry(dims.position_key.clone())
            .and_modify(|pos| {
                pos.amount += amount;
                pos.deployment_scope = dims.deployment_scope.clone();
                pos.coin = dims.coin.clone();
                pos.horizon = dims.horizon;
            })
            .or_insert_with(|| PositionExposure {
                deployment_scope: dims.deployment_scope.clone(),
                coin: dims.coin.clone(),
                horizon: dims.horizon,
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
        let mut coin = None;
        let mut horizon = None;
        let mut delete_key = false;

        if let Some(pos) = self.by_position.get_mut(position_key) {
            removed = amount.min(pos.amount);
            if removed > Decimal::ZERO {
                pos.amount -= removed;
                coin = Some(pos.coin.clone());
                horizon = Some(pos.horizon);
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

        if let Some(coin) = coin {
            if let Some(value) = self.by_coin.get_mut(&coin) {
                *value = (*value - removed).max(Decimal::ZERO);
                if *value == Decimal::ZERO {
                    self.by_coin.remove(&coin);
                }
            }
        }

        if let Some(horizon) = horizon {
            if let Some(value) = self.by_horizon.get_mut(&horizon) {
                *value = (*value - removed).max(Decimal::ZERO);
                if *value == Decimal::ZERO {
                    self.by_horizon.remove(&horizon);
                }
            }
        }

        removed
    }

    pub(super) fn subtract_matching_bucket(
        &mut self,
        deployment_scope: &str,
        coin: &str,
        horizon: CryptoHorizon,
        amount: Decimal,
    ) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let mut remaining = amount;
        let keys: Vec<String> = self
            .by_position
            .iter()
            .filter(|(_, pos)| {
                pos.deployment_scope == deployment_scope
                    && pos.coin == coin
                    && pos.horizon == horizon
            })
            .map(|(key, _)| key.clone())
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
pub(super) struct PendingCryptoIntent {
    pub(super) dims: CryptoIntentDimensions,
    pub(super) requested_notional: Decimal,
}

impl CryptoCapitalAllocator {
    pub(in crate::coordinator) fn reset_runtime_state(&mut self) {
        self.open = ExposureBook::default();
        self.pending = ExposureBook::default();
        self.pending_by_intent.clear();
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
        if !self.enabled || intent.domain != Domain::Crypto || !intent.is_buy {
            return;
        }

        let reservation = self
            .pending_by_intent
            .remove(&intent.intent_id)
            .unwrap_or_else(|| PendingCryptoIntent {
                dims: CryptoIntentDimensions::from_intent(intent),
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
        if !self.enabled || intent.domain != Domain::Crypto || intent.is_buy || filled_shares == 0 {
            return;
        }

        let dims = CryptoIntentDimensions::from_intent(intent);
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
            self.open.subtract_matching_bucket(
                &dims.deployment_scope,
                &dims.coin,
                dims.horizon,
                remaining,
            );
        }
    }

    pub(in crate::coordinator) fn open_notional(&self) -> Decimal {
        self.open.total
    }

    pub(in crate::coordinator) fn pending_notional(&self) -> Decimal {
        self.pending.total
    }

    pub(in crate::coordinator) fn ledger_snapshot(&self) -> AllocatorLedgerSnapshot {
        let open_notional_usd = self.open.total;
        let pending_notional_usd = self.pending.total;
        let used = open_notional_usd + pending_notional_usd;
        let available_notional_usd = (self.total_cap - used).max(Decimal::ZERO);
        AllocatorLedgerSnapshot {
            domain: "crypto".to_string(),
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
                        domain: "crypto".to_string(),
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
}
