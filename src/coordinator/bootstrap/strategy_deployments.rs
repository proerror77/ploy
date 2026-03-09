use super::*;

fn normalize_strategy_key(strategy: &str) -> String {
    strategy.to_ascii_lowercase().replace(['-', '_', ' '], "")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CryptoStrategyKind {
    Momentum,
    PatternMemory,
    SplitArb,
    LobMl,
    #[cfg(feature = "rl")]
    RlPolicy,
    Unknown,
}

fn classify_crypto_strategy(strategy: &str) -> CryptoStrategyKind {
    let key = normalize_strategy_key(strategy);

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

pub(super) fn crypto_series_id_for(coin: &str, horizon: &str) -> Option<&'static str> {
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

pub(super) fn coin_symbol_for(coin: &str) -> Option<String> {
    let c = coin.to_ascii_uppercase();
    if c.is_empty() {
        return None;
    }
    Some(format!("{}USDT", c))
}

pub(super) fn symbol_for_crypto_series_id(series_id: &str) -> Option<&'static str> {
    match series_id {
        "10684" | "10192" => Some("BTCUSDT"),
        "10683" | "10191" => Some("ETHUSDT"),
        "10686" | "10423" => Some("SOLUSDT"),
        "10685" | "10422" => Some("XRPUSDT"),
        _ => None,
    }
}

#[derive(Debug, Default)]
pub(super) struct RuntimeCryptoStrategyTargets {
    pub(super) pattern_memory_coins: HashSet<String>,
    pub(super) split_arb_coins: HashSet<String>,
    pub(super) split_arb_horizons: HashSet<String>,
}

pub(super) fn collect_runtime_crypto_strategy_targets(
    runtime_account_id: &str,
    runtime_dry_run: bool,
) -> RuntimeCryptoStrategyTargets {
    let deployments = load_strategy_deployments();
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

pub(super) fn build_pattern_memory_runtime_config(coins: &[String]) -> Result<String> {
    let mut selected: Vec<String> = coins
        .iter()
        .filter_map(|c| {
            c.strip_suffix("USDT")
                .map(|s| s.to_string())
                .or_else(|| Some(c.clone()))
        })
        .map(|c| c.to_ascii_uppercase())
        .collect();
    selected.sort();
    selected.dedup();

    let mut markets_block = String::new();
    for coin in selected {
        if let (Some(symbol), Some(series_id)) =
            (coin_symbol_for(&coin), crypto_series_id_for(&coin, "5m"))
        {
            markets_block.push_str("\n[[markets]]\n");
            markets_block.push_str(&format!("symbol = \"{}\"\n", symbol));
            markets_block.push_str(&format!("series_id = \"{}\"\n", series_id));
        }
    }

    if markets_block.trim().is_empty() {
        return Err(crate::error::PloyError::Validation(
            "pattern_memory runtime has no recognized crypto coins/series ids".to_string(),
        ));
    }

    Ok(format!(
        r#"# Auto-generated by platform bootstrap
[strategy]
name = "pattern_memory"
enabled = true
{markets}
[pattern]
corr_threshold = 0.70
alpha = 1.0
beta = 1.0
min_matches = 3
min_n_eff = 2.0
min_confidence = 0.60

[filter_15m]
enabled = true
min_confidence = 0.55
min_n_eff = 1.0

[timing]
target_remaining_secs = 300
tolerance_secs = 45
min_remaining_secs = 60

[trade]
shares = 100
max_entry_price = 0.55
min_net_ev = 0.0
cooldown_secs = 30
"#,
        markets = markets_block
    ))
}

fn render_momentum_runtime_config(
    mut config: toml::Value,
    crypto_cfg: &CryptoTradingConfig,
    symbols: &[String],
) -> String {
    let root = config
        .as_table_mut()
        .expect("momentum runtime config must be a table");
    let strategy = root
        .entry("strategy")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[strategy] must be a table");
    strategy.insert("name".to_string(), toml::Value::String("momentum".to_string()));
    strategy.insert("enabled".to_string(), toml::Value::Boolean(true));
    strategy.insert(
        "mode".to_string(),
        toml::Value::String(if crypto_cfg.enable_price_exits {
            "predictive".to_string()
        } else {
            "confirmatory".to_string()
        }),
    );

    let entry = root
        .entry("entry")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[entry] must be a table");
    entry.insert(
        "symbols".to_string(),
        toml::Value::Array(symbols.iter().cloned().map(toml::Value::String).collect()),
    );
    entry.insert(
        "min_move".to_string(),
        toml::Value::Float(
            (crypto_cfg.min_window_move_pct * rust_decimal_macros::dec!(100))
                .to_f64()
                .unwrap_or(0.01),
        ),
    );
    entry.insert(
        "min_edge".to_string(),
        toml::Value::Float(
            (crypto_cfg.min_edge * rust_decimal_macros::dec!(100))
                .to_f64()
                .unwrap_or(5.0),
        ),
    );
    entry.insert(
        "cooldown_secs".to_string(),
        toml::Value::Float(crypto_cfg.entry_cooldown_secs as f64),
    );
    entry.insert(
        "require_mtf_agreement".to_string(),
        toml::Value::Boolean(crypto_cfg.require_mtf_agreement),
    );
    entry.insert(
        "directional_mode".to_string(),
        toml::Value::Boolean(matches!(crypto_cfg.entry_mode, CryptoEntryMode::Directional)),
    );
    entry.insert(
        "directional_entry_threshold".to_string(),
        toml::Value::Float(
            (crypto_cfg.min_edge * rust_decimal_macros::dec!(100))
                .to_f64()
                .unwrap_or(5.0),
        ),
    );
    entry.insert(
        "min_confidence".to_string(),
        toml::Value::Float(crypto_cfg.min_signal_score.to_f64().unwrap_or(0.4)),
    );

    let timing = root
        .entry("timing")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[timing] must be a table");
    timing.insert(
        "min_time_remaining".to_string(),
        toml::Value::Float(crypto_cfg.min_time_remaining_secs as f64),
    );
    timing.insert(
        "max_time_remaining".to_string(),
        toml::Value::Float(crypto_cfg.max_time_remaining_secs as f64),
    );
    timing.insert(
        "cooldown_secs".to_string(),
        toml::Value::Float(crypto_cfg.entry_cooldown_secs as f64),
    );

    let risk = root
        .entry("risk")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[risk] must be a table");
    risk.insert(
        "shares".to_string(),
        toml::Value::Float(crypto_cfg.default_shares as f64),
    );
    risk.insert(
        "max_positions".to_string(),
        toml::Value::Float(crypto_cfg.risk_params.max_unhedged_positions.max(1) as f64),
    );

    let exit = root
        .entry("exit")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[exit] must be a table");
    exit.insert(
        "exit_edge_floor_pct".to_string(),
        toml::Value::Float(
            (crypto_cfg.exit_edge_floor * rust_decimal_macros::dec!(100))
                .to_f64()
                .unwrap_or(20.0),
        ),
    );
    exit.insert(
        "exit_price_band_pct".to_string(),
        toml::Value::Float(
            (crypto_cfg.exit_price_band * rust_decimal_macros::dec!(100))
                .to_f64()
                .unwrap_or(12.0),
        ),
    );

    format!(
        "# Auto-generated by platform bootstrap — momentum runtime\n{}",
        toml::to_string(&config).expect("runtime config must serialize to TOML")
    )
}

fn load_momentum_config_file(
    crypto_cfg: &CryptoTradingConfig,
    symbols: &[String],
) -> Option<String> {
    let candidates = [
        std::env::var("PLOY_MOMENTUM_CONFIG").ok(),
        Some("config/strategies/momentum.toml".to_string()),
        Some("/root/ploy/config/strategies/momentum.toml".to_string()),
        Some("/opt/ploy/config/strategies/momentum.toml".to_string()),
    ];
    for candidate in candidates.iter().flatten() {
        if let Ok(contents) = std::fs::read_to_string(candidate) {
            if let Ok(val) = toml::from_str::<toml::Value>(&contents) {
                if val.get("strategy").is_some() {
                    info!(path = %candidate, "loaded momentum config from external file");
                    return Some(render_momentum_runtime_config(val, crypto_cfg, symbols));
                }
            }
            warn!(path = %candidate, "momentum config file found but invalid TOML");
        }
    }
    None
}

pub(super) fn build_momentum_runtime_config(crypto_cfg: &CryptoTradingConfig) -> Result<String> {
    if !matches!(crypto_cfg.entry_mode, CryptoEntryMode::Directional) {
        return Err(crate::error::PloyError::Validation(format!(
            "momentum managed runtime only supports directional entry_mode for now; got {:?}",
            crypto_cfg.entry_mode
        )));
    }

    let mut symbols: Vec<String> = crypto_cfg
        .coins
        .iter()
        .map(|coin| format!("{}USDT", coin.trim_end_matches("USDT").to_ascii_uppercase()))
        .collect();
    symbols.sort();
    symbols.dedup();

    if symbols.is_empty() {
        return Err(crate::error::PloyError::Validation(
            "momentum runtime has no recognized crypto symbols".to_string(),
        ));
    }

    if let Some(cfg) = load_momentum_config_file(crypto_cfg, &symbols) {
        return Ok(cfg);
    }

    let config: toml::Value =
        toml::from_str(include_str!("../../../config/strategies/momentum.toml"))
            .expect("embedded momentum runtime config must stay valid TOML");
    Ok(render_momentum_runtime_config(config, crypto_cfg, &symbols))
}

fn render_split_arb_runtime_config(
    mut config: toml::Value,
    symbols: &[String],
    series_ids: &[String],
) -> String {
    let root = config
        .as_table_mut()
        .expect("staggered_arb runtime config must be a table");
    let strategy = root
        .entry("strategy")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[strategy] must be a table");
    strategy.insert("enabled".to_string(), toml::Value::Boolean(true));

    let entry = root
        .entry("entry")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[entry] must be a table");
    entry.insert(
        "symbols".to_string(),
        toml::Value::Array(symbols.iter().cloned().map(toml::Value::String).collect()),
    );

    let markets = root
        .entry("markets")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .expect("[markets] must be a table");
    markets.insert(
        "series_ids".to_string(),
        toml::Value::Array(
            series_ids
                .iter()
                .cloned()
                .map(toml::Value::String)
                .collect(),
        ),
    );

    format!(
        "# Auto-generated by platform bootstrap — staggered arb (時間差套利)\n{}",
        toml::to_string(&config).expect("runtime config must serialize to TOML")
    )
}

fn load_split_arb_config_file(symbols: &[String], series_ids: &[String]) -> Option<String> {
    let candidates = [
        std::env::var("PLOY_STAGGERED_ARB_CONFIG").ok(),
        Some("config/strategies/staggered_arb.toml".to_string()),
        Some("/root/ploy/config/strategies/staggered_arb.toml".to_string()),
        Some("/opt/ploy/config/strategies/staggered_arb.toml".to_string()),
    ];
    for candidate in candidates.iter().flatten() {
        if let Ok(contents) = std::fs::read_to_string(candidate) {
            if let Ok(val) = toml::from_str::<toml::Value>(&contents) {
                if val.get("strategy").is_some() {
                    info!(
                        path = %candidate,
                        "loaded staggered_arb config from external file"
                    );
                    return Some(render_split_arb_runtime_config(val, symbols, series_ids));
                }
            }
            warn!(path = %candidate, "staggered_arb config file found but invalid TOML");
        }
    }
    None
}

pub(super) fn build_split_arb_runtime_config(symbols: &[String], series_ids: &[String]) -> String {
    if let Some(cfg) = load_split_arb_config_file(symbols, series_ids) {
        return cfg;
    }

    let config: toml::Value =
        toml::from_str(include_str!("../../../config/strategies/staggered_arb.toml"))
            .expect("embedded staggered_arb runtime config must stay valid TOML");
    render_split_arb_runtime_config(config, symbols, series_ids)
}

pub(super) fn apply_strategy_deployments(
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
    cfg.enable_crypto_lob_ml = false;
    #[cfg(feature = "rl")]
    {
        cfg.enable_crypto_rl_policy = false;
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
                    CryptoStrategyKind::PatternMemory => {
                        cfg.enable_crypto_pattern_memory = true;
                        true
                    }
                    CryptoStrategyKind::SplitArb => {
                        cfg.enable_crypto_split_arb = true;
                        true
                    }
                    CryptoStrategyKind::LobMl => {
                        cfg.enable_crypto_lob_ml = true;
                        true
                    }
                    #[cfg(feature = "rl")]
                    CryptoStrategyKind::RlPolicy => {
                        cfg.enable_crypto_rl_policy = true;
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
                custom_domains.insert(format!("custom:{}", custom_domain));
            }
        }
    }

    if !coins.is_empty() {
        let mut sorted: Vec<String> = coins.into_iter().collect();
        sorted.sort();
        cfg.crypto.coins = sorted.clone();
        cfg.crypto_lob_ml.coins = sorted.clone();
        #[cfg(feature = "rl")]
        {
            cfg.crypto_rl_policy.coins = sorted.clone();
        }
    }

    let mut tf: Vec<String> = timeframe_summary
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, v))
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
    let crypto_rl_policy_enabled = cfg.enable_crypto_rl_policy;
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
        crypto_pattern_memory = cfg.enable_crypto_pattern_memory,
        crypto_split_arb = cfg.enable_crypto_split_arb,
        crypto_lob_ml = cfg.enable_crypto_lob_ml,
        crypto_rl_policy = crypto_rl_policy_enabled,
        sports = cfg.enable_sports,
        politics = cfg.enable_politics,
        economics = cfg.enable_economics,
        coins = ?cfg.crypto.coins,
        timeframes = ?tf,
        "applied strategy deployment matrix to platform runtime"
    );
}
