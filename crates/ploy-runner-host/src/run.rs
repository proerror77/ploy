use ploy_strategy_bundles::FullConfig;
use ploy_strategy_runtime::run_strategy_with_deployment_id;

use crate::print_usage;

pub async fn run_command(args: &[String], command: Option<&str>) {
    let mut config_path: Option<String> = None;
    let mut deployment_id: Option<String> = None;
    let mut force_dry_run = false;
    let mut i = if command == Some("run") { 2 } else { 1 };
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                if i + 1 >= args.len() || args[i + 1].starts_with("--") {
                    eprintln!("Error: --config requires a value");
                    print_usage();
                    std::process::exit(1);
                }
                i += 1;
                config_path = args.get(i).cloned();
            }
            "--deployment-id" => {
                if i + 1 >= args.len() || args[i + 1].starts_with("--") {
                    eprintln!("Error: --deployment-id requires a value");
                    print_usage();
                    std::process::exit(1);
                }
                i += 1;
                deployment_id = args.get(i).cloned();
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

    run_strategy_with_deployment_id(config, &config_path, force_dry_run, deployment_id).await;
}
