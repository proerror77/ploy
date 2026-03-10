use rust_decimal::Decimal;
use std::collections::HashMap;
use uuid::Uuid;

use crate::coordinator::command::{AllocatorLedgerSnapshot, DeploymentLedgerSnapshot};
use crate::coordinator::config::CoordinatorConfig;
use crate::platform::{Domain, OrderIntent};

use super::{
    intent_deployment_scope, intent_market_identity, sell_release_reference_price,
    KNOWN_15M_SERIES_IDS, KNOWN_5M_SERIES_IDS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::coordinator) enum CryptoHorizon {
    M5,
    M15,
    Other,
}

impl CryptoHorizon {
    pub(in crate::coordinator) fn as_str(&self) -> &'static str {
        match self {
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::Other => "other",
        }
    }

    pub(in crate::coordinator) fn from_hint(raw: &str) -> Option<Self> {
        let normalized = raw.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return None;
        }
        if normalized.contains("15m") || normalized == "15" {
            return Some(Self::M15);
        }
        if normalized.contains("5m") || normalized == "5" {
            return Some(Self::M5);
        }
        if KNOWN_15M_SERIES_IDS.iter().any(|id| *id == normalized) {
            return Some(Self::M15);
        }
        if KNOWN_5M_SERIES_IDS.iter().any(|id| *id == normalized) {
            return Some(Self::M5);
        }
        None
    }
}

#[derive(Debug, Clone)]
struct CryptoIntentDimensions {
    coin: String,
    horizon: CryptoHorizon,
    deployment_scope: String,
    position_key: String,
}

impl CryptoIntentDimensions {
    fn from_intent(intent: &OrderIntent) -> Self {
        let coin = Self::parse_coin(intent).unwrap_or_else(|| "OTHER".to_string());
        let horizon = Self::parse_horizon(intent).unwrap_or(CryptoHorizon::Other);
        let market_identity = intent_market_identity(intent);
        let deployment_scope = intent_deployment_scope(intent);
        let position_key = format!(
            "{}|{}|{}|{}",
            deployment_scope,
            market_identity,
            intent.token_id,
            intent.side.as_str()
        );
        Self {
            coin,
            horizon,
            deployment_scope,
            position_key,
        }
    }

    fn parse_coin(intent: &OrderIntent) -> Option<String> {
        if let Some(coin) = intent
            .metadata
            .get("coin")
            .and_then(|raw| Self::normalize_coin(raw))
        {
            return Some(coin);
        }

        if let Some(symbol) = intent.metadata.get("symbol") {
            let cleaned = symbol
                .trim()
                .to_ascii_uppercase()
                .replace("USDT", "")
                .replace("USD", "");
            if let Some(coin) = Self::normalize_coin(&cleaned) {
                return Some(coin);
            }
        }

        let slug = intent.market_slug.to_ascii_lowercase();
        for (needle, coin) in [
            ("bitcoin", "BTC"),
            ("btc", "BTC"),
            ("ethereum", "ETH"),
            ("eth", "ETH"),
            ("solana", "SOL"),
            ("sol", "SOL"),
            ("xrp", "XRP"),
        ] {
            if slug.contains(needle) {
                return Some(coin.to_string());
            }
        }

        None
    }

    fn parse_horizon(intent: &OrderIntent) -> Option<CryptoHorizon> {
        if let Some(h) = intent
            .metadata
            .get("horizon")
            .and_then(|raw| CryptoHorizon::from_hint(raw))
        {
            return Some(h);
        }

        if let Some(h) = intent
            .metadata
            .get("event_series_id")
            .and_then(|raw| CryptoHorizon::from_hint(raw))
        {
            return Some(h);
        }

        if let Some(h) = intent
            .metadata
            .get("series_id")
            .and_then(|raw| CryptoHorizon::from_hint(raw))
        {
            return Some(h);
        }

        CryptoHorizon::from_hint(&intent.market_slug)
    }

    fn normalize_coin(raw: &str) -> Option<String> {
        let coin = raw.trim().to_ascii_uppercase();
        if coin.is_empty() {
            return None;
        }
        Some(match coin.as_str() {
            "BITCOIN" | "BTC" => "BTC".to_string(),
            "ETHEREUM" | "ETH" => "ETH".to_string(),
            "SOLANA" | "SOL" => "SOL".to_string(),
            "XRP" => "XRP".to_string(),
            other => other.to_string(),
        })
    }
}

