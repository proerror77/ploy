use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::platform::{Domain, OrderIntent};

use super::command::{AllocatorLedgerSnapshot, DeploymentLedgerSnapshot};
use super::config::CoordinatorConfig;

pub(super) fn normalized_identity_component(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_ascii_lowercase())
}

fn intent_condition_id(intent: &OrderIntent) -> Option<String> {
    intent
        .condition_id()
        .and_then(normalized_identity_component)
}

pub(super) fn intent_market_identity(intent: &OrderIntent) -> String {
    if let Some(condition_id) = intent_condition_id(intent) {
        return format!("condition:{}", condition_id);
    }
    if let Some(slug) = normalized_identity_component(&intent.market_slug) {
        return format!("slug:{}", slug);
    }
    if let Some(token) = normalized_identity_component(&intent.token_id) {
        return format!("token:{}", token);
    }
    "unknown".to_string()
}

pub(super) fn intent_deployment_scope(intent: &OrderIntent) -> String {
    if let Some(scope) = intent
        .deployment_id()
        .and_then(normalized_identity_component)
    {
        return scope;
    }

    let strategy = intent
        .metadata
        .get("strategy")
        .and_then(|v| normalized_identity_component(v))
        .unwrap_or_else(|| "default".to_string());
    format!(
        "agent:{}|strategy:{}",
        intent.agent_id.trim().to_ascii_lowercase(),
        strategy
    )
}

/// Resolve the notional reference price for sell-side exposure release.
///
/// Returns `(price, has_explicit_entry_price)` where `has_explicit_entry_price`
/// indicates whether the value came from metadata.
fn sell_release_reference_price(
    intent: &OrderIntent,
    execution_price: Decimal,
) -> Option<(Decimal, bool)> {
    if let Some(entry_price) = intent
        .metadata
        .get("entry_price")
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| Decimal::from_str(v).ok())
        .filter(|v| *v > Decimal::ZERO)
    {
        return Some((entry_price, true));
    }

    if execution_price > Decimal::ZERO {
        return Some((execution_price, false));
    }

    (intent.limit_price > Decimal::ZERO).then_some((intent.limit_price, false))
}

