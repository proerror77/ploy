use ploy_strategy_bundles::FullConfig;
use ploy_strategy_runtime::run_strategy;

use crate::print_usage;

pub async fn run_command(args: &[String], command: Option<&str>) {
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
