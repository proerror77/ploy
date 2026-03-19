use rust_decimal::Decimal;

use crate::coordinator::config::CoordinatorConfig;
use crate::domain::Domain;

#[derive(Debug, Clone, Copy)]
pub(super) struct MarketAllocatorDomainConfig {
    pub(super) domain_label: &'static str,
    pub(super) enabled: bool,
    pub(super) total_cap: Decimal,
    pub(super) market_cap_pct: Decimal,
    pub(super) auto_split_by_active_markets: bool,
}

impl MarketAllocatorDomainConfig {
    pub(super) fn for_domain(config: &CoordinatorConfig, domain: Domain) -> Self {
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
            domain_label,
            enabled,
            total_cap,
            market_cap_pct: normalize_pct(market_cap_pct),
            auto_split_by_active_markets,
        }
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
