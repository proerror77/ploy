use ploy_market_data::collector::{CollectorConfig, QuoteCollector};
use ploy_market_data::diagnostics::check_database;
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
    };

    let collector = QuoteCollector::new(config, pool);
    if let Err(error) = collector.run().await {
        eprintln!("Quote collector failed: {error}");
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
