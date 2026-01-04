//! Strategy management commands
//!
//! ploy strategy list              - List all strategies and their status
//! ploy strategy start <name>      - Start a strategy
//! ploy strategy stop <name>       - Stop a strategy
//! ploy strategy status [name]     - Show strategy status
//! ploy strategy logs <name>       - View strategy logs
//! ploy strategy reload <name>     - Reload strategy config

use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::fs;
use tracing::info;

use crate::strategy::{StrategyFactory, StrategyManager};

/// Strategy-related commands
#[derive(Subcommand, Debug, Clone)]
pub enum StrategyCommands {
    /// List all available strategies
    List,

    /// Start a strategy
    Start {
        /// Strategy name (momentum, split_arb, sports)
        name: String,

        /// Config file path (optional, uses default if not specified)
        #[arg(short, long)]
        config: Option<PathBuf>,

        /// Run in dry-run mode (no real orders)
        #[arg(long)]
        dry_run: bool,

        /// Run in foreground (don't daemonize)
        #[arg(long)]
        foreground: bool,
    },

    /// Stop a running strategy
    Stop {
        /// Strategy name
        name: String,

        /// Force stop (SIGKILL instead of SIGTERM)
        #[arg(long)]
        force: bool,
    },

    /// Show status of strategies
    Status {
        /// Specific strategy name (optional, shows all if not specified)
        name: Option<String>,
    },

    /// View strategy logs
    Logs {
        /// Strategy name
        name: String,

        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "50")]
        tail: usize,

        /// Follow log output
        #[arg(short, long)]
        follow: bool,
    },

    /// Reload strategy configuration
    Reload {
        /// Strategy name
        name: String,
    },
}

impl StrategyCommands {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::List => list_strategies().await,
            Self::Start { name, config, dry_run, foreground } => {
                start_strategy(&name, config, dry_run, foreground).await
            }
            Self::Stop { name, force } => stop_strategy(&name, force).await,
            Self::Status { name } => show_status(name.as_deref()).await,
            Self::Logs { name, tail, follow } => show_logs(&name, tail, follow).await,
            Self::Reload { name } => reload_strategy(&name).await,
        }
    }
}

/// Get the config directory path
fn config_dir() -> PathBuf {
    std::env::var("PLOY_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/config"))
}

/// Get the run directory for PID files
fn run_dir() -> PathBuf {
    std::env::var("PLOY_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/run"))
}

/// Get the log directory
fn log_dir() -> PathBuf {
    std::env::var("PLOY_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/logs"))
}

/// List all available strategies
async fn list_strategies() -> Result<()> {
    let strategies_dir = config_dir().join("strategies");

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Available Strategies                                         ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    // Get strategies from factory
    let available = StrategyFactory::available_strategies();

    println!("  {:<15} {:<12} {}", "NAME", "STATUS", "DESCRIPTION");
    println!("  {}", "-".repeat(55));

    for strategy_info in &available {
        let status = get_strategy_status(&strategy_info.name);
        let status_str = match status {
            StrategyStatus::Running(_) => "\x1b[32m● running\x1b[0m",
            StrategyStatus::Stopped => "\x1b[90m○ stopped\x1b[0m",
            StrategyStatus::Error(_) => "\x1b[31m✗ error\x1b[0m",
        };
        println!("  {:<15} {:<20} {}", strategy_info.name, status_str, strategy_info.description);
    }

    // Check for custom strategy configs
    if strategies_dir.exists() {
        println!("\n  Custom Configs:");
        println!("  {}", "-".repeat(55));

        if let Ok(entries) = fs::read_dir(&strategies_dir) {
            let mut found = false;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "toml").unwrap_or(false) {
                    let name = path.file_stem().unwrap().to_string_lossy();
                    // Skip default configs
                    if !name.ends_with("_default") {
                        println!("  {:<15} (config: {})", name, path.display());
                        found = true;
                    }
                }
            }
            if !found {
                println!("  \x1b[90m(no custom configs found)\x1b[0m");
            }
        }
    }

    println!("\n  Commands:");
    println!("  {}", "-".repeat(55));
    println!("  ploy strategy start <name>     Start a strategy");
    println!("  ploy strategy stop <name>      Stop a running strategy");
    println!("  ploy strategy status           Show all strategy status");
    println!("  ploy strategy logs <name>      View strategy logs\n");

    Ok(())
}

