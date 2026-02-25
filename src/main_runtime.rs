use ploy::adapters::PolymarketClient;
use ploy::error::Result;
use ploy::safety::direct_live;
use tracing::warn;
use tracing_subscriber::EnvFilter;

pub async fn create_pm_client(rest_url: &str, dry_run: bool) -> Result<PolymarketClient> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::signing::Wallet;

    if dry_run {
        return PolymarketClient::new(rest_url, true);
    }

    let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
    let funder = std::env::var("POLYMARKET_FUNDER").ok();
    if let Some(funder_addr) = funder {
        PolymarketClient::new_authenticated_proxy(rest_url, wallet, &funder_addr, true).await
    } else {
        PolymarketClient::new_authenticated(rest_url, wallet, true).await
    }
}

pub fn enforce_coordinator_only_live(cmd: &str) -> Result<()> {
    let result = direct_live::enforce_live_gate(cmd);
    if let Err(ref e) = result {
        warn!("{e}");
        println!("\x1b[31m✗ {e}\x1b[0m");
    }
    result
}

pub fn init_logging() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ploy=debug,sqlx=warn"));

    // Check if we should write to file (prefer PLOY_LOG_DIR, fallback to LOG_DIR or /var/log/ploy).
    let log_dir = std::env::var("PLOY_LOG_DIR")
        .or_else(|_| std::env::var("LOG_DIR"))
        .unwrap_or_else(|_| "/var/log/ploy".to_string());

    // Try to create log directory.
    //
    // Important: `tracing_appender::rolling::daily` will panic (and in our release build,
    // abort) if it can't create the initial log file. So we must preflight writability.
    let file_layer = if std::fs::create_dir_all(&log_dir).is_ok() {
        let test_path = std::path::Path::new(&log_dir).join(".ploy_write_test");
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&test_path)
        {
            Ok(_) => {
                let _ = std::fs::remove_file(&test_path);

                // Daily rotating file appender
                let file_appender = tracing_appender::rolling::daily(&log_dir, "ploy.log");
                let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

                // Keep the guard alive by leaking it (acceptable for long-running process)
                Box::leak(Box::new(_guard));

                Some(
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .with_ansi(false) // No color codes in file
                        .with_target(true),
                )
            }
            Err(e) => {
                eprintln!(
                    "Warning: Could not write to log directory {} ({}), file logging disabled",
                    log_dir, e
                );
                None
            }
        }
    } else {
        eprintln!(
            "Warning: Could not create log directory {}, file logging disabled",
            log_dir
        );
        None
    };

    // Console layer
    let console_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false);

    // Combine layers
    let file_logging_enabled = file_layer.is_some();
    tracing_subscriber::registry()
        .with(filter)
        .with(console_layer)
        .with(file_layer)
        .init();

    if file_logging_enabled {
        eprintln!("Logging to: {}/ploy.log", log_dir);
    }
}

pub fn init_logging_simple() {
    // Minimal logging for CLI commands
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
}
