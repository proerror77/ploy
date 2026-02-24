//! Service management commands
//!
//! ploy service start     - Start all core services
//! ploy service stop      - Stop all services
//! ploy service status    - Show service status
//! ploy service logs      - View service logs

use anyhow::Result;
use clap::Subcommand;

/// Service-related commands
#[derive(Subcommand, Debug)]
pub enum ServiceCommands {
    /// Start core services (market data, executor)
    Start {
        /// Specific service to start (optional)
        service: Option<String>,
    },

    /// Stop core services
    Stop {
        /// Specific service to stop (optional)
        service: Option<String>,
    },

    /// Show status of core services
    Status,

    /// View service logs
    Logs {
        /// Service name
        service: String,

        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "50")]
        tail: usize,

        /// Follow log output
        #[arg(short, long)]
        follow: bool,
    },
}

impl ServiceCommands {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::Start { service } => start_services(service.as_deref()).await,
            Self::Stop { service } => stop_services(service.as_deref()).await,
            Self::Status => show_status().await,
            Self::Logs {
                service,
                tail,
                follow,
            } => show_logs(&service, tail, follow).await,
        }
    }
}

async fn start_services(service: Option<&str>) -> Result<()> {
    disabled_service_command("start", service)
}

async fn stop_services(service: Option<&str>) -> Result<()> {
    disabled_service_command("stop", service)
}

async fn show_status() -> Result<()> {
    disabled_service_command("status", None)
}

async fn show_logs(service: &str, _tail: usize, _follow: bool) -> Result<()> {
    disabled_service_command("logs", Some(service))
}

fn disabled_service_command(cmd: &str, service: Option<&str>) -> Result<()> {
    let target = service
        .map(|s| format!(" `{}`", s))
        .unwrap_or_else(|| "".to_string());
    let msg = format!(
        "legacy `ploy service {cmd}{target}` is disabled because it used non-functional stubs; use `ploy platform start` for coordinator runtime and system tooling/scripts for process control"
    );
    Err(anyhow::anyhow!(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_service_command_returns_error_with_hint() {
        let err = disabled_service_command("status", None)
            .expect_err("legacy service command should be disabled");
        assert!(err.to_string().contains("ploy platform start"));
    }

    #[test]
    fn disabled_service_command_includes_target_service_when_provided() {
        let err = disabled_service_command("logs", Some("executor"))
            .expect_err("legacy service command should be disabled");
        assert!(err.to_string().contains("logs `executor`"));
    }
}