/// Start a strategy
async fn start_strategy(
    name: &str,
    config: Option<PathBuf>,
    dry_run: bool,
    foreground: bool,
) -> Result<()> {
    info!("Starting strategy: {}", name);

    // Check if already running
    if let StrategyStatus::Running(pid) = get_strategy_status(name) {
        println!("\x1b[33m⚠ Strategy '{}' is already running (PID: {})\x1b[0m", name, pid);
        println!("  Use 'ploy strategy stop {}' first", name);
        return Ok(());
    }

    // Find config file
    let config_path = config.unwrap_or_else(|| {
        config_dir().join("strategies").join(format!("{}.toml", name))
    });

    if !config_path.exists() {
        // Try to use default config
        let default_config = config_dir().join("strategies").join(format!("{}_default.toml", name));
        if !default_config.exists() {
            println!("\x1b[33m⚠ No config found for '{}'.\x1b[0m", name);
            println!("  Creating default config at: {}", config_path.display());
            create_default_config(name, &config_path)?;
        }
    }

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Starting Strategy: {:<40}║\x1b[0m", name);
    println!("\x1b[36m╠══════════════════════════════════════════════════════════════╣\x1b[0m");
    println!("\x1b[36m║\x1b[0m  Config: {:<51}\x1b[36m║\x1b[0m", config_path.display());
    println!("\x1b[36m║\x1b[0m  Dry Run: {:<50}\x1b[36m║\x1b[0m", if dry_run { "YES" } else { "NO" });
    println!("\x1b[36m║\x1b[0m  Mode: {:<53}\x1b[36m║\x1b[0m", if foreground { "foreground" } else { "daemon" });
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    if foreground {
        // Run in foreground - exec directly
        run_strategy_foreground(name, &config_path, dry_run).await
    } else {
        // Run as daemon
        run_strategy_daemon(name, &config_path, dry_run).await
    }
}

/// Run strategy in foreground using StrategyManager
async fn run_strategy_foreground(name: &str, config_path: &PathBuf, dry_run: bool) -> Result<()> {

    // Load config
    let config_content = fs::read_to_string(config_path)
        .context(format!("Failed to read config: {}", config_path.display()))?;

    println!("\x1b[32m▶ Running {} in foreground (Ctrl+C to stop)\x1b[0m\n", name);

    // Create strategy via factory
    let strategy = StrategyFactory::from_toml(&config_content, dry_run)
        .context("Failed to create strategy from config")?;

    let strategy_id = strategy.id().to_string();

    println!("  Strategy ID: {}", strategy_id);
    println!("  Strategy: {}", strategy.name());
    println!("  Description: {}", strategy.description());
    println!("  Dry Run: {}", dry_run);
    println!();

    // Create strategy manager
    let manager = StrategyManager::new(1000); // 1 second tick interval

    // Take the action receiver before starting strategy
    let action_rx = manager.take_action_receiver().await
        .expect("Action receiver should be available");

    // Start the strategy
    manager.start_strategy(strategy, Some(config_path.display().to_string())).await
        .context("Failed to start strategy")?;

    println!("\x1b[32m✓ Strategy started successfully\x1b[0m\n");

    // Spawn action handler task
    let action_handle = tokio::spawn(handle_strategy_actions(action_rx));

    // Wait for shutdown signal
    println!("Press Ctrl+C to stop...\n");
    tokio::signal::ctrl_c().await?;

    println!("\n\x1b[33m⚠ Shutdown signal received\x1b[0m");

    // Graceful shutdown
    println!("Stopping strategy gracefully...");
    manager.stop_strategy(&strategy_id, true).await
        .context("Failed to stop strategy")?;

    // Cancel action handler
    action_handle.abort();

    println!("\x1b[32m✓ Strategy stopped\x1b[0m");

    Ok(())
}

/// Handle actions emitted by strategies
async fn handle_strategy_actions(mut rx: tokio::sync::mpsc::Receiver<(String, crate::strategy::StrategyAction)>) {
    use crate::strategy::StrategyAction;

    while let Some((strategy_id, action)) = rx.recv().await {
        match action {
            StrategyAction::SubmitOrder { client_order_id, order, priority } => {
                println!("  \x1b[36m[{}]\x1b[0m Order: {} (priority: {})",
                    strategy_id, client_order_id, priority);
                println!("         Token: {}", order.token_id);
                println!("         Side: {:?}, Shares: {}, Price: {:.2}¢",
                    order.market_side, order.shares, order.limit_price * rust_decimal::Decimal::from(100));
                // In production, this would submit to the order executor
            }
            StrategyAction::CancelOrder { order_id } => {
                println!("  \x1b[33m[{}]\x1b[0m Cancel: {}",
                    strategy_id, order_id);
            }
            StrategyAction::ModifyOrder { order_id, new_price, new_size } => {
                println!("  \x1b[33m[{}]\x1b[0m Modify: {} price={:?} size={:?}",
                    strategy_id, order_id, new_price, new_size);
            }
            StrategyAction::Alert { level, message } => {
                let color = match level {
                    crate::strategy::AlertLevel::Info => "\x1b[36m",
                    crate::strategy::AlertLevel::Warning => "\x1b[33m",
                    crate::strategy::AlertLevel::Error => "\x1b[31m",
                    crate::strategy::AlertLevel::Critical => "\x1b[31;1m",
                };
                println!("  {}[{}] {:?}: {}\x1b[0m", color, strategy_id, level, message);
            }
            StrategyAction::LogEvent { event } => {
                println!("  \x1b[90m[{}] {:?}: {}\x1b[0m",
                    strategy_id, event.event_type, event.message);
            }
            StrategyAction::UpdateRisk { level, reason } => {
                println!("  \x1b[35m[{}]\x1b[0m Risk: {:?} - {}",
                    strategy_id, level, reason);
            }
            StrategyAction::SubscribeFeed { feed } => {
                println!("  \x1b[90m[{}]\x1b[0m Subscribe: {:?}",
                    strategy_id, feed);
            }
            StrategyAction::UnsubscribeFeed { feed } => {
                println!("  \x1b[90m[{}]\x1b[0m Unsubscribe: {:?}",
                    strategy_id, feed);
            }
        }
    }
}

/// Run strategy as daemon
async fn run_strategy_daemon(name: &str, config_path: &PathBuf, dry_run: bool) -> Result<()> {
    // Ensure run directory exists
    let run_dir = run_dir();
    fs::create_dir_all(&run_dir)?;

    let pid_file = run_dir.join(format!("{}.pid", name));
    let log_file = log_dir().join(format!("{}.log", name));

    // Build command
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("strategy")
        .arg("start")
        .arg(name)
        .arg("--config")
        .arg(config_path)
        .arg("--foreground");

    if dry_run {
        cmd.arg("--dry-run");
    }

    // Redirect output to log file
    fs::create_dir_all(log_dir())?;
    let log = fs::File::create(&log_file)?;
    let log_err = log.try_clone()?;

    cmd.stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .stdin(Stdio::null());

    // Spawn daemon
    let child = cmd.spawn()
        .context("Failed to spawn strategy process")?;

    // Write PID file
    fs::write(&pid_file, child.id().to_string())?;

    println!("\x1b[32m✓ Strategy '{}' started (PID: {})\x1b[0m", name, child.id());
    println!("  Log file: {}", log_file.display());
    println!("  PID file: {}", pid_file.display());
    println!("\n  Use 'ploy strategy logs {} -f' to follow logs", name);

    Ok(())
}

/// Stop a running strategy
async fn stop_strategy(name: &str, force: bool) -> Result<()> {
    let pid_file = run_dir().join(format!("{}.pid", name));

    if !pid_file.exists() {
        println!("\x1b[33m⚠ Strategy '{}' is not running\x1b[0m", name);
        return Ok(());
    }

    let pid: u32 = fs::read_to_string(&pid_file)?
        .trim()
        .parse()
        .context("Invalid PID file")?;

    let signal = if force { "SIGKILL" } else { "SIGTERM" };

    println!("Stopping strategy '{}' (PID: {}) with {}...", name, pid, signal);

    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        let sig = if force { Signal::SIGKILL } else { Signal::SIGTERM };
        match kill(Pid::from_raw(pid as i32), sig) {
            Ok(_) => {
                // Remove PID file
                let _ = fs::remove_file(&pid_file);
                println!("\x1b[32m✓ Strategy '{}' stopped\x1b[0m", name);
            }
            Err(e) => {
                println!("\x1b[31m✗ Failed to stop: {}\x1b[0m", e);
                // Clean up stale PID file
                let _ = fs::remove_file(&pid_file);
            }
        }
    }

    #[cfg(not(unix))]
    {
        println!("\x1b[33m⚠ Signal handling not supported on this platform\x1b[0m");
        println!("  Manually kill process with PID: {}", pid);
    }

    Ok(())
}

