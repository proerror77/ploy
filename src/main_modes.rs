use ploy::adapters::PolymarketClient;
use ploy::cli::legacy::Cli;
use ploy::config::AppConfig;
use ploy::coordinator::bootstrap::{start_platform, PlatformBootstrapConfig, PlatformStartControl};
use ploy::error::{PloyError, Result};
use tracing::{error, info, warn};

fn build_platform_config_for_runtime(
    app_config: &AppConfig,
    crypto: bool,
    sports: bool,
    politics: bool,
    dry_run: bool,
) -> PlatformBootstrapConfig {
    let mut platform_cfg = PlatformBootstrapConfig::from_app_config(app_config);

    if dry_run {
        platform_cfg.dry_run = true;
    }

    // Always reapply deployment matrix after runtime overrides so execution_mode/account scope
    // is evaluated against the effective runtime (e.g., CLI --dry-run).
    platform_cfg.reapply_strategy_deployments_for_runtime(app_config);

    // Explicit domain flags work as a filter, not an override.
    // This prevents bypassing deployment matrix enable/disable controls.
    let explicit_selection = crypto || sports || politics;
    if explicit_selection {
        if !crypto {
            platform_cfg.enable_crypto = false;
            platform_cfg.enable_crypto_momentum = false;
            platform_cfg.enable_crypto_lob_ml = false;
            #[cfg(feature = "rl")]
            {
                platform_cfg.enable_crypto_rl_policy = false;
            }
        }
        if !sports {
            platform_cfg.enable_sports = false;
        }
        if !politics {
            platform_cfg.enable_politics = false;
        }
    }

    if app_config.openclaw_runtime_lockdown() {
        platform_cfg.enable_crypto = false;
        platform_cfg.enable_crypto_momentum = false;
        platform_cfg.enable_crypto_lob_ml = false;
        #[cfg(feature = "rl")]
        {
            platform_cfg.enable_crypto_rl_policy = false;
        }
        platform_cfg.enable_sports = false;
        platform_cfg.enable_politics = false;
        warn!("platform started in openclaw lockdown mode; built-in agents forced off");
    }

    platform_cfg
}

pub async fn run_claimer(check_only: bool, min_size: f64, interval: u64) -> Result<()> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::signing::Wallet;
    use ploy::strategy::{AutoClaimer, ClaimerConfig};
    use rust_decimal::Decimal;
    use std::str::FromStr;

    info!(
        "Starting auto-claimer (check_only={}, min_size={}, interval={}s)",
        check_only, min_size, interval
    );

    let private_key = std::env::var("POLYMARKET_PRIVATE_KEY")
        .or_else(|_| std::env::var("PRIVATE_KEY"))
        .ok();

    if private_key.is_none() && !check_only {
        warn!("No POLYMARKET_PRIVATE_KEY found - running in check-only mode");
    }

    let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;

    let funder = std::env::var("POLYMARKET_FUNDER").ok();
    let client = if let Some(ref funder_addr) = funder {
        info!("Using proxy wallet, funder: {}", funder_addr);
        PolymarketClient::new_authenticated_proxy(
            "https://clob.polymarket.com",
            wallet,
            funder_addr,
            false,
        )
        .await?
    } else {
        PolymarketClient::new_authenticated("https://clob.polymarket.com", wallet, false).await?
    };

    let config = ClaimerConfig {
        check_interval_secs: if interval > 0 { interval } else { 60 },
        min_claim_size: Decimal::from_str(&min_size.to_string()).unwrap_or(Decimal::ONE),
        auto_claim: !check_only && private_key.is_some(),
        private_key,
    };

    let claimer = AutoClaimer::new(client, config);

    if interval == 0 {
        info!("One-shot mode: checking for redeemable positions...");
        let positions = claimer.check_once().await?;

        if positions.is_empty() {
            info!("No redeemable positions found");
        } else {
            info!("Found {} redeemable positions:", positions.len());
            for pos in &positions {
                info!(
                    "  • {} {} shares = ${:.2} | condition={}",
                    pos.outcome,
                    pos.size,
                    pos.payout,
                    &pos.condition_id[..16.min(pos.condition_id.len())]
                );
            }

            if !check_only {
                info!("Claiming positions...");
                let results = claimer.check_and_claim().await?;
                for result in results {
                    if result.success {
                        info!(
                            "✅ Claimed ${:.2} from {} | tx: {}",
                            result.amount_claimed, result.condition_id, result.tx_hash
                        );
                    } else {
                        error!(
                            "❌ Failed to claim {}: {:?}",
                            result.condition_id, result.error
                        );
                    }
                }
            }
        }
    } else {
        info!(
            "Starting continuous claiming service (interval: {}s)...",
            interval
        );
        claimer.start().await?;
    }

    Ok(())
}