#[derive(Debug, Clone)]
struct PositionExposure {
    deployment_scope: String,
    coin: String,
    horizon: CryptoHorizon,
    amount: Decimal,
}

#[derive(Debug, Default)]
struct ExposureBook {
    total: Decimal,
    by_coin: HashMap<String, Decimal>,
    by_horizon: HashMap<CryptoHorizon, Decimal>,
    by_position: HashMap<String, PositionExposure>,
}

impl ExposureBook {
    fn value_for_coin(&self, coin: &str) -> Decimal {
        self.by_coin.get(coin).copied().unwrap_or(Decimal::ZERO)
    }

    fn value_for_horizon(&self, horizon: CryptoHorizon) -> Decimal {
        self.by_horizon
            .get(&horizon)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    fn add(&mut self, dims: &CryptoIntentDimensions, amount: Decimal) {
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

    fn subtract_from_position_key(&mut self, position_key: &str, amount: Decimal) -> Decimal {
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

        if let Some(c) = coin {
            if let Some(v) = self.by_coin.get_mut(&c) {
                *v = (*v - removed).max(Decimal::ZERO);
                if *v == Decimal::ZERO {
                    self.by_coin.remove(&c);
                }
            }
        }

        if let Some(h) = horizon {
            if let Some(v) = self.by_horizon.get_mut(&h) {
                *v = (*v - removed).max(Decimal::ZERO);
                if *v == Decimal::ZERO {
                    self.by_horizon.remove(&h);
                }
            }
        }

        removed
    }

    fn subtract_matching_bucket(
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
            .filter(|(_, p)| {
                p.deployment_scope == deployment_scope && p.coin == coin && p.horizon == horizon
            })
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
struct PendingCryptoIntent {
    dims: CryptoIntentDimensions,
    requested_notional: Decimal,
}

#[derive(Debug)]
pub(super) struct CryptoCapitalAllocator {
    enabled: bool,
    total_cap: Decimal,
    coin_cap_pct: HashMap<String, Decimal>,
    horizon_cap_pct: HashMap<CryptoHorizon, Decimal>,
    open: ExposureBook,
    pending: ExposureBook,
    pending_by_intent: HashMap<Uuid, PendingCryptoIntent>,
}

impl CryptoCapitalAllocator {
    pub(super) fn new(config: &CoordinatorConfig) -> Self {
        let configured_cap = config
            .crypto_allocator_total_cap_usd
            .or(config.risk.crypto_max_exposure)
            .unwrap_or(config.risk.max_platform_exposure);
        let total_cap = config
            .risk
            .crypto_max_exposure
            .map(|risk_cap| configured_cap.min(risk_cap))
            .unwrap_or(configured_cap)
            .max(Decimal::ZERO);

        let mut coin_cap_pct = HashMap::new();
        coin_cap_pct.insert(
            "BTC".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_btc_pct),
        );
        coin_cap_pct.insert(
            "ETH".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_eth_pct),
        );
        coin_cap_pct.insert(
            "SOL".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_sol_pct),
        );
        coin_cap_pct.insert(
            "XRP".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_xrp_pct),
        );
        coin_cap_pct.insert(
            "OTHER".to_string(),
            Self::normalize_pct(config.crypto_coin_cap_other_pct),
        );

        let mut horizon_cap_pct = HashMap::new();
        horizon_cap_pct.insert(
            CryptoHorizon::M5,
            Self::normalize_pct(config.crypto_horizon_cap_5m_pct),
        );
        horizon_cap_pct.insert(
            CryptoHorizon::M15,
            Self::normalize_pct(config.crypto_horizon_cap_15m_pct),
        );
        horizon_cap_pct.insert(
            CryptoHorizon::Other,
            Self::normalize_pct(config.crypto_horizon_cap_other_pct),
        );

        Self {
            enabled: config.crypto_allocator_enabled,
            total_cap,
            coin_cap_pct,
            horizon_cap_pct,
            open: ExposureBook::default(),
            pending: ExposureBook::default(),
            pending_by_intent: HashMap::new(),
        }
    }

    fn normalize_pct(value: Decimal) -> Decimal {
        if value <= Decimal::ZERO {
            Decimal::ZERO
        } else if value >= Decimal::ONE {
            Decimal::ONE
        } else {
            value
        }
    }