/// Show strategy status
async fn show_status(name: Option<&str>) -> Result<()> {
    println!("\n{}", "=".repeat(60));
    println!("  STRATEGY STATUS");
    println!("{}\n", "=".repeat(60));

    let strategies = if let Some(n) = name {
        vec![n.to_string()]
    } else {
        vec!["momentum".into(), "split_arb".into(), "sports".into(), "politics".into()]
    };

    println!("  {:<15} {:<12} {:<10} {}", "NAME", "STATUS", "PID", "UPTIME");
    println!("  {}", "-".repeat(55));

    for strat_name in strategies {
        let status = get_strategy_status(&strat_name);
        match status {
            StrategyStatus::Running(pid) => {
                let uptime = get_process_uptime(pid).unwrap_or_else(|| "unknown".into());
                println!(
                    "  {:<15} \x1b[32m{:<12}\x1b[0m {:<10} {}",
                    strat_name, "● running", pid, uptime
                );
            }
            StrategyStatus::Stopped => {
                println!(
                    "  {:<15} \x1b[90m{:<12}\x1b[0m {:<10} {}",
                    strat_name, "○ stopped", "-", "-"
                );
            }
            StrategyStatus::Error(e) => {
                println!(
                    "  {:<15} \x1b[31m{:<12}\x1b[0m {:<10} {}",
                    strat_name, "✗ error", "-", e
                );
            }
        }
    }

    println!("\n{}", "=".repeat(60));

    Ok(())
}

