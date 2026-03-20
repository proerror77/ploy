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

    match command {
        Command::SystemStatus => eprintln!("{}", system::render_system_status(&client)),
        Command::TradingStatus => {
            eprintln!("{}", trading::render_trading_state(&client))
        }
        Command::TradingInspect(deployment_id) => {
            eprintln!(
                "{}",
                trading::render_one_trading_state(&client, &deployment_id).expect("trading state")
            )
        }
        Command::DeploymentsList => eprintln!("{}", deployments::render_deployments(&client)),
        Command::DeploymentsInspect(deployment_id) => eprintln!(
            "{}",
            deployments::render_deployment(&client, &deployment_id)
                .expect("deployment exists in runtime snapshot")
        ),
        Command::DeploymentsApply(manifest_path) => eprintln!(
            "{}",
            deployments::apply_deployment_file(&client, std::path::Path::new(&manifest_path))
                .expect("apply deployment")
        ),
        Command::DeploymentsPause(deployment_id) => eprintln!(
            "{}",
            deployments::control_deployment(
                &client,
                &deployment_id,
                ploy_operator_contracts::DesiredState::Paused,
            )
            .expect("pause deployment")
        ),
        Command::DeploymentsResume(deployment_id) => eprintln!(
            "{}",
            deployments::control_deployment(
                &client,
                &deployment_id,
                ploy_operator_contracts::DesiredState::Running,
            )
            .expect("resume deployment")
        ),
        Command::DeploymentsStop(deployment_id) => eprintln!(
            "{}",
            deployments::control_deployment(
                &client,
                &deployment_id,
                ploy_operator_contracts::DesiredState::Stopped,
            )
            .expect("stop deployment")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::Command;

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
}
