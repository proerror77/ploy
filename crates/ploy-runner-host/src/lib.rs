#[cfg(feature = "ops")]
use ploy_market_data::collector::{CollectorConfig, QuoteCollector};
#[cfg(feature = "ops")]
use ploy_market_data::diagnostics::check_database;
use ploy_strategy_bundles::FullConfig;
use ploy_strategy_runtime::run_strategy;
#[cfg(feature = "ops")]
use sqlx::postgres::PgPoolOptions;

pub fn print_usage() {
    eprintln!("Usage: ploy-runner [COMMAND] [OPTIONS]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  run               Run the strategy (default)");
    #[cfg(feature = "ops")]
    eprintln!("  check-db          Check database data completeness");
    #[cfg(feature = "ops")]
    eprintln!("  collect-quotes    Collect orderbook quotes from Polymarket CLOB WebSocket");
    eprintln!();
    eprintln!("Options for 'run':");
    eprintln!("  --config <path>   Unified TOML config file (required)");
    eprintln!("  --dry-run         Force dry-run mode (simulated execution)");
    eprintln!("  --foreground      Run in foreground (default, kept for compat)");
    eprintln!();
    #[cfg(feature = "ops")]
    {
        eprintln!("Options for 'check-db':");
        eprintln!(
        "  --db-url <url>    Database URL (default: postgresql://postgres:postgres@localhost:5432/ploy)"
    );
        eprintln!();
        eprintln!("Options for 'collect-quotes':");
        eprintln!("  --symbols <list>  Comma-separated symbols (default: BTCUSDT,ETHUSDT,SOLUSDT)");
        eprintln!("  --timeframe <tf>  Market timeframe: 5m or 15m (default: 5m)");
        eprintln!(
        "  --db-url <url>    Database URL (default: postgresql://postgres:postgres@localhost:5432/ploy)"
    );
    }
}

pub async fn run_with_args(args: Vec<String>) {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            "info,hyper_util=off,hyper=off,reqwest=off,h2=off,rustls=off"
                .parse()
                .unwrap()
        })
        .add_directive(
            "polymarket_client_sdk::serde_helpers=error"
                .parse()
                .unwrap(),
        );
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .try_init();

    let command = args.get(1).map(|s| s.as_str());
    match command {
        Some("check-db") => {
            #[cfg(not(feature = "ops"))]
            {
                eprintln!("The check-db command requires the full/ops runner build");
                std::process::exit(1);
            }

            #[cfg(feature = "ops")]
            {
                let db_url = args
                    .iter()
                    .position(|s| s == "--db-url")
                    .and_then(|i| args.get(i + 1))
                    .map(|s| s.as_str())
                    .unwrap_or("postgresql://postgres:postgres@localhost:5432/ploy");

                if let Err(error) = check_database(db_url).await {
                    eprintln!("Database check failed: {error}");
                    std::process::exit(1);
                }
                return;
            }
        }
        Some("collect-quotes") => {
            #[cfg(not(feature = "ops"))]
            {
                eprintln!("The collect-quotes command requires the full/ops runner build");
                std::process::exit(1);
            }

            #[cfg(feature = "ops")]
            {
                let db_url = args
                    .iter()
                    .position(|s| s == "--db-url")
                    .and_then(|i| args.get(i + 1))
                    .map(|s| s.as_str())
                    .unwrap_or("postgresql://postgres:postgres@localhost:5432/ploy");

                let symbols_str = args
                    .iter()
                    .position(|s| s == "--symbols")
                    .and_then(|i| args.get(i + 1))
                    .map(|s| s.as_str())
                    .unwrap_or("BTCUSDT,ETHUSDT,SOLUSDT");

                let timeframe = args
                    .iter()
                    .position(|s| s == "--timeframe")
                    .and_then(|i| args.get(i + 1))
                    .map(|s| s.as_str())
                    .unwrap_or("5m");

                let symbols: Vec<String> = symbols_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect();

                let pool = match PgPoolOptions::new()
                    .max_connections(5)
                    .connect(db_url)
                    .await
                {
                    Ok(pool) => pool,
                    Err(error) => {
                        eprintln!("Failed to connect to database: {error}");
                        std::process::exit(1);
                    }
                };

                let config = CollectorConfig {
                    symbols,
                    timeframe: timeframe.to_string(),
                    refresh_interval_secs: 300,
                };

                let collector = QuoteCollector::new(config, pool);
                if let Err(error) = collector.run().await {
                    eprintln!("Quote collector failed: {error}");
                    std::process::exit(1);
                }
                return;
            }
        }
        Some("run") | None => {}
        Some("--help") | Some("-h") => {
            print_usage();
            return;
        }
        Some(other) => {
            eprintln!("Unknown command: {other}");
            print_usage();
            std::process::exit(1);
        }
    }

    let mut config_path: Option<String> = None;
    let mut force_dry_run = false;
    let mut i = if command == Some("run") { 2 } else { 1 };
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                config_path = args.get(i).cloned();
            }
            "--dry-run" => force_dry_run = true,
            "--foreground" => {}
            "--help" | "-h" => {
                print_usage();
                return;
            }
            other => {
                eprintln!("Unknown argument: {other}");
                print_usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let config_path = config_path.unwrap_or_else(|| {
        eprintln!("Error: --config is required");
        print_usage();
        std::process::exit(1);
    });

    let config = match FullConfig::from_file(&config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Failed to load config {config_path}: {error}");
            std::process::exit(1);
        }
    };

    run_strategy(config, &config_path, force_dry_run).await;
}