const KNOWN_5M_SERIES_IDS: &[&str] = &["10684", "10683", "10686", "10685"];
const KNOWN_15M_SERIES_IDS: &[&str] = &["10192", "10191", "10423", "10422"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum CryptoHorizon {
    M5,
    M15,
    Other,
}

impl CryptoHorizon {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::Other => "other",
        }
    }

    pub(super) fn from_hint(raw: &str) -> Option<Self> {
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
struct CryptoCapitalAllocator {
    enabled: bool,
    total_cap: Decimal,
    coin_cap_pct: HashMap<String, Decimal>,
    horizon_cap_pct: HashMap<CryptoHorizon, Decimal>,
    open: ExposureBook,
    pending: ExposureBook,
    pending_by_intent: HashMap<Uuid, PendingCryptoIntent>,
}

impl CryptoCapitalAllocator {
    fn new(config: &CoordinatorConfig) -> Self {
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

    fn reset_runtime_state(&mut self) {
        self.open = ExposureBook::default();
        self.pending = ExposureBook::default();
        self.pending_by_intent.clear();
    }

    fn reserve_buy(&mut self, intent: &OrderIntent) -> std::result::Result<(), String> {
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

    fn available_notional_for(&self, intent: &OrderIntent) -> Option<Decimal> {
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

    fn release_buy_reservation(&mut self, intent_id: Uuid) {
        let Some(reservation) = self.pending_by_intent.remove(&intent_id) else {
            return;
        };
        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );
    }

    fn settle_buy_execution(
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

    fn settle_sell_execution(
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

    fn open_notional(&self) -> Decimal {
        self.open.total
    }

    fn pending_notional(&self) -> Decimal {
        self.pending.total
    }

    fn ledger_snapshot(&self) -> AllocatorLedgerSnapshot {
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

    fn deployment_ledger_snapshot(&self) -> Vec<DeploymentLedgerSnapshot> {
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

#[derive(Debug, Clone)]
struct MarketIntentDimensions {
    market_key: String,
    deployment_scope: String,
    position_key: String,
}

impl MarketIntentDimensions {
    fn from_intent(intent: &OrderIntent) -> Self {
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
struct MarketPositionExposure {
    market_key: String,
    deployment_scope: String,
    amount: Decimal,
}

#[derive(Debug, Default)]
struct MarketExposureBook {
    total: Decimal,
    by_market: HashMap<String, Decimal>,
    by_position: HashMap<String, MarketPositionExposure>,
}

impl MarketExposureBook {
    fn value_for_market(&self, market_key: &str) -> Decimal {
        self.by_market
            .get(market_key)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    fn add(&mut self, dims: &MarketIntentDimensions, amount: Decimal) {
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

    fn subtract_from_position_key(&mut self, position_key: &str, amount: Decimal) -> Decimal {
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

    fn subtract_matching_market(
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
struct PendingMarketIntent {
    dims: MarketIntentDimensions,
    requested_notional: Decimal,
}

#[derive(Debug)]
struct MarketCapitalAllocator {
    domain: Domain,
    domain_label: &'static str,
    enabled: bool,
    total_cap: Decimal,
    market_cap_pct: Decimal,
    auto_split_by_active_markets: bool,
    open: MarketExposureBook,
    pending: MarketExposureBook,
    pending_by_intent: HashMap<Uuid, PendingMarketIntent>,
}

impl MarketCapitalAllocator {
    fn for_sports(config: &CoordinatorConfig) -> Self {
        Self::new_for_domain(config, Domain::Sports)
    }

    fn for_politics(config: &CoordinatorConfig) -> Self {
        Self::new_for_domain(config, Domain::Politics)
    }

    fn for_economics(config: &CoordinatorConfig) -> Self {
        Self::new_for_domain(config, Domain::Economics)
    }

    fn new_for_domain(config: &CoordinatorConfig, domain: Domain) -> Self {
        let (
            domain_label,
            enabled,
            configured_cap,
            risk_cap,
            market_cap_pct,
            auto_split_by_active_markets,
        ) = match domain {
            Domain::Sports => (
                "sports",
                config.sports_allocator_enabled,
                config.sports_allocator_total_cap_usd,
                config.risk.sports_max_exposure,
                config.sports_market_cap_pct,
                config.sports_auto_split_by_active_markets,
            ),
            Domain::Politics => (
                "politics",
                config.politics_allocator_enabled,
                config.politics_allocator_total_cap_usd,
                config.risk.politics_max_exposure,
                config.politics_market_cap_pct,
                config.politics_auto_split_by_active_markets,
            ),
            Domain::Economics => (
                "economics",
                config.economics_allocator_enabled,
                config.economics_allocator_total_cap_usd,
                config.risk.economics_max_exposure,
                config.economics_market_cap_pct,
                config.economics_auto_split_by_active_markets,
            ),
            Domain::Crypto | Domain::Custom(_) => {
                panic!("market allocator does not support domain {:?}", domain)
            }
        };

        let configured_cap = configured_cap
            .or(risk_cap)
            .unwrap_or(config.risk.max_platform_exposure);
        let total_cap = risk_cap
            .map(|cap| configured_cap.min(cap))
            .unwrap_or(configured_cap)
            .max(Decimal::ZERO);

        Self {
            domain,
            domain_label,
            enabled,
            total_cap,
            market_cap_pct: Self::normalize_pct(market_cap_pct),
            auto_split_by_active_markets,
            open: MarketExposureBook::default(),
            pending: MarketExposureBook::default(),
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

    fn reset_runtime_state(&mut self) {
        self.open = MarketExposureBook::default();
        self.pending = MarketExposureBook::default();
        self.pending_by_intent.clear();
    }

    fn reserve_buy(&mut self, intent: &OrderIntent) -> std::result::Result<(), String> {
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

    fn release_buy_reservation(&mut self, intent_id: Uuid) {
        let Some(reservation) = self.pending_by_intent.remove(&intent_id) else {
            return;
        };
        self.pending.subtract_from_position_key(
            &reservation.dims.position_key,
            reservation.requested_notional,
        );
    }

    fn settle_buy_execution(
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

    fn settle_sell_execution(
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

        let mut active_markets: HashSet<String> = self
            .open
            .by_market
            .iter()
            .filter(|(_, v)| **v > Decimal::ZERO)
            .map(|(k, _)| k.clone())
            .collect();

        for (k, v) in &self.pending.by_market {
            if *v > Decimal::ZERO {
                active_markets.insert(k.clone());
            }
        }

        if !market_key.is_empty() {
            active_markets.insert(market_key.to_string());
        }

        let market_count = active_markets.len().max(1) as u64;
        let dynamic_cap = self.total_cap / Decimal::from(market_count);
        dynamic_cap.min(fixed_cap)
    }

    fn open_notional(&self) -> Decimal {
        self.open.total
    }

    fn pending_notional(&self) -> Decimal {
        self.pending.total
    }

    fn ledger_snapshot(&self) -> AllocatorLedgerSnapshot {
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

    fn deployment_ledger_snapshot(&self) -> Vec<DeploymentLedgerSnapshot> {
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

    fn available_notional_for(&self, intent: &OrderIntent) -> Option<Decimal> {
        if !self.enabled || intent.domain != Domain::Sports || !intent.is_buy {
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

#[derive(Debug)]
pub(super) struct CapitalPolicy {
    crypto: RwLock<CryptoCapitalAllocator>,
    sports: RwLock<MarketCapitalAllocator>,
    politics: RwLock<MarketCapitalAllocator>,
    economics: RwLock<MarketCapitalAllocator>,
}

impl CapitalPolicy {
    pub(super) fn new(config: &CoordinatorConfig) -> Self {
        Self {
            crypto: RwLock::new(CryptoCapitalAllocator::new(config)),
            sports: RwLock::new(MarketCapitalAllocator::for_sports(config)),
            politics: RwLock::new(MarketCapitalAllocator::for_politics(config)),
            economics: RwLock::new(MarketCapitalAllocator::for_economics(config)),
        }
    }

    pub(super) async fn reset_runtime_state(&self) {
        self.crypto.write().await.reset_runtime_state();
        self.sports.write().await.reset_runtime_state();
        self.politics.write().await.reset_runtime_state();
        self.economics.write().await.reset_runtime_state();
    }

    pub(super) async fn allocator_totals(&self) -> (Decimal, Decimal) {
        let (crypto_open, crypto_pending) = {
            let allocator = self.crypto.read().await;
            (allocator.open_notional(), allocator.pending_notional())
        };
        let (sports_open, sports_pending) = {
            let allocator = self.sports.read().await;
            (allocator.open_notional(), allocator.pending_notional())
        };
        let (politics_open, politics_pending) = {
            let allocator = self.politics.read().await;
            (allocator.open_notional(), allocator.pending_notional())
        };
        let (economics_open, economics_pending) = {
            let allocator = self.economics.read().await;
            (allocator.open_notional(), allocator.pending_notional())
        };

        (
            crypto_open + sports_open + politics_open + economics_open,
            crypto_pending + sports_pending + politics_pending + economics_pending,
        )
    }

    pub(super) async fn ledger_rows(
        &self,
    ) -> (
        Vec<AllocatorLedgerSnapshot>,
        Vec<DeploymentLedgerSnapshot>,
        Decimal,
        Decimal,
    ) {
        let (crypto, mut deployments) = {
            let allocator = self.crypto.read().await;
            (
                allocator.ledger_snapshot(),
                allocator.deployment_ledger_snapshot(),
            )
        };
        let (sports, sports_deployments) = {
            let allocator = self.sports.read().await;
            (
                allocator.ledger_snapshot(),
                allocator.deployment_ledger_snapshot(),
            )
        };
        deployments.extend(sports_deployments);
        let (politics, politics_deployments) = {
            let allocator = self.politics.read().await;
            (
                allocator.ledger_snapshot(),
                allocator.deployment_ledger_snapshot(),
            )
        };
        deployments.extend(politics_deployments);
        let (economics, economics_deployments) = {
            let allocator = self.economics.read().await;
            (
                allocator.ledger_snapshot(),
                allocator.deployment_ledger_snapshot(),
            )
        };
        deployments.extend(economics_deployments);
        deployments.sort_by(|a, b| {
            a.domain
                .cmp(&b.domain)
                .then_with(|| a.deployment_id.cmp(&b.deployment_id))
        });

        let allocator_open_notional = crypto.open_notional_usd
            + sports.open_notional_usd
            + politics.open_notional_usd
            + economics.open_notional_usd;
        let allocator_pending_notional = crypto.pending_notional_usd
            + sports.pending_notional_usd
            + politics.pending_notional_usd
            + economics.pending_notional_usd;

        (
            vec![crypto, sports, politics, economics],
            deployments,
            allocator_open_notional,
            allocator_pending_notional,
        )
    }

    pub(super) async fn available_notional_for(&self, intent: &OrderIntent) -> Option<Decimal> {
        match intent.domain {
            Domain::Crypto => self.crypto.read().await.available_notional_for(intent),
            Domain::Sports => self.sports.read().await.available_notional_for(intent),
            _ => None,
        }
    }

    pub(super) async fn reserve_buy(&self, intent: &OrderIntent) -> Option<String> {
        if !intent.is_buy {
            return None;
        }
        match intent.domain {
            Domain::Crypto => self.crypto.write().await.reserve_buy(intent).err(),
            Domain::Sports => self.sports.write().await.reserve_buy(intent).err(),
            Domain::Politics => self.politics.write().await.reserve_buy(intent).err(),
            Domain::Economics => self.economics.write().await.reserve_buy(intent).err(),
            _ => None,
        }
    }

    pub(super) async fn release_buy_reservation(&self, intent_id: Uuid) {
        self.crypto.write().await.release_buy_reservation(intent_id);
        self.sports.write().await.release_buy_reservation(intent_id);
        self.politics
            .write()
            .await
            .release_buy_reservation(intent_id);
        self.economics
            .write()
            .await
            .release_buy_reservation(intent_id);
    }

    pub(super) async fn settle_success(
        &self,
        intent: &OrderIntent,
        filled_shares: u64,
        fill_price: Decimal,
    ) {
        match intent.domain {
            Domain::Crypto => {
                let mut allocator = self.crypto.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            Domain::Sports => {
                let mut allocator = self.sports.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            Domain::Politics => {
                let mut allocator = self.politics.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            Domain::Economics => {
                let mut allocator = self.economics.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            _ => {}
        }
    }

    pub(super) async fn settle_failure(&self, intent: &OrderIntent) {
        if !intent.is_buy {
            return;
        }
        match intent.domain {
            Domain::Crypto => self
                .crypto
                .write()
                .await
                .release_buy_reservation(intent.intent_id),
            Domain::Sports => self
                .sports
                .write()
                .await
                .release_buy_reservation(intent.intent_id),
            Domain::Politics => self
                .politics
                .write()
                .await
                .release_buy_reservation(intent.intent_id),
            Domain::Economics => self
                .economics
                .write()
                .await
                .release_buy_reservation(intent.intent_id),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::OrderIntent;
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
            crate::domain::Side::Up,
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

    fn make_sports_intent(
        market_slug: &str,
        is_buy: bool,
        shares: u64,
        limit_price: Decimal,
    ) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "sports",
            Domain::Sports,
            market_slug,
            "sports-token-yes",
            crate::domain::Side::Up,
            is_buy,
            shares,
            limit_price,
        );
        if !is_buy {
            intent
                .metadata
                .insert("entry_price".to_string(), limit_price.to_string());
        }
        intent
    }

    fn make_domain_market_intent(
        domain: Domain,
        market_slug: &str,
        is_buy: bool,
        shares: u64,
        limit_price: Decimal,
    ) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "domain-agent",
            domain,
            market_slug,
            "domain-token-yes",
            crate::domain::Side::Up,
            is_buy,
            shares,
            limit_price,
        );
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
        buy_b.side = crate::domain::Side::Down;
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
    fn test_sports_allocator_auto_splits_by_active_markets() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(30));
        cfg.sports_market_cap_pct = dec!(0.70);
        cfg.sports_auto_split_by_active_markets = true;

        let mut allocator = MarketCapitalAllocator::for_sports(&cfg);

        let game1_buy = make_sports_intent("nba-game-1", true, 100, dec!(0.15));
        let game2_buy = make_sports_intent("nba-game-2", true, 100, dec!(0.15));
        let game1_extra = make_sports_intent("nba-game-1", true, 10, dec!(0.10));

        assert!(allocator.reserve_buy(&game1_buy).is_ok());
        assert!(allocator.reserve_buy(&game2_buy).is_ok());
        assert!(allocator.reserve_buy(&game1_extra).is_err());
    }

    #[test]
    fn test_sports_allocator_releases_pending_on_buy_failure() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(30));

        let mut allocator = MarketCapitalAllocator::for_sports(&cfg);
        let intent = make_sports_intent("nba-game-1", true, 100, dec!(0.10));

        assert!(allocator.reserve_buy(&intent).is_ok());
        assert!(allocator.pending.total > Decimal::ZERO);

        allocator.release_buy_reservation(intent.intent_id);

        assert_eq!(allocator.pending.total, Decimal::ZERO);
        assert!(allocator.pending_by_intent.is_empty());
    }

    #[test]
    fn test_sports_allocator_clamps_total_cap_to_risk_domain_cap() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(50));
        cfg.risk.sports_max_exposure = Some(dec!(25));

        let allocator = MarketCapitalAllocator::for_sports(&cfg);
        assert_eq!(allocator.total_cap, dec!(25));
    }

    #[test]
    fn test_market_allocator_sell_without_entry_price_does_not_release_other_positions() {
        let mut cfg = make_allocator_config(dec!(200));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(200));
        cfg.sports_market_cap_pct = dec!(1.0);

        let mut allocator = MarketCapitalAllocator::for_sports(&cfg);

        let mut buy_yes = make_sports_intent("nba-game-1", true, 100, dec!(0.2));
        buy_yes = buy_yes.with_deployment_id("deploy.sports.nba.comeback");

        let mut buy_no = make_sports_intent("nba-game-1", true, 100, dec!(0.2));
        buy_no.token_id = "sports-token-no".to_string();
        buy_no.side = crate::domain::Side::Down;
        buy_no = buy_no.with_deployment_id("deploy.sports.nba.comeback");

        assert!(allocator.reserve_buy(&buy_yes).is_ok());
        allocator.settle_buy_execution(&buy_yes, 100, dec!(0.2));
        assert!(allocator.reserve_buy(&buy_no).is_ok());
        allocator.settle_buy_execution(&buy_no, 100, dec!(0.2));
        assert_eq!(allocator.open.total, dec!(40));

        let mut sell_yes = make_sports_intent("nba-game-1", false, 100, dec!(0.2));
        sell_yes.token_id = buy_yes.token_id.clone();
        sell_yes.side = buy_yes.side;
        sell_yes = sell_yes.with_deployment_id("deploy.sports.nba.comeback");
        sell_yes.metadata.remove("entry_price");

        allocator.settle_sell_execution(&sell_yes, 100, dec!(0.8));
        assert_eq!(allocator.open.total, dec!(20));
        assert_eq!(allocator.open.by_position.len(), 1);
    }

    #[test]
    fn test_politics_allocator_clamps_total_cap_to_risk_domain_cap() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.politics_allocator_enabled = true;
        cfg.politics_allocator_total_cap_usd = Some(dec!(40));
        cfg.risk.politics_max_exposure = Some(dec!(18));

        let allocator = MarketCapitalAllocator::for_politics(&cfg);
        assert_eq!(allocator.total_cap, dec!(18));
    }

    #[test]
    fn test_economics_allocator_reserves_with_condition_identity() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.economics_allocator_enabled = true;
        cfg.economics_allocator_total_cap_usd = Some(dec!(30));
        cfg.economics_market_cap_pct = dec!(0.60);
        cfg.economics_auto_split_by_active_markets = true;

        let mut allocator = MarketCapitalAllocator::for_economics(&cfg);
        let mut first =
            make_domain_market_intent(Domain::Economics, "fed-rate-cut-v1", true, 100, dec!(0.10));
        first.metadata.insert(
            "condition_id".to_string(),
            "0x2222000000000000000000000000000000000000000000000000000000000000".to_string(),
        );
        let mut second =
            make_domain_market_intent(Domain::Economics, "fed-rate-cut-v2", true, 100, dec!(0.10));
        second.metadata.insert(
            "condition_id".to_string(),
            "0x2222000000000000000000000000000000000000000000000000000000000000".to_string(),
        );

        assert!(allocator.reserve_buy(&first).is_ok());
        assert!(allocator.reserve_buy(&second).is_err());
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
    fn test_sports_allocator_ledger_snapshot_reports_open_pending_and_available() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(50));
        let mut allocator = MarketCapitalAllocator::for_sports(&cfg);

        let buy = make_sports_intent("nba-game-1", true, 100, dec!(0.10));
        assert!(allocator.reserve_buy(&buy).is_ok());
        allocator.settle_buy_execution(&buy, 50, dec!(0.10));

        let pending = make_sports_intent("nba-game-2", true, 40, dec!(0.10));
        assert!(allocator.reserve_buy(&pending).is_ok());

        let snap = allocator.ledger_snapshot();
        assert_eq!(snap.domain, "sports");
        assert_eq!(snap.cap_notional_usd, dec!(50));
        assert_eq!(snap.open_notional_usd, dec!(5));
        assert_eq!(snap.pending_notional_usd, dec!(4));
        assert_eq!(snap.available_notional_usd, dec!(41));
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

    #[test]
    fn test_market_allocator_deployment_ledger_snapshot_groups_open_and_pending() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(60));
        cfg.sports_market_cap_pct = dec!(1.0);
        let mut allocator = MarketCapitalAllocator::for_sports(&cfg);

        let buy_a = make_sports_intent("nba-game-1", true, 100, dec!(0.2))
            .with_deployment_id("deploy.sports.alpha");
        assert!(allocator.reserve_buy(&buy_a).is_ok());
        allocator.settle_buy_execution(&buy_a, 50, dec!(0.2));

        let pending_a = make_sports_intent("nba-game-2", true, 20, dec!(0.2))
            .with_deployment_id("deploy.sports.alpha");
        assert!(allocator.reserve_buy(&pending_a).is_ok());

        let buy_b = make_sports_intent("nba-game-3", true, 40, dec!(0.25))
            .with_deployment_id("deploy.sports.beta");
        assert!(allocator.reserve_buy(&buy_b).is_ok());
        allocator.settle_buy_execution(&buy_b, 20, dec!(0.25));

        let deployments = allocator.deployment_ledger_snapshot();
        assert_eq!(deployments.len(), 2);
        assert_eq!(deployments[0].deployment_id, "deploy.sports.alpha");
        assert_eq!(deployments[0].domain, "sports");
        assert_eq!(deployments[0].open_notional_usd, dec!(10));
        assert_eq!(deployments[0].pending_notional_usd, dec!(4));
        assert_eq!(deployments[0].total_notional_usd, dec!(14));

        assert_eq!(deployments[1].deployment_id, "deploy.sports.beta");
        assert_eq!(deployments[1].domain, "sports");
        assert_eq!(deployments[1].open_notional_usd, dec!(5));
        assert_eq!(deployments[1].pending_notional_usd, Decimal::ZERO);
        assert_eq!(deployments[1].total_notional_usd, dec!(5));
    }
}