pub async fn run_platform_mode(
    action: &str,
    crypto: bool,
    sports: bool,
    politics: bool,
    dry_run: bool,
    pause: Option<String>,
    resume: Option<String>,
    cli: &Cli,
) -> Result<()> {
    let app_config = AppConfig::load_from(&cli.config).unwrap_or_else(|e| {
        warn!("Failed to load config: {}, using defaults", e);
        AppConfig::default_config(true, "btc-price-series-15m")
    });

    if action != "start" {
        return Err(PloyError::Validation(format!(
            "unsupported platform action '{}'; only 'start' is supported",
            action
        )));
    }

    let platform_cfg =
        build_platform_config_for_runtime(&app_config, crypto, sports, politics, dry_run);

    info!(
        "Platform mode: crypto={} sports={} politics={} dry_run={}",
        platform_cfg.enable_crypto,
        platform_cfg.enable_sports,
        platform_cfg.enable_politics,
        platform_cfg.dry_run,
    );

    let pm_client =
        crate::create_pm_client(&app_config.market.rest_url, platform_cfg.dry_run).await?;

    let control = PlatformStartControl { pause, resume };
    start_platform(platform_cfg, pm_client, &app_config, control).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    #[derive(Default)]
    struct EnvOverride {
        previous: Vec<(String, Option<String>)>,
    }

    impl EnvOverride {
        fn set(&mut self, key: &str, value: &str) {
            if !self.previous.iter().any(|(existing, _)| existing == key) {
                self.previous.push((key.to_string(), env::var(key).ok()));
            }
            unsafe {
                env::set_var(key, value);
            }
        }

        fn remove(&mut self, key: &str) {
            if !self.previous.iter().any(|(existing, _)| existing == key) {
                self.previous.push((key.to_string(), env::var(key).ok()));
            }
            unsafe {
                env::remove_var(key);
            }
        }
    }

    impl Drop for EnvOverride {
        fn drop(&mut self) {
            for (key, value) in self.previous.iter().rev() {
                if let Some(value) = value {
                    unsafe {
                        env::set_var(key, value);
                    }
                } else {
                    unsafe {
                        env::remove_var(key);
                    }
                }
            }
        }
    }

    #[test]
    fn runtime_dry_run_override_reapplies_deployment_execution_mode() {
        let _guard = env_lock().lock().expect("failed to lock env");
        let mut env_override = EnvOverride::default();
        env_override.remove("PLOY_DEPLOYMENTS_JSON");
        env_override.set(
            "PLOY_STRATEGY_DEPLOYMENTS_JSON",
            r#"[
              {
                "id":"dep-crypto-dryrun-only",
                "strategy":"momentum",
                "domain":"Crypto",
                "market_selector":{"mode":"dynamic","domain":"Crypto","query":"BTC"},
                "timeframe":"5m",
                "enabled":true,
                "allocator_profile":"default",
                "risk_profile":"default",
                "priority":90,
                "cooldown_secs":60,
                "execution_mode":"dry_run_only"
              }
            ]"#,
        );

        let app = AppConfig::default_config(false, "btc-price-series-15m");

        let live_cfg = build_platform_config_for_runtime(&app, false, false, false, false);
        assert!(
            !live_cfg.enable_crypto,
            "dry_run_only deployment should not enable crypto in live runtime"
        );

        let dry_cfg = build_platform_config_for_runtime(&app, false, false, false, true);
        assert!(
            dry_cfg.dry_run,
            "runtime dry-run override should be applied"
        );
        assert!(
            dry_cfg.enable_crypto,
            "dry_run_only deployment should enable crypto in dry-run runtime"
        );
    }

    #[test]
    fn explicit_domain_flags_filter_deployments_instead_of_overriding() {
        let _guard = env_lock().lock().expect("failed to lock env");
        let mut env_override = EnvOverride::default();
        env_override.remove("PLOY_DEPLOYMENTS_JSON");
        env_override.set(
            "PLOY_STRATEGY_DEPLOYMENTS_JSON",
            r#"[
              {
                "id":"dep-sports-only",
                "strategy":"sports",
                "domain":"Sports",
                "market_selector":{"mode":"dynamic","domain":"Sports","query":"NBA"},
                "timeframe":"15m",
                "enabled":true,
                "allocator_profile":"default",
                "risk_profile":"default",
                "priority":80,
                "cooldown_secs":120
              }
            ]"#,
        );

        let app = AppConfig::default_config(false, "btc-price-series-15m");
        let cfg = build_platform_config_for_runtime(&app, true, false, false, false);

        assert!(
            !cfg.enable_crypto,
            "explicit --crypto must not bypass deployment matrix if crypto has no enabled deployment"
        );
        assert!(
            !cfg.enable_sports,
            "explicit selection should filter out unselected domains"
        );
    }
}

