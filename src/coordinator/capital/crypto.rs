use rust_decimal::Decimal;
use std::collections::HashMap;
use uuid::Uuid;

use crate::coordinator::config::CoordinatorConfig;
use crate::platform::{Domain, OrderIntent};

use super::{
    intent_deployment_scope, intent_market_identity, KNOWN_15M_SERIES_IDS, KNOWN_5M_SERIES_IDS,
};

mod ledger;

use ledger::{ExposureBook, PendingCryptoIntent};

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
