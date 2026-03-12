use std::collections::{HashMap, HashSet};

use tracing::{info, warn};

use crate::coordinator::bootstrap::PlatformBootstrapConfig;
use crate::platform::{Domain, MarketSelector, StrategyDeployment};

fn normalize_strategy_key(strategy: &str) -> String {
    strategy.to_ascii_lowercase().replace(['-', '_', ' '], "")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CryptoStrategyKind {
    Momentum,
    PatternMemory,
    SplitArb,
    Pm5mDirectional,
    LobMl,
    #[cfg(feature = "rl")]
    RlPolicy,
    Unknown,
}

fn classify_crypto_strategy(strategy: &str) -> CryptoStrategyKind {
    let key = normalize_strategy_key(strategy);

    if key == "pm5mdirectional" || key == "pm5mdirection" || key == "pm5mdir" || key == "pm5m" {
        return CryptoStrategyKind::Pm5mDirectional;
    }
    if key.contains("momentum")
        || key == "mom"
        || key == "directional"
        || key == "directionalmomentum"
    {
        return CryptoStrategyKind::Momentum;
    }
    if key.contains("pattern") || key.contains("memory") || key.contains("pattenmem") {
        return CryptoStrategyKind::PatternMemory;
    }
    if key.contains("splitarb")
        || (key.contains("split") && key.contains("arb"))
        || key.contains("staggeredarb")
        || key.contains("gammascalping")
    {
        return CryptoStrategyKind::SplitArb;
    }
    if key.contains("lob")
        || key.contains("ml")
        || key.contains("dl")
        || key.contains("deep")
        || key.contains("learning")
    {
        return CryptoStrategyKind::LobMl;
    }
    #[cfg(feature = "rl")]
    if key.contains("rl") || key.contains("policy") {
        return CryptoStrategyKind::RlPolicy;
    }

    CryptoStrategyKind::Unknown
}

fn normalize_horizon(value: &str) -> Option<&'static str> {
    let key = value.to_ascii_lowercase().replace(['-', '_', ' '], "");
    if key == "5m" || key == "5min" || key == "5minute" {
        return Some("5m");
    }
    if key == "15m" || key == "15min" || key == "15minute" {
        return Some("15m");
    }
    None
}

pub(crate) fn crypto_series_id_for(coin: &str, horizon: &str) -> Option<&'static str> {
    let c = coin.to_ascii_uppercase();
    match (c.as_str(), horizon) {
        ("BTC", "5m") => Some("10684"),
        ("ETH", "5m") => Some("10683"),
        ("SOL", "5m") => Some("10686"),
        ("XRP", "5m") => Some("10685"),
        ("BTC", "15m") => Some("10192"),
        ("ETH", "15m") => Some("10191"),
        ("SOL", "15m") => Some("10423"),
        ("XRP", "15m") => Some("10422"),
        _ => None,
    }
}

pub(crate) fn coin_symbol_for(coin: &str) -> Option<String> {
    let c = coin.to_ascii_uppercase();
    if c.is_empty() {
        return None;
    }
    Some(format!("{c}USDT"))
}

pub(crate) fn symbol_for_crypto_series_id(series_id: &str) -> Option<&'static str> {
    match series_id {
        "10684" | "10192" => Some("BTCUSDT"),
        "10683" | "10191" => Some("ETHUSDT"),
        "10686" | "10423" => Some("SOLUSDT"),
        "10685" | "10422" => Some("XRPUSDT"),
        _ => None,
    }
}

fn add_coin_from_text(raw: &str, coins: &mut HashSet<String>) {
    let upper = raw.to_ascii_uppercase();
    for coin in ["BTC", "ETH", "SOL", "XRP"] {
        if upper.contains(coin) {
            coins.insert(coin.to_string());
        }
    }
}