/// Show strategy logs
async fn show_logs(name: &str, tail: usize, follow: bool) -> Result<()> {
    let log_file = log_dir().join(format!("{}.log", name));

    if !log_file.exists() {
        println!("\x1b[33m⚠ No log file found for '{}'\x1b[0m", name);
        println!("  Expected: {}", log_file.display());
        return Ok(());
    }

    if follow {
        // Use tail -f
        let mut child = Command::new("tail")
            .arg("-f")
            .arg("-n")
            .arg(tail.to_string())
            .arg(&log_file)
            .spawn()
            .context("Failed to run tail")?;

        child.wait()?;
    } else {
        // Just show last N lines
        let output = Command::new("tail")
            .arg("-n")
            .arg(tail.to_string())
            .arg(&log_file)
            .output()
            .context("Failed to run tail")?;

        print!("{}", String::from_utf8_lossy(&output.stdout));
    }

    Ok(())
}

/// Reload strategy configuration
async fn reload_strategy(name: &str) -> Result<()> {
    let pid_file = run_dir().join(format!("{}.pid", name));

    if !pid_file.exists() {
        println!("\x1b[33m⚠ Strategy '{}' is not running\x1b[0m", name);
        return Ok(());
    }

    let pid: u32 = fs::read_to_string(&pid_file)?
        .trim()
        .parse()
        .context("Invalid PID file")?;

    println!("Reloading config for strategy '{}' (PID: {})...", name, pid);

    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        match kill(Pid::from_raw(pid as i32), Signal::SIGHUP) {
            Ok(_) => {
                println!("\x1b[32m✓ Reload signal sent\x1b[0m");
            }
            Err(e) => {
                println!("\x1b[31m✗ Failed to send reload signal: {}\x1b[0m", e);
            }
        }
    }

    Ok(())
}

// === Helper Types and Functions ===

#[derive(Debug)]
enum StrategyStatus {
    Running(u32),
    Stopped,
    Error(String),
}

fn get_strategy_status(name: &str) -> StrategyStatus {
    let pid_file = run_dir().join(format!("{}.pid", name));

    if !pid_file.exists() {
        return StrategyStatus::Stopped;
    }

    match fs::read_to_string(&pid_file) {
        Ok(content) => {
            match content.trim().parse::<u32>() {
                Ok(pid) => {
                    // Check if process is actually running
                    if is_process_running(pid) {
                        StrategyStatus::Running(pid)
                    } else {
                        // Stale PID file
                        let _ = fs::remove_file(&pid_file);
                        StrategyStatus::Stopped
                    }
                }
                Err(_) => StrategyStatus::Error("Invalid PID file".into()),
            }
        }
        Err(e) => StrategyStatus::Error(e.to_string()),
    }
}

fn is_process_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        kill(Pid::from_raw(pid as i32), Signal::SIGCONT).is_ok()
    }
    #[cfg(not(unix))]
    {
        // On non-Unix, just assume it's running if PID file exists
        true
    }
}

fn get_process_uptime(_pid: u32) -> Option<String> {
    // TODO: Implement actual uptime calculation
    Some("--".into())
}

fn create_default_config(name: &str, path: &PathBuf) -> Result<()> {
    let config = match name {
        "momentum" => include_str!("../../config/strategies/momentum_default.toml"),
        "split_arb" => include_str!("../../config/strategies/split_arb_default.toml"),
        _ => return Ok(()), // No default for unknown strategies
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, config)?;
    Ok(())
}

