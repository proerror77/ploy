use super::*;

mod foreground;

pub(super) async fn list_strategies() -> Result<()> {
    let strategies_dir = config_dir().join("strategies");

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Available Strategies                                         ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

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
        println!(
            "  {:<15} {:<20} {}",
            strategy_info.name, status_str, strategy_info.description
        );
    }

    if strategies_dir.exists() {
        println!("\n  Custom Configs:");
        println!("  {}", "-".repeat(55));

        if let Ok(entries) = fs::read_dir(&strategies_dir) {
            let mut found = false;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "toml").unwrap_or(false) {
                    if let Some(stem) = path.file_stem() {
                        let name = stem.to_string_lossy();
                        if !name.ends_with("_default") {
                            println!("  {:<15} (config: {})", name, path.display());
                            found = true;
                        }
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

pub(super) async fn start_strategy(
    name: &str,
    config: Option<PathBuf>,
    dry_run: bool,
    foreground: bool,
) -> Result<()> {
    info!("Starting strategy: {}", name);

    if !dry_run {
        let result = crate::safety::direct_live::enforce_live_gate("ploy strategy start");
        if let Err(ref e) = result {
            warn!("{e}");
            println!("\x1b[31m✗ {e}\x1b[0m");
        }
        result?;
    }

    let under_systemd = std::env::var_os("INVOCATION_ID").is_some()
        || std::env::var_os("SYSTEMD_EXEC_PID").is_some()
        || std::env::var_os("JOURNAL_STREAM").is_some();

    if !under_systemd {
        if let StrategyStatus::Running(pid) = get_strategy_status(name) {
            println!(
                "\x1b[33m⚠ Strategy '{}' is already running (PID: {})\x1b[0m",
                name, pid
            );
            println!("  Use 'ploy strategy stop {}' first", name);
            return Ok(());
        }
    }

    let config_path = config.unwrap_or_else(|| {
        config_dir()
            .join("strategies")
            .join(format!("{}.toml", name))
    });

    if !config_path.exists() {
        let default_config = config_dir()
            .join("strategies")
            .join(format!("{}_default.toml", name));
        if !default_config.exists() {
            println!("\x1b[33m⚠ No config found for '{}'.\x1b[0m", name);
            println!("  Creating default config at: {}", config_path.display());
            create_default_config(name, &config_path)?;
        }
    }

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Starting Strategy: {:<40}║\x1b[0m", name);
    println!("\x1b[36m╠══════════════════════════════════════════════════════════════╣\x1b[0m");
    println!(
        "\x1b[36m║\x1b[0m  Config: {:<51}\x1b[36m║\x1b[0m",
        config_path.display()
    );
    println!(
        "\x1b[36m║\x1b[0m  Dry Run: {:<50}\x1b[36m║\x1b[0m",
        if dry_run { "YES" } else { "NO" }
    );
    println!(
        "\x1b[36m║\x1b[0m  Mode: {:<53}\x1b[36m║\x1b[0m",
        if foreground { "foreground" } else { "daemon" }
    );
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    if foreground {
        run_strategy_foreground(name, &config_path, dry_run).await
    } else {
        run_strategy_daemon(name, &config_path, dry_run).await
    }
}

async fn run_strategy_foreground(name: &str, config_path: &PathBuf, dry_run: bool) -> Result<()> {
    foreground::run_strategy_foreground(name, config_path, dry_run).await
}

async fn run_strategy_daemon(name: &str, config_path: &PathBuf, dry_run: bool) -> Result<()> {
    let run_dir = run_dir();
    fs::create_dir_all(&run_dir)?;

    let pid_file = run_dir.join(format!("{}.pid", name));
    let log_file = log_dir().join(format!("{}.log", name));

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

    fs::create_dir_all(log_dir())?;
    let log = fs::File::create(&log_file)?;
    let log_err = log.try_clone()?;

    cmd.stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .stdin(Stdio::null());

    let child = cmd.spawn().context("Failed to spawn strategy process")?;
    fs::write(&pid_file, child.id().to_string())?;

    println!(
        "\x1b[32m✓ Strategy '{}' started (PID: {})\x1b[0m",
        name,
        child.id()
    );
    println!("  Log file: {}", log_file.display());
    println!("  PID file: {}", pid_file.display());
    println!("\n  Use 'ploy strategy logs {} -f' to follow logs", name);

    Ok(())
}

pub(super) async fn stop_strategy(name: &str, force: bool) -> Result<()> {
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
    println!(
        "Stopping strategy '{}' (PID: {}) with {}...",
        name, pid, signal
    );

    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        let sig = if force {
            Signal::SIGKILL
        } else {
            Signal::SIGTERM
        };
        match kill(Pid::from_raw(pid as i32), sig) {
            Ok(_) => {
                let _ = fs::remove_file(&pid_file);
                println!("\x1b[32m✓ Strategy '{}' stopped\x1b[0m", name);
            }
            Err(e) => {
                println!("\x1b[31m✗ Failed to stop: {}\x1b[0m", e);
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

pub(super) async fn show_status(name: Option<&str>) -> Result<()> {
    println!("\n{}", "=".repeat(60));
    println!("  STRATEGY STATUS");
    println!("{}\n", "=".repeat(60));

    let strategies = if let Some(name) = name {
        vec![name.to_string()]
    } else {
        vec![
            "momentum".into(),
            "split_arb".into(),
            "pattern_memory".into(),
            "sports".into(),
            "politics".into(),
        ]
    };

    println!(
        "  {:<15} {:<12} {:<10} {}",
        "NAME", "STATUS", "PID", "UPTIME"
    );
    println!("  {}", "-".repeat(55));

    for strategy_name in strategies {
        let status = get_strategy_status(&strategy_name);
        match status {
            StrategyStatus::Running(pid) => {
                let pid_str = if pid == 0 {
                    "-".to_string()
                } else {
                    pid.to_string()
                };
                let uptime = if pid == 0 {
                    "unknown".into()
                } else {
                    get_process_uptime(pid).unwrap_or_else(|| "unknown".into())
                };
                println!(
                    "  {:<15} \x1b[32m{:<12}\x1b[0m {:<10} {}",
                    strategy_name, "● running", pid_str, uptime
                );
            }
            StrategyStatus::Stopped => {
                println!(
                    "  {:<15} \x1b[90m{:<12}\x1b[0m {:<10} {}",
                    strategy_name, "○ stopped", "-", "-"
                );
            }
            StrategyStatus::Error(error) => {
                println!(
                    "  {:<15} \x1b[31m{:<12}\x1b[0m {:<10} {}",
                    strategy_name, "✗ error", "-", error
                );
            }
        }
    }

    println!("\n{}", "=".repeat(60));
    Ok(())
}

pub(super) async fn show_logs(name: &str, tail: usize, follow: bool) -> Result<()> {
    let log_file = log_dir().join(format!("{}.log", name));

    if !log_file.exists() {
        println!("\x1b[33m⚠ No log file found for '{}'\x1b[0m", name);
        println!("  Expected: {}", log_file.display());
        return Ok(());
    }

    if follow {
        let mut child = Command::new("tail")
            .arg("-f")
            .arg("-n")
            .arg(tail.to_string())
            .arg(&log_file)
            .spawn()
            .context("Failed to run tail")?;

        child.wait()?;
    } else {
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

pub(super) async fn reload_strategy(name: &str) -> Result<()> {
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

fn config_dir() -> PathBuf {
    std::env::var("PLOY_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/config"))
}

fn run_dir() -> PathBuf {
    std::env::var("PLOY_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/run"))
}

fn log_dir() -> PathBuf {
    std::env::var("PLOY_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/logs"))
}

#[derive(Debug)]
enum StrategyStatus {
    Running(u32),
    Stopped,
    Error(String),
}

fn get_strategy_status(name: &str) -> StrategyStatus {
    let pid_file = run_dir().join(format!("{}.pid", name));

    if pid_file.exists() {
        match fs::read_to_string(&pid_file) {
            Ok(content) => match content.trim().parse::<u32>() {
                Ok(pid) => {
                    if is_process_running(pid) {
                        return StrategyStatus::Running(pid);
                    }
                    let _ = fs::remove_file(&pid_file);
                }
                Err(_) => {
                    let _ = fs::remove_file(&pid_file);
                }
            },
            Err(e) => return StrategyStatus::Error(e.to_string()),
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(status) = systemd_strategy_status(name) {
            return status;
        }
    }

    StrategyStatus::Stopped
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
        true
    }
}

fn get_process_uptime(_pid: u32) -> Option<String> {
    Some("--".into())
}

#[cfg(target_os = "linux")]
fn systemd_strategy_status(name: &str) -> Option<StrategyStatus> {
    if Command::new("systemctl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        return None;
    }

    let slug = name.replace('_', "-");
    let mut candidates = vec![
        format!("ploy-strategy-{}-dryrun.service", slug),
        format!("ploy-strategy-{}.service", slug),
        format!("ploy-strategy-{}-dryrun.service", name),
        format!("ploy-strategy-{}.service", name),
    ];
    candidates.dedup();

    for unit in candidates {
        let out = Command::new("systemctl")
            .arg("is-active")
            .arg(&unit)
            .output()
            .ok()?;

        let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match state.as_str() {
            "active" | "activating" | "reloading" | "deactivating" => {
                let pid_out = Command::new("systemctl")
                    .arg("show")
                    .arg(&unit)
                    .arg("--property=MainPID")
                    .arg("--value")
                    .output()
                    .ok();

                let pid = pid_out
                    .as_ref()
                    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(0);

                return Some(StrategyStatus::Running(pid));
            }
            "failed" => {
                return Some(StrategyStatus::Error(format!(
                    "systemd unit failed: {}",
                    unit
                )));
            }
            _ => {}
        }
    }

    None
}

fn create_default_config(name: &str, path: &PathBuf) -> Result<()> {
    let config = match name {
        "momentum" => include_str!("../../../config/strategies/momentum_default.toml"),
        "split_arb" => include_str!("../../../config/strategies/split_arb_default.toml"),
        "pattern_memory" => include_str!("../../../config/strategies/pattern_memory_default.toml"),
        "weather_market" => {
            include_str!("../../../config/strategies/weather_market_default.toml")
        }
        _ => return Ok(()),
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, config)?;
    Ok(())
}