fn add_coins_from_selector(selector: &MarketSelector, coins: &mut HashSet<String>) {
    match selector {
        MarketSelector::Static {
            symbol,
            series_id,
            market_slug,
        } => {
            if let Some(raw) = symbol.as_deref() {
                add_coin_from_text(raw, coins);
            }
            if let Some(raw) = series_id.as_deref() {
                add_coin_from_text(raw, coins);
            }
            if let Some(raw) = market_slug.as_deref() {
                add_coin_from_text(raw, coins);
            }
        }
        MarketSelector::Dynamic { query, .. } => {
            if let Some(raw) = query.as_deref() {
                add_coin_from_text(raw, coins);
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct RuntimeCryptoStrategyTargets {
    pub(crate) pm_5m_directional_coins: HashSet<String>,
    pub(crate) pattern_memory_coins: HashSet<String>,
    pub(crate) split_arb_coins: HashSet<String>,
    pub(crate) split_arb_horizons: HashSet<String>,
}

pub(crate) fn collect_runtime_crypto_strategy_targets(
    deployments: &[StrategyDeployment],
    runtime_account_id: &str,
    runtime_dry_run: bool,
) -> RuntimeCryptoStrategyTargets {
    let mut out = RuntimeCryptoStrategyTargets::default();

    for dep in deployments
        .iter()
        .filter(|d| d.enabled)
        .filter(|d| d.matches_account(runtime_account_id))
        .filter(|d| d.matches_execution_mode(runtime_dry_run))
    {
        if !matches!(dep.domain, Domain::Crypto) {
            continue;
        }

        match classify_crypto_strategy(&dep.strategy) {
            CryptoStrategyKind::Pm5mDirectional => {
                add_coins_from_selector(&dep.market_selector, &mut out.pm_5m_directional_coins);
            }
            CryptoStrategyKind::PatternMemory => {
                add_coins_from_selector(&dep.market_selector, &mut out.pattern_memory_coins);
            }
            CryptoStrategyKind::SplitArb => {
                add_coins_from_selector(&dep.market_selector, &mut out.split_arb_coins);
                if let Some(h) = normalize_horizon(dep.timeframe.as_str()) {
                    out.split_arb_horizons.insert(h.to_string());
                }
            }
            _ => {}
        }
    }

    out
}

pub(crate) fn apply_strategy_deployments(
    cfg: &mut PlatformBootstrapConfig,
    deployments: &[StrategyDeployment],
    runtime_account_id: &str,
    runtime_dry_run: bool,
) {
    if deployments.is_empty() {
        return;
    }

    let runtime_scoped: Vec<&StrategyDeployment> = deployments
        .iter()
        .filter(|d| d.matches_account(runtime_account_id))
        .filter(|d| d.matches_execution_mode(runtime_dry_run))
        .collect();
    let enabled: Vec<&StrategyDeployment> = runtime_scoped
        .iter()
        .copied()
        .filter(|d| d.enabled)
        .collect();

    cfg.enable_crypto = false;
    cfg.enable_crypto_momentum = false;
    cfg.enable_crypto_pattern_memory = false;
    cfg.enable_crypto_split_arb = false;
    cfg.enable_crypto_pm_5m_directional = false;
    cfg.managed_crypto.enable_lob_ml = false;
    #[cfg(feature = "rl")]
    {
        cfg.managed_crypto.enable_rl_policy = false;
    }
    cfg.enable_sports = false;
    cfg.enable_politics = false;
    cfg.enable_economics = false;

    let mut coins: HashSet<String> = HashSet::new();
    let mut timeframe_summary: HashMap<String, usize> = HashMap::new();
    let mut custom_domains: HashSet<String> = HashSet::new();

    for dep in enabled.iter().copied() {
        *timeframe_summary
            .entry(dep.timeframe.as_str().to_string())
            .or_insert(0) += 1;

        match dep.domain {
            Domain::Crypto => {
                let mapped = match classify_crypto_strategy(&dep.strategy) {
                    CryptoStrategyKind::Momentum => {
                        cfg.enable_crypto_momentum = true;
                        true
                    }
                    CryptoStrategyKind::Pm5mDirectional => {
                        cfg.enable_crypto_pm_5m_directional = true;
                        true
                    }
                    CryptoStrategyKind::PatternMemory => {
                        cfg.enable_crypto_pattern_memory = true;
                        true
                    }
                    CryptoStrategyKind::SplitArb => {
                        cfg.enable_crypto_split_arb = true;
                        true
                    }
                    CryptoStrategyKind::LobMl => {
                        cfg.managed_crypto.enable_lob_ml = true;
                        true
                    }
                    #[cfg(feature = "rl")]
                    CryptoStrategyKind::RlPolicy => {
                        cfg.managed_crypto.enable_rl_policy = true;
                        true
                    }
                    CryptoStrategyKind::Unknown => {
                        warn!(
                            deployment_id = %dep.id,
                            strategy = %dep.strategy,
                            "unknown crypto strategy in deployment matrix; skipping built-in mapping"
                        );
                        false
                    }
                };

                if mapped {
                    cfg.enable_crypto = true;
                    add_coins_from_selector(&dep.market_selector, &mut coins);
                }
            }
            Domain::Sports => cfg.enable_sports = true,
            Domain::Politics => cfg.enable_politics = true,
            Domain::Economics => cfg.enable_economics = true,
            Domain::Custom(ref custom_domain) => {
                custom_domains.insert(format!("custom:{custom_domain}"));
            }
        }
    }

    if !coins.is_empty() {
        let mut sorted: Vec<String> = coins.into_iter().collect();
        sorted.sort();
        cfg.crypto.coins = sorted.clone();
        cfg.managed_crypto.lob_ml.coins = sorted.clone();
        #[cfg(feature = "rl")]
        {
            cfg.managed_crypto.rl_policy.coins = sorted.clone();
        }
    }

    let mut tf: Vec<String> = timeframe_summary
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    tf.sort();
    if !custom_domains.is_empty() {
        let mut custom: Vec<String> = custom_domains.into_iter().collect();
        custom.sort();
        warn!(
            domains = ?custom,
            "custom deployments detected without built-in runtime agent registration"
        );
    }
    #[cfg(feature = "rl")]
    let crypto_rl_policy_enabled = cfg.managed_crypto.enable_rl_policy;
    #[cfg(not(feature = "rl"))]
    let crypto_rl_policy_enabled = false;

    info!(
        total = deployments.len(),
        scoped = runtime_scoped.len(),
        enabled = enabled.len(),
        runtime_account_id = runtime_account_id,
        runtime_dry_run = runtime_dry_run,
        crypto = cfg.enable_crypto,
        crypto_momentum = cfg.enable_crypto_momentum,
        crypto_pm_5m_directional = cfg.enable_crypto_pm_5m_directional,
        crypto_pattern_memory = cfg.enable_crypto_pattern_memory,
        crypto_split_arb = cfg.enable_crypto_split_arb,
        crypto_lob_ml = cfg.managed_crypto.enable_lob_ml,
        crypto_rl_policy = crypto_rl_policy_enabled,
        sports = cfg.enable_sports,
        politics = cfg.enable_politics,
        economics = cfg.enable_economics,
        coins = ?cfg.crypto.coins,
        timeframes = ?tf,
        "applied strategy deployment matrix to platform runtime"
    );
}