    pub(super) fn reset_runtime_state(&mut self) {
        self.open = ExposureBook::default();
        self.pending = ExposureBook::default();
        self.pending_by_intent.clear();
    }

    pub(super) fn reserve_buy(&mut self, intent: &OrderIntent) -> std::result::Result<(), String> {
        if !self.enabled || intent.domain != Domain::Crypto || !intent.is_buy {
            return Ok(());
        }

        if self.total_cap <= Decimal::ZERO {
            return Err("Crypto allocator cap is 0; buy intent blocked".to_string());
        }

        let requested = intent.notional_value();
        if requested <= Decimal::ZERO {
            return Err("Crypto buy intent has non-positive notional".to_string());
        }

        let dims = CryptoIntentDimensions::from_intent(intent);

        let projected_total = self.open.total + self.pending.total + requested;
        if projected_total > self.total_cap {
            return Err(format!(
                "Crypto total cap exceeded: projected={} cap={}",
                projected_total, self.total_cap
            ));
        }

        let coin_cap = self.total_cap * self.coin_cap_for(&dims.coin);
        let projected_coin = self.open.value_for_coin(&dims.coin)
            + self.pending.value_for_coin(&dims.coin)
            + requested;
        if projected_coin > coin_cap {
            return Err(format!(
                "Crypto coin cap exceeded: coin={} projected={} cap={}",
                dims.coin, projected_coin, coin_cap
            ));
        }

        let horizon_cap = self.total_cap * self.horizon_cap_for(dims.horizon);
        let projected_horizon = self.open.value_for_horizon(dims.horizon)
            + self.pending.value_for_horizon(dims.horizon)
            + requested;
        if projected_horizon > horizon_cap {
            return Err(format!(
                "Crypto horizon cap exceeded: horizon={} projected={} cap={}",
                dims.horizon.as_str(),
                projected_horizon,
                horizon_cap
            ));
        }

        self.pending.add(&dims, requested);
        self.pending_by_intent.insert(
            intent.intent_id,
            PendingCryptoIntent {
                dims,
                requested_notional: requested,
            },
        );

        Ok(())
    }

    pub(super) fn available_notional_for(&self, intent: &OrderIntent) -> Option<Decimal> {
        if !self.enabled || intent.domain != Domain::Crypto || !intent.is_buy {
            return None;
        }

        if self.total_cap <= Decimal::ZERO {
            return Some(Decimal::ZERO);
        }

        let dims = CryptoIntentDimensions::from_intent(intent);
        let remaining_total =
            (self.total_cap - self.open.total - self.pending.total).max(Decimal::ZERO);

        let coin_cap = self.total_cap * self.coin_cap_for(&dims.coin);
        let projected_coin =
            self.open.value_for_coin(&dims.coin) + self.pending.value_for_coin(&dims.coin);
        let remaining_coin = (coin_cap - projected_coin).max(Decimal::ZERO);

        let horizon_cap = self.total_cap * self.horizon_cap_for(dims.horizon);
        let projected_horizon = self.open.value_for_horizon(dims.horizon)
            + self.pending.value_for_horizon(dims.horizon);
        let remaining_horizon = (horizon_cap - projected_horizon).max(Decimal::ZERO);

        Some(remaining_total.min(remaining_coin).min(remaining_horizon))
    }

