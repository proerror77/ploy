//! Configuration management commands
//!
//! ploy config show     - Show current configuration
//! ploy config edit     - Edit configuration file
//! ploy config validate - Validate configuration
//! ploy config init     - Initialize default configuration

use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::PathBuf;

/// Configuration-related commands
#[derive(Subcommand, Debug)]
pub enum ConfigCommands {
    /// Show current configuration
    Show {
        /// Configuration section to show
        #[arg(short, long)]
        section: Option<String>,
    },

    /// Edit configuration file
    Edit {
        /// Specific config file to edit
        config: Option<String>,
    },

    /// Validate configuration files
    Validate {
        /// Specific config file to validate
        config: Option<String>,
    },

    /// Initialize default configuration
    Init {
        /// Force overwrite existing config
        #[arg(short, long)]
        force: bool,
    },

    /// List all configuration files
    List,
}

impl ConfigCommands {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::Show { section } => show_config(section.as_deref()).await,
            Self::Edit { config } => edit_config(config.as_deref()).await,
            Self::Validate { config } => validate_config(config.as_deref()).await,
            Self::Init { force } => init_config(force).await,
            Self::List => list_configs().await,
        }
    }
}

fn get_config_dir() -> PathBuf {
    std::env::var("PLOY_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/config"))
}

async fn show_config(section: Option<&str>) -> Result<()> {
    let config_dir = get_config_dir();
    let main_config = config_dir.join("ploy.toml");

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Configuration                                                ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    if !main_config.exists() {
        println!(
            "  \x1b[33m⚠ No configuration found at {}\x1b[0m",
            main_config.display()
        );
        println!("  Run 'ploy config init' to create default configuration\n");
        return Ok(());
    }

    let content = std::fs::read_to_string(&main_config).context("Failed to read config file")?;

    if let Some(section) = section {
        // Show specific section
        println!("  Section: {}\n", section);
        let mut in_section = false;
        for line in content.lines() {
            if line.starts_with('[') {
                in_section = line.contains(section);
            }
            if in_section {
                println!("  {}", line);
            }
        }
    } else {
        // Show all
        println!("  Config file: {}\n", main_config.display());
        for line in content.lines() {
            println!("  {}", line);
        }
    }

    println!();
    Ok(())
}

async fn edit_config(config: Option<&str>) -> Result<()> {
    let config_dir = get_config_dir();
    let config_file = match config {
        Some(name) => config_dir.join("strategies").join(format!("{}.toml", name)),
        None => config_dir.join("ploy.toml"),
    };

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());

    println!("Opening {} with {}", config_file.display(), editor);

    std::process::Command::new(&editor)
        .arg(&config_file)
        .status()
        .context("Failed to open editor")?;

    Ok(())
}

async fn validate_config(config: Option<&str>) -> Result<()> {
    let config_dir = get_config_dir();

    println!("\n  Validating configuration...\n");

    let configs_to_validate: Vec<PathBuf> = if let Some(name) = config {
        vec![config_dir.join("strategies").join(format!("{}.toml", name))]
    } else {
        // Validate all configs
        let mut configs = vec![config_dir.join("ploy.toml")];

        let strategies_dir = config_dir.join("strategies");
        if strategies_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&strategies_dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().map_or(false, |e| e == "toml") {
                        configs.push(entry.path());
                    }
                }
            }
        }
        configs
    };

    let mut all_valid = true;

    for config_path in configs_to_validate {
        let name = config_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        if !config_path.exists() {
            println!("  \x1b[33m⚠ {} - not found\x1b[0m", name);
            all_valid = false;
            continue;
        }

        match std::fs::read_to_string(&config_path) {
            Ok(content) => match toml::from_str::<toml::Value>(&content) {
                Ok(_) => println!("  \x1b[32m✓ {} - valid\x1b[0m", name),
                Err(e) => {
                    println!("  \x1b[31m✗ {} - invalid: {}\x1b[0m", name, e);
                    all_valid = false;
                }
            },
            Err(e) => {
                println!("  \x1b[31m✗ {} - read error: {}\x1b[0m", name, e);
                all_valid = false;
            }
        }
    }

    println!();
    if all_valid {
        println!("  \x1b[32m✓ All configurations valid\x1b[0m\n");
    } else {
        println!("  \x1b[31m✗ Some configurations have errors\x1b[0m\n");
    }

    Ok(())
}

