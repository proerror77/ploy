use rust_decimal::Decimal;
use std::collections::HashMap;
use uuid::Uuid;

use crate::coordinator::config::CoordinatorConfig;

mod dimensions;
mod ledger;
mod policy;

pub(in crate::coordinator) use dimensions::CryptoHorizon;
use dimensions::CryptoIntentDimensions;
use ledger::{ExposureBook, PendingCryptoIntent};
use policy::normalize_pct;

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
            normalize_pct(config.crypto_coin_cap_btc_pct),
        );
        coin_cap_pct.insert(
            "ETH".to_string(),
            normalize_pct(config.crypto_coin_cap_eth_pct),
        );
        coin_cap_pct.insert(
            "SOL".to_string(),
            normalize_pct(config.crypto_coin_cap_sol_pct),
        );
        coin_cap_pct.insert(
            "XRP".to_string(),
            normalize_pct(config.crypto_coin_cap_xrp_pct),
        );
        coin_cap_pct.insert(
            "OTHER".to_string(),
            normalize_pct(config.crypto_coin_cap_other_pct),
        );

        let mut horizon_cap_pct = HashMap::new();
        horizon_cap_pct.insert(
            CryptoHorizon::M5,
            normalize_pct(config.crypto_horizon_cap_5m_pct),
        );
        horizon_cap_pct.insert(
            CryptoHorizon::M15,
            normalize_pct(config.crypto_horizon_cap_15m_pct),
        );
        horizon_cap_pct.insert(
            CryptoHorizon::Other,
            normalize_pct(config.crypto_horizon_cap_other_pct),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Side;
    use crate::coordinator::OrderIntent;
    use crate::platform::Domain;
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