    pub(super) fn release_buy_reservation(&mut self, intent_id: Uuid) {
        let Some(reservation) = self.pending_by_intent.remove(&intent_id) else {
            return;
        };
        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );
    }

    pub(super) fn settle_buy_execution(
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

    pub(super) fn settle_sell_execution(
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

    fn coin_cap_for(&self, coin: &str) -> Decimal {
        self.coin_cap_pct
            .get(coin)
            .copied()
            .or_else(|| self.coin_cap_pct.get("OTHER").copied())
            .unwrap_or(Decimal::ZERO)
    }

    fn horizon_cap_for(&self, horizon: CryptoHorizon) -> Decimal {
        self.horizon_cap_pct
            .get(&horizon)
            .copied()
            .or_else(|| self.horizon_cap_pct.get(&CryptoHorizon::Other).copied())
            .unwrap_or(Decimal::ZERO)
    }

    pub(super) fn open_notional(&self) -> Decimal {
        self.open.total
    }

    pub(super) fn pending_notional(&self) -> Decimal {
        self.pending.total
    }

    pub(super) fn ledger_snapshot(&self) -> AllocatorLedgerSnapshot {
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

    pub(super) fn deployment_ledger_snapshot(&self) -> Vec<DeploymentLedgerSnapshot> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Side;
    use rust_decimal_macros::dec;

    fn make_allocator_config(total_cap: Decimal) -> CoordinatorConfig {
        let mut cfg = CoordinatorConfig::default();
        cfg.crypto_allocator_enabled = true;
        cfg.crypto_allocator_total_cap_usd = Some(total_cap);
        cfg.crypto_coin_cap_btc_pct = dec!(0.40);
        cfg.crypto_coin_cap_eth_pct = dec!(0.40);
        cfg.crypto_coin_cap_sol_pct = dec!(0.30);
        cfg.crypto_coin_cap_xrp_pct = dec!(0.20);
        cfg.crypto_coin_cap_other_pct = dec!(0.10);
        cfg.crypto_horizon_cap_5m_pct = dec!(0.50);
        cfg.crypto_horizon_cap_15m_pct = dec!(0.60);
        cfg.crypto_horizon_cap_other_pct = dec!(0.25);
        cfg
    }

    fn make_crypto_intent(
        coin: &str,
        horizon: &str,
        is_buy: bool,
        shares: u64,
        limit_price: Decimal,
    ) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "crypto",
            Domain::Crypto,
            "btc-up-or-down",
            "token-up-123",
            Side::Up,
            is_buy,
            shares,
            limit_price,
        );
        intent.metadata.insert("coin".to_string(), coin.to_string());
        intent
            .metadata
            .insert("horizon".to_string(), horizon.to_string());
        if !is_buy {
            intent
                .metadata
                .insert("entry_price".to_string(), limit_price.to_string());
        }
        intent
    }

    #[test]
    fn test_crypto_allocator_blocks_buy_when_coin_cap_exceeded() {
        let cfg = make_allocator_config(dec!(100));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);

        let first = make_crypto_intent("BTC", "5m", true, 60, dec!(0.5));
        let second = make_crypto_intent("BTC", "5m", true, 30, dec!(0.5));

        assert!(allocator.reserve_buy(&first).is_ok());
        assert!(allocator.reserve_buy(&second).is_err());
    }

    #[test]
    fn test_crypto_allocator_clamps_total_cap_to_risk_domain_cap() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.crypto_allocator_total_cap_usd = Some(dec!(100));
        cfg.risk.crypto_max_exposure = Some(dec!(60));

        let allocator = CryptoCapitalAllocator::new(&cfg);
        assert_eq!(allocator.total_cap, dec!(60));
    }

    #[test]
    fn test_crypto_allocator_releases_pending_on_buy_failure() {
        let cfg = make_allocator_config(dec!(100));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);
        let intent = make_crypto_intent("BTC", "5m", true, 50, dec!(0.5));

        assert!(allocator.reserve_buy(&intent).is_ok());
        assert!(allocator.pending.total > Decimal::ZERO);

        allocator.release_buy_reservation(intent.intent_id);

        assert_eq!(allocator.pending.total, Decimal::ZERO);
        assert!(allocator.pending_by_intent.is_empty());
    }

    #[test]
    fn test_crypto_allocator_settles_buy_then_sell() {
        let cfg = make_allocator_config(dec!(200));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);
        let buy = make_crypto_intent("BTC", "15m", true, 100, dec!(0.5));

        assert!(allocator.reserve_buy(&buy).is_ok());
        allocator.settle_buy_execution(&buy, 80, dec!(0.5));

        assert_eq!(allocator.pending.total, Decimal::ZERO);
        assert_eq!(allocator.open.total, dec!(40));

        let mut sell = make_crypto_intent("BTC", "15m", false, 40, dec!(0.5));
        sell.market_slug = buy.market_slug.clone();
        sell.token_id = buy.token_id.clone();
        sell.side = buy.side;
        allocator.settle_sell_execution(&sell, 40, dec!(0.55));

        assert_eq!(allocator.open.total, dec!(20));
    }

    #[test]
    fn test_crypto_allocator_sell_without_entry_price_does_not_release_other_positions() {
        let cfg = make_allocator_config(dec!(200));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);

        let mut buy_a = make_crypto_intent("BTC", "15m", true, 100, dec!(0.2));
        buy_a.market_slug = "btc-updown-a".to_string();
        buy_a.token_id = "token-up-a".to_string();
        buy_a = buy_a.with_deployment_id("deploy.crypto.btc.15m");

        let mut buy_b = make_crypto_intent("BTC", "15m", true, 100, dec!(0.2));
        buy_b.market_slug = "btc-updown-b".to_string();
        buy_b.token_id = "token-up-b".to_string();
        buy_b.side = Side::Down;
        buy_b = buy_b.with_deployment_id("deploy.crypto.btc.15m");

        assert!(allocator.reserve_buy(&buy_a).is_ok());
        allocator.settle_buy_execution(&buy_a, 100, dec!(0.2));
        assert!(allocator.reserve_buy(&buy_b).is_ok());
        allocator.settle_buy_execution(&buy_b, 100, dec!(0.2));
        assert_eq!(allocator.open.total, dec!(40));

        let mut sell_a = make_crypto_intent("BTC", "15m", false, 100, dec!(0.2));
        sell_a.market_slug = buy_a.market_slug.clone();
        sell_a.token_id = buy_a.token_id.clone();
        sell_a.side = buy_a.side;
        sell_a = sell_a.with_deployment_id("deploy.crypto.btc.15m");
        sell_a.metadata.remove("entry_price");

        allocator.settle_sell_execution(&sell_a, 100, dec!(0.8));
        assert_eq!(allocator.open.total, dec!(20));
        assert_eq!(allocator.open.by_position.len(), 1);
    }

    #[test]
    fn test_crypto_allocator_ledger_snapshot_reports_open_pending_and_available() {
        let cfg = make_allocator_config(dec!(200));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);

        let buy = make_crypto_intent("BTC", "15m", true, 100, dec!(0.5));
        assert!(allocator.reserve_buy(&buy).is_ok());
        allocator.settle_buy_execution(&buy, 80, dec!(0.5));

        let second = make_crypto_intent("ETH", "5m", true, 20, dec!(0.5));
        assert!(allocator.reserve_buy(&second).is_ok());

        let snap = allocator.ledger_snapshot();
        assert_eq!(snap.domain, "crypto");
        assert_eq!(snap.cap_notional_usd, dec!(200));
        assert_eq!(snap.open_notional_usd, dec!(40));
        assert_eq!(snap.pending_notional_usd, dec!(10));
        assert_eq!(snap.available_notional_usd, dec!(150));
    }

    #[test]
    fn test_crypto_allocator_deployment_ledger_snapshot_groups_open_and_pending() {
        let cfg = make_allocator_config(dec!(200));
        let mut allocator = CryptoCapitalAllocator::new(&cfg);

        let buy_a = make_crypto_intent("BTC", "15m", true, 100, dec!(0.5))
            .with_deployment_id("deploy.crypto.alpha");
        assert!(allocator.reserve_buy(&buy_a).is_ok());
        allocator.settle_buy_execution(&buy_a, 80, dec!(0.5));

        let pending_a = make_crypto_intent("BTC", "15m", true, 20, dec!(0.5))
            .with_deployment_id("deploy.crypto.alpha");
        assert!(allocator.reserve_buy(&pending_a).is_ok());

        let buy_b = make_crypto_intent("ETH", "5m", true, 50, dec!(0.4))
            .with_deployment_id("deploy.crypto.beta");
        assert!(allocator.reserve_buy(&buy_b).is_ok());
        allocator.settle_buy_execution(&buy_b, 25, dec!(0.4));

        let deployments = allocator.deployment_ledger_snapshot();
        assert_eq!(deployments.len(), 2);
        assert_eq!(deployments[0].deployment_id, "deploy.crypto.alpha");
        assert_eq!(deployments[0].domain, "crypto");
        assert_eq!(deployments[0].open_notional_usd, dec!(40));
        assert_eq!(deployments[0].pending_notional_usd, dec!(10));
        assert_eq!(deployments[0].total_notional_usd, dec!(50));

        assert_eq!(deployments[1].deployment_id, "deploy.crypto.beta");
        assert_eq!(deployments[1].domain, "crypto");
        assert_eq!(deployments[1].open_notional_usd, dec!(10));
        assert_eq!(deployments[1].pending_notional_usd, Decimal::ZERO);
        assert_eq!(deployments[1].total_notional_usd, dec!(10));
    }
}
