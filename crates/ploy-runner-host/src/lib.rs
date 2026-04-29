#[cfg(feature = "ops")]
mod ops;
mod run;

pub fn print_usage() {
    print_usage_for("ploy-runner");
}

fn print_usage_for(program: &str) {
    eprintln!("Usage: {program} [COMMAND] [OPTIONS]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  run               Run the strategy (default)");
    #[cfg(feature = "ops")]
    eprintln!("  check-db          Check database data completeness");
    #[cfg(feature = "ops")]
    eprintln!("  collect-quotes    Collect orderbook quotes from Polymarket CLOB WebSocket");
    eprintln!();
    eprintln!("Options for 'run':");
    eprintln!("  --config <path>          Unified TOML config file (required)");
    eprintln!("  --deployment-id <id>     Platform deployment identity for order attribution");
    eprintln!("  --dry-run                Force dry-run mode (simulated execution)");
    eprintln!("  --foreground             Run in foreground (default, kept for compat)");
    #[cfg(feature = "ops")]
    ops::print_usage();
}

fn print_mode_usage(program: &str) {
    eprintln!("Usage: {program} --config <path> [--deployment-id <id>] [--dry-run] [--foreground]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --config <path>          Unified TOML config file (required)");
    eprintln!("  --deployment-id <id>     Platform deployment identity for order attribution");
    eprintln!("  --dry-run                Force dry-run mode (simulated execution)");
    eprintln!("  --foreground             Run in foreground (default, kept for compat)");
}

fn program_name(args: &[String]) -> String {
    args.first()
        .and_then(|arg| std::path::Path::new(arg).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("ploy-runner")
        .to_string()
}

pub async fn run_with_args(args: Vec<String>) {
    init_tracing();

    let command = args.get(1).map(|s| s.as_str());
    match command {
        Some("check-db") => {
            run_check_db(&args).await;
        }
        Some("collect-quotes") => {
            run_collect_quotes(&args).await;
        }
        Some("run") | None => {
            run::run_command(&args, command).await;
        }
        Some("--help") | Some("-h") => {
            print_usage();
        }
        Some(other) => {
            eprintln!("Unknown command: {other}");
            print_usage();
            std::process::exit(1);
        }
    }
}

pub async fn run_mode_binary(args: Vec<String>) {
    if matches!(args.get(1).map(String::as_str), Some("--help" | "-h")) {
        print_mode_usage(&program_name(&args));
        return;
    }

    run_with_args(normalize_mode_args(args)).await;
}

pub async fn run_with_implicit_run_args(args: Vec<String>) {
    run_with_args(normalize_mode_args(args)).await;
}

fn normalize_mode_args(mut args: Vec<String>) -> Vec<String> {
    match args.get(1).map(String::as_str) {
        None | Some("--config" | "--deployment-id" | "--dry-run" | "--foreground") => {
            args.insert(1, "run".to_string());
        }
        _ => {}
    }
    args
}

fn init_tracing() {
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
}

#[cfg(feature = "ops")]
async fn run_check_db(args: &[String]) {
    ops::run_check_db(args).await;
}

#[cfg(not(feature = "ops"))]
async fn run_check_db(_args: &[String]) {
    eprintln!("The check-db command requires the full/ops runner build");
    std::process::exit(1);
}

#[cfg(feature = "ops")]
async fn run_collect_quotes(args: &[String]) {
    ops::run_collect_quotes(args).await;
}

#[cfg(not(feature = "ops"))]
async fn run_collect_quotes(_args: &[String]) {
    eprintln!("The collect-quotes command requires the full/ops runner build");
    std::process::exit(1);
}
