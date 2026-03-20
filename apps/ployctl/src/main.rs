use ployctl::{client::ControlPlaneClient, deployments, system, trading};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    SystemStatus,
    TradingStatus,
    TradingInspect(String),
    DeploymentsList,
    DeploymentsInspect(String),
    DeploymentsApply(String),
    DeploymentsPause(String),
    DeploymentsResume(String),
    DeploymentsStop(String),
}

impl Command {
    fn parse(args: &[String]) -> Result<Self, String> {
        match args {
            [_bin, system, status] if system == "system" && status == "status" => {
                Ok(Self::SystemStatus)
            }
            [_bin, trading, status] if trading == "trading" && status == "status" => {
                Ok(Self::TradingStatus)
            }
            [_bin, trading, inspect, deployment_id]
                if trading == "trading" && inspect == "inspect" =>
            {
                Ok(Self::TradingInspect(deployment_id.clone()))
            }
            [_bin, deployments, list] if deployments == "deployments" && list == "list" => {
                Ok(Self::DeploymentsList)
            }
            [_bin, deployments, inspect, deployment_id]
                if deployments == "deployments" && inspect == "inspect" =>
            {
                Ok(Self::DeploymentsInspect(deployment_id.clone()))
            }
            [_bin, deployments, apply, manifest_path]
                if deployments == "deployments" && apply == "apply" =>
            {
                Ok(Self::DeploymentsApply(manifest_path.clone()))
            }
            [_bin, deployments, pause, deployment_id]
                if deployments == "deployments" && pause == "pause" =>
            {
                Ok(Self::DeploymentsPause(deployment_id.clone()))
            }
            [_bin, deployments, resume, deployment_id]
                if deployments == "deployments" && resume == "resume" =>
            {
                Ok(Self::DeploymentsResume(deployment_id.clone()))
            }
            [_bin, deployments, stop, deployment_id]
                if deployments == "deployments" && stop == "stop" =>
            {
                Ok(Self::DeploymentsStop(deployment_id.clone()))
            }
            _ => Err("usage: ployctl system status | ployctl trading status | ployctl trading inspect <deployment-id> | ployctl deployments list | ployctl deployments inspect <deployment-id> | ployctl deployments apply <manifest.json> | ployctl deployments pause <deployment-id> | ployctl deployments resume <deployment-id> | ployctl deployments stop <deployment-id>".to_string()),
        }
    }
}

fn main() {
    let command =
        Command::parse(&std::env::args().collect::<Vec<_>>()).expect("valid ployctl command");
    let client = ControlPlaneClient::default();

    match execute(command, &client) {
        Ok(output) => eprintln!("{output}"),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

fn execute(command: Command, client: &ControlPlaneClient) -> Result<String, String> {
    match command {
        Command::SystemStatus => system::render_system_status(client),
        Command::TradingStatus => trading::render_trading_state(client),
        Command::TradingInspect(deployment_id) => {
            trading::render_one_trading_state(client, &deployment_id)
        }
        Command::DeploymentsList => Ok(deployments::render_deployments(client)),
        Command::DeploymentsInspect(deployment_id) => {
            deployments::render_deployment(client, &deployment_id)
        }
        Command::DeploymentsApply(manifest_path) => {
            deployments::apply_deployment_file(client, std::path::Path::new(&manifest_path))
        }
        Command::DeploymentsPause(deployment_id) => deployments::control_deployment(
            client,
            &deployment_id,
            ploy_operator_contracts::DesiredState::Paused,
        ),
        Command::DeploymentsResume(deployment_id) => deployments::control_deployment(
            client,
            &deployment_id,
            ploy_operator_contracts::DesiredState::Running,
        ),
        Command::DeploymentsStop(deployment_id) => deployments::control_deployment(
            client,
            &deployment_id,
            ploy_operator_contracts::DesiredState::Stopped,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{execute, Command};
    use ployctl::client::ControlPlaneClient;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployctl-main-{label}-{unique}"))
    }

    #[test]
    fn parses_system_status_command() {
        let command =
            Command::parse(&["ployctl", "system", "status"].map(str::to_string)).expect("command");
        assert_eq!(command, Command::SystemStatus);
    }

    #[test]
    fn parses_trading_status_command() {
        let command =
            Command::parse(&["ployctl", "trading", "status"].map(str::to_string)).expect("command");
        assert_eq!(command, Command::TradingStatus);
    }

    #[test]
    fn parses_trading_inspect_command() {
        let command =
            Command::parse(&["ployctl", "trading", "inspect", "example.paper"].map(str::to_string))
                .expect("command");
        assert_eq!(
            command,
            Command::TradingInspect("example.paper".to_string())
        );
    }

    #[test]
    fn parses_deployment_inspect_command() {
        let command = Command::parse(
            &["ployctl", "deployments", "inspect", "example.paper"].map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            command,
            Command::DeploymentsInspect("example.paper".to_string())
        );
    }

    #[test]
    fn parses_deployment_apply_command() {
        let command = Command::parse(
            &["ployctl", "deployments", "apply", "example.paper.json"].map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            command,
            Command::DeploymentsApply("example.paper.json".to_string())
        );
    }

    #[test]
    fn parses_deployment_pause_command() {
        let command = Command::parse(
            &["ployctl", "deployments", "pause", "example.paper"].map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            command,
            Command::DeploymentsPause("example.paper".to_string())
        );
    }

    #[test]
    fn execute_returns_error_for_missing_deployment_instead_of_panicking() {
        let runtime_root = temp_dir("missing-deployment");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(runtime_root.join("deployments.json"), "[]").expect("write deployments");
        let client = ControlPlaneClient::from_runtime_root(&runtime_root);

        let error = execute(
            Command::DeploymentsInspect("missing.paper".to_string()),
            &client,
        )
        .expect_err("missing deployment should error");
        assert!(error.contains("missing.paper"));
    }
}