pub async fn run_paper_trading(
    symbols: String,
    min_vol_edge: f64,
    min_price_edge: f64,
    log_file: String,
    stats_interval: u64,
) -> Result<()> {
    use ploy::strategy::{run_paper_trading, PaperTradingConfig, VolatilityArbConfig};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    let symbols: Vec<String> = symbols
        .split(',')
        .map(|s| s.trim().to_uppercase())
        .collect();

    let series_ids: Vec<String> = symbols
        .iter()
        .filter_map(|s| match s.trim_end_matches("USDT") {
            "BTC" => Some("btc-price-series-15m".into()),
            "ETH" => Some("eth-price-series-15m".into()),
            "SOL" => Some("sol-price-series-15m".into()),
            _ => None,
        })
        .collect();

    let mut vol_arb_config = VolatilityArbConfig::default();
    vol_arb_config.min_vol_edge_pct = min_vol_edge / 100.0;
    vol_arb_config.min_price_edge =
        Decimal::from_f64_retain(min_price_edge / 100.0).unwrap_or(dec!(0.02));
    vol_arb_config.symbols = symbols.clone();

    let config = PaperTradingConfig {
        vol_arb_config,
        symbols,
        series_ids,
        kline_update_interval_secs: 60,
        stats_interval_secs: stats_interval,
        log_file: Some(log_file),
    };

    let pm_client = PolymarketClient::new("https://clob.polymarket.com", true)?;
    run_paper_trading(pm_client, Some(config)).await?;

    Ok(())
}

pub async fn run_history(
    limit: usize,
    symbol: Option<String>,
    stats_only: bool,
    open_only: bool,
) -> Result<()> {
    use ploy::strategy::TradeLogger;
    use std::path::PathBuf;

    let logger = TradeLogger::new(PathBuf::from("data/trades.json"));

    if let Err(e) = logger.load().await {
        eprintln!("Warning: Could not load trades: {}", e);
    }

    let stats = logger.get_stats().await;

    if stats.total_trades == 0 {
        println!("\n  No trading history found.");
        println!("  Trade data will be saved to: data/trades.json\n");
        return Ok(());
    }

    println!("{}", logger.format_stats().await);

    if stats_only {
        return Ok(());
    }

    let trades = if open_only {
        logger.get_open_trades().await
    } else if let Some(ref sym) = symbol {
        logger.get_trades_by_symbol(sym).await
    } else {
        logger.get_recent_trades(limit).await
    };

    if trades.is_empty() {
        if open_only {
            println!("\n  No open trades.\n");
        } else if let Some(sym) = &symbol {
            println!("\n  No trades for symbol: {}\n", sym);
        }
        return Ok(());
    }

    println!("\n  ── Recent Trades ───────────────────────────────────────────\n");
    println!("  Time                Symbol     Dir   Price   Shares  PnL      Status");
    println!("  ──────────────────  ─────────  ────  ──────  ──────  ───────  ──────");

    for trade in trades {
        let outcome_str = match &trade.outcome {
            ploy::strategy::TradeOutcome::Open => "OPEN",
            ploy::strategy::TradeOutcome::Won => "WON",
            ploy::strategy::TradeOutcome::Lost => "LOST",
            ploy::strategy::TradeOutcome::ExitedEarly { .. } => "EXIT",
            ploy::strategy::TradeOutcome::Cancelled => "CANCEL",
        };

        let pnl_str = match trade.pnl_usd {
            Some(pnl) => format!("${:+.2}", pnl),
            None => "-".to_string(),
        };

        println!(
            "  {}  {:10} {:4}  {:5.1}¢  {:6}  {:>7}  {}",
            trade.timestamp.format("%Y-%m-%d %H:%M"),
            trade.symbol,
            trade.direction,
            trade.entry_price * rust_decimal_macros::dec!(100),
            trade.shares,
            pnl_str,
            outcome_str
        );
    }

    println!();
    Ok(())
}
