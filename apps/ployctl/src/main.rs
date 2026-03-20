mod client;
mod deployments;
mod system;

use client::ControlPlaneClient;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    SystemStatus,
    DeploymentsList,
    DeploymentsInspect(String),
}

impl Command {
    fn parse(args: &[String]) -> Result<Self, String> {
        match args {
            [_bin, system, status] if system == "system" && status == "status" => {
                Ok(Self::SystemStatus)
            }
            [_bin, deployments, list] if deployments == "deployments" && list == "list" => {
                Ok(Self::DeploymentsList)
            }
            [_bin, deployments, inspect, deployment_id]
                if deployments == "deployments" && inspect == "inspect" =>
            {
                Ok(Self::DeploymentsInspect(deployment_id.clone()))
            }
            _ => Err("usage: ployctl system status | ployctl deployments list | ployctl deployments inspect <deployment-id>".to_string()),
        }
    }
}

fn main() {
    let command = Command::parse(&std::env::args().collect::<Vec<_>>())
        .expect("valid ployctl command");
    let client = ControlPlaneClient::default();

    match command {
        Command::SystemStatus => eprintln!("{}", system::render_system_status(&client)),
        Command::DeploymentsList => eprintln!("{}", deployments::render_deployments(&client)),
        Command::DeploymentsInspect(deployment_id) => eprintln!(
            "{}",
            deployments::render_deployment(&client, &deployment_id)
                .expect("deployment exists in runtime snapshot")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::Command;

    #[test]
    fn parses_system_status_command() {
        let command = Command::parse(&["ployctl", "system", "status"].map(str::to_string))
            .expect("command");
        assert_eq!(command, Command::SystemStatus);
    }

    #[test]
    fn parses_deployment_inspect_command() {
        let command = Command::parse(
            &["ployctl", "deployments", "inspect", "example.paper"].map(str::to_string),
        )
        .expect("command");
        assert_eq!(command, Command::DeploymentsInspect("example.paper".to_string()));
    }
}