async fn init_config(force: bool) -> Result<()> {
    let config_dir = get_config_dir();
    let main_config = config_dir.join("ploy.toml");
    let strategies_dir = config_dir.join("strategies");

    println!("\n  Initializing configuration...\n");

    // Check if config exists
    if main_config.exists() && !force {
        println!(
            "  \x1b[33m⚠ Configuration already exists at {}\x1b[0m",
            main_config.display()
        );
        println!("  Use --force to overwrite\n");
        return Ok(());
    }

    // Create directories
    std::fs::create_dir_all(&strategies_dir).context("Failed to create config directories")?;

    // Write main config
    let main_config_content = r#"# Ploy Trading System Configuration
# Generated by ploy config init

[system]
# Log level: trace, debug, info, warn, error
log_level = "info"

# Data directory for PID files, logs, etc.
data_dir = "/opt/ploy/data"

[polymarket]
# API endpoints
rest_url = "https://clob.polymarket.com"
ws_url = "wss://ws-subscriptions-clob.polymarket.com/ws/market"

[binance]
# WebSocket endpoint for price data
ws_url = "wss://stream.binance.com:9443/ws"

[database]
# PostgreSQL connection
url = "postgres://localhost/ploy"

[risk]
# Maximum concurrent positions
max_positions = 5

# Maximum position size (shares)
max_shares = 500

# Daily loss limit (USD)
daily_loss_limit = 100.0
"#;

    std::fs::write(&main_config, main_config_content).context("Failed to write main config")?;
    println!("  \x1b[32m✓ Created {}\x1b[0m", main_config.display());

    // Write example strategy config
    let momentum_config = strategies_dir.join("momentum.toml");
    let momentum_content = r#"# Momentum Strategy Configuration

[strategy]
name = "momentum"
enabled = true
mode = "predictive"  # "predictive" or "confirmatory"

[entry]
# Symbols to trade
symbols = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT"]

# Minimum CEX price move to trigger entry (%)
min_move = 0.5

# Maximum entry price (cents)
max_entry = 45

# Minimum edge required (%)
min_edge = 5

[exit]
# Binary options: minimum modeled edge floor (%) before forced exit
exit_edge_floor_pct = 20

# Binary options: adverse price-band threshold (%) for risk exit
exit_price_band_pct = 12

# Hold to resolution (confirmatory mode only)
hold_to_resolution = false

[timing]
# Entry window before resolution (seconds)
min_time_remaining = 300  # 5 minutes
max_time_remaining = 900  # 15 minutes

[risk]
# Shares per trade
shares = 100

# Maximum concurrent positions
max_positions = 5
"#;

    std::fs::write(&momentum_config, momentum_content)
        .context("Failed to write momentum config")?;
    println!("  \x1b[32m✓ Created {}\x1b[0m", momentum_config.display());

    println!("\n  Configuration initialized successfully!");
    println!("  Edit configs with: ploy config edit\n");

    Ok(())
}

async fn list_configs() -> Result<()> {
    let config_dir = get_config_dir();

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Configuration Files                                          ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    println!("  Config directory: {}\n", config_dir.display());

    // Main config
    let main_config = config_dir.join("ploy.toml");
    if main_config.exists() {
        println!("  \x1b[32m●\x1b[0m ploy.toml (main configuration)");
    } else {
        println!("  \x1b[33m○\x1b[0m ploy.toml (not found)");
    }

    // Strategy configs
    let strategies_dir = config_dir.join("strategies");
    println!("\n  Strategy configurations:");

    if strategies_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&strategies_dir) {
            let mut found = false;
            for entry in entries.flatten() {
                if entry.path().extension().map_or(false, |e| e == "toml") {
                    let name = entry.file_name().to_string_lossy().to_string();
                    println!("    \x1b[32m●\x1b[0m {}", name);
                    found = true;
                }
            }
            if !found {
                println!("    \x1b[33m(no strategy configs found)\x1b[0m");
            }
        }
    } else {
        println!("    \x1b[33m(strategies directory not found)\x1b[0m");
    }

    println!();
    Ok(())
}
