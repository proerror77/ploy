use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use uuid::Uuid;

use crate::coordinator::OrderIntent;
use crate::coordinator::config::CoordinatorConfig;
use crate::domain::Domain;

#[path = "market/accounting.rs"]
mod accounting;

use accounting::{MarketExposureBook, PendingMarketIntent};

pub(in crate::coordinator) fn normalized_identity_component(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_ascii_lowercase())
}

fn intent_condition_id(intent: &OrderIntent) -> Option<String> {
    intent
        .condition_id()
        .and_then(normalized_identity_component)
}

pub(in crate::coordinator) fn intent_market_identity(intent: &OrderIntent) -> String {
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

pub(in crate::coordinator) fn intent_deployment_scope(intent: &OrderIntent) -> String {
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
pub(in crate::coordinator) fn sell_release_reference_price(
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

#[derive(Debug)]
pub(super) struct MarketCapitalAllocator {
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
    pub(super) fn for_sports(config: &CoordinatorConfig) -> Self {
        Self::new_for_domain(config, Domain::Sports)
    }

    pub(super) fn for_politics(config: &CoordinatorConfig) -> Self {
        Self::new_for_domain(config, Domain::Politics)
    }

    pub(super) fn for_economics(config: &CoordinatorConfig) -> Self {
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

    pub(super) fn reset_runtime_state(&mut self) {
        self.open = MarketExposureBook::default();
        self.pending = MarketExposureBook::default();
        self.pending_by_intent.clear();
    }

    pub(super) fn open_notional(&self) -> Decimal {
        self.open.total
    }

    pub(super) fn pending_notional(&self) -> Decimal {
        self.pending.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_sports_allocator_auto_split_deduplicates_current_pending_market() {
        let mut cfg = make_allocator_config(dec!(100));
        cfg.sports_allocator_enabled = true;
        cfg.sports_allocator_total_cap_usd = Some(dec!(36));
        cfg.sports_market_cap_pct = dec!(1.0);
        cfg.sports_auto_split_by_active_markets = true;

        let mut allocator = MarketCapitalAllocator::for_sports(&cfg);

        let game1_open = make_sports_intent("nba-game-1", true, 100, dec!(0.10));
        assert!(allocator.reserve_buy(&game1_open).is_ok());
        allocator.settle_buy_execution(&game1_open, 100, dec!(0.10));

        let game2_pending = make_sports_intent("nba-game-2", true, 100, dec!(0.10));
        assert!(allocator.reserve_buy(&game2_pending).is_ok());

        let game2_extra = make_sports_intent("nba-game-2", true, 50, dec!(0.10));
        assert!(allocator.reserve_buy(&game2_extra).is_ok());
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
