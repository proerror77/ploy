use ploy_market_data::collector::{CollectorConfig, QuoteCollector};
use ploy_market_data::diagnostics::check_database;
use ploy_market_data::pm_trades::{TradeCollector, TradeCollectorConfig};
use sqlx::postgres::PgPoolOptions;

pub fn print_usage() {
    eprintln!();
    eprintln!("Options for 'check-db':");
    eprintln!("  --db-url <url>    Database URL (or DATABASE_URL/PLOY_DATABASE__URL)");
    eprintln!();
    eprintln!("Options for 'collect-quotes':");
    eprintln!("  --symbols <list>  Comma-separated symbols (default: BTCUSDT,ETHUSDT,SOLUSDT)");
    eprintln!("  --timeframe <tf>  Market timeframe: 5m or 15m (default: 5m)");
    eprintln!("  --db-url <url>    Database URL (or DATABASE_URL/PLOY_DATABASE__URL)");
    eprintln!(
        "  env PLOY_QUOTE_COLLECTOR_QUEUE_CAPACITY  Bounded DB persistence queue (default: 4096)"
    );
    eprintln!(
        "  env PLOY_QUOTE_COLLECTOR_PERSIST_WORKERS  DB persistence worker count (default: 4)"
    );
    eprintln!(
        "  env PLOY_QUOTE_COLLECTOR_STALE_AFTER_SECS Stale self-restart threshold (default: 120)"
    );
    eprintln!();
    eprintln!("Options for 'collect-pm-trades':");
    eprintln!("  --symbols <list>  Comma-separated symbols (default: BTCUSDT,ETHUSDT,SOLUSDT)");
    eprintln!("  --db-url <url>    Database URL (or DATABASE_URL/PLOY_DATABASE__URL)");
    eprintln!("  env PLOY_PM_TRADE_COLLECTOR_REFRESH_SECS     Poll interval (default: 15)");
    eprintln!(
        "  env PLOY_PM_TRADE_COLLECTOR_MARKET_LOOKBACK_SECS Active market lookback (default: 7200)"
    );
    eprintln!(
        "  env PLOY_PM_TRADE_COLLECTOR_MARKET_LOOKAHEAD_SECS Active market lookahead (default: 7200)"
    );
    eprintln!(
        "  env PLOY_PM_TRADE_COLLECTOR_TRADE_LOOKBACK_SECS  Trade timestamp retention per poll (default: 7200)"
    );
    eprintln!(
        "  env PLOY_PM_TRADE_COLLECTOR_API_LIMIT       Data API limit per market (default: 500)"
    );
    eprintln!(
        "  env PLOY_PM_TRADE_COLLECTOR_DELAY_MS        Delay between market calls (default: 250)"
    );
    eprintln!(
        "  env PLOY_PM_TRADE_COLLECTOR_STALE_AFTER_SECS Stale self-restart threshold (default: 180)"
    );
    eprintln!(
        "  env PLOY_PM_TRADE_COLLECTOR_TAKER_ONLY      true/false Data API takerOnly (default: true)"
    );
}

pub async fn run_check_db(args: &[String]) {
    let db_url = match db_url(args) {
        Ok(db_url) => db_url,
        Err(error) => {
            eprintln!("{error}");
            print_usage();
            std::process::exit(1);
        }
    };

    if let Err(error) = check_database(&db_url).await {
        eprintln!("Database check failed: {error}");
        std::process::exit(1);
    }
}

pub async fn run_collect_quotes(args: &[String]) {
    let db_url = match db_url(args) {
        Ok(db_url) => db_url,
        Err(error) => {
            eprintln!("{error}");
            print_usage();
            std::process::exit(1);
        }
    };

    let symbols_str = arg_value(args, "--symbols")
        .map(String::as_str)
        .unwrap_or("BTCUSDT,ETHUSDT,SOLUSDT");

    let timeframe = arg_value(args, "--timeframe")
        .map(String::as_str)
        .unwrap_or("5m");

    let symbols: Vec<String> = symbols_str
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    let pool = match PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
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
        persist_queue_capacity: env_usize("PLOY_QUOTE_COLLECTOR_QUEUE_CAPACITY", 4_096),
        persist_workers: env_usize("PLOY_QUOTE_COLLECTOR_PERSIST_WORKERS", 4),
        stale_after_secs: env_u64("PLOY_QUOTE_COLLECTOR_STALE_AFTER_SECS", 120),
    };

    let collector = QuoteCollector::new(config, pool);
    if let Err(error) = collector.run().await {
        eprintln!("Quote collector failed: {error}");
        std::process::exit(1);
    }
}

pub async fn run_collect_pm_trades(args: &[String]) {
    let db_url = match db_url(args) {
        Ok(db_url) => db_url,
        Err(error) => {
            eprintln!("{error}");
            print_usage();
            std::process::exit(1);
        }
    };

    let symbols_str = arg_value(args, "--symbols")
        .map(String::as_str)
        .unwrap_or("BTCUSDT,ETHUSDT,SOLUSDT");

    let symbols = parse_symbols(symbols_str);

    let pool = match PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            eprintln!("Failed to connect to database: {error}");
            std::process::exit(1);
        }
    };

    let config = TradeCollectorConfig {
        symbols,
        refresh_interval_secs: env_u64("PLOY_PM_TRADE_COLLECTOR_REFRESH_SECS", 15),
        market_lookback_secs: env_i64("PLOY_PM_TRADE_COLLECTOR_MARKET_LOOKBACK_SECS", 7_200),
        market_lookahead_secs: env_i64("PLOY_PM_TRADE_COLLECTOR_MARKET_LOOKAHEAD_SECS", 7_200),
        trade_lookback_secs: env_i64("PLOY_PM_TRADE_COLLECTOR_TRADE_LOOKBACK_SECS", 7_200),
        api_limit: env_i32("PLOY_PM_TRADE_COLLECTOR_API_LIMIT", 500),
        per_market_delay_ms: env_u64("PLOY_PM_TRADE_COLLECTOR_DELAY_MS", 250),
        stale_after_secs: env_u64("PLOY_PM_TRADE_COLLECTOR_STALE_AFTER_SECS", 180),
        taker_only: env_bool("PLOY_PM_TRADE_COLLECTOR_TAKER_ONLY", true),
    };

    let collector = TradeCollector::new(config, pool);
    if let Err(error) = collector.run().await {
        eprintln!("Polymarket trade collector failed: {error}");
        std::process::exit(1);
    }
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
}

fn db_url(args: &[String]) -> Result<String, &'static str> {
    arg_value(args, "--db-url")
        .cloned()
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .or_else(|| std::env::var("PLOY_DATABASE__URL").ok())
        .filter(|url| !url.trim().is_empty())
        .ok_or("Database URL is required: pass --db-url or set DATABASE_URL/PLOY_DATABASE__URL")
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_i32(name: &str, default: i32) -> i32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "y" => Some(true),
            "0" | "false" | "no" | "n" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

fn parse_symbols(symbols: &str) -> Vec<String> {
    symbols
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}
