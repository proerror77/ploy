use crate::client::ControlPlaneClient;

pub fn render_deployments(client: &ControlPlaneClient) -> String {
    client
        .list_deployments()
        .into_iter()
        .map(|deployment| {
            format!(
                "{} desired={:?} observed={:?}",
                deployment.deployment_id, deployment.desired_state, deployment.observed_state
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_deployment(client: &ControlPlaneClient, deployment_id: &str) -> Option<String> {
    client.inspect_deployment(deployment_id).map(|deployment| {
        format!(
            "{} desired={:?} observed={:?}",
            deployment.deployment_id, deployment.desired_state, deployment.observed_state
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{render_deployment, render_deployments};
    use crate::client::ControlPlaneClient;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployctl-deployments-{label}-{unique}"))
    }

    #[test]
    fn list_deployments() {
        let runtime_root = temp_dir("list");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("deployments.json"),
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("write deployments");

        let output = render_deployments(&ControlPlaneClient::from_runtime_root(&runtime_root));
        assert!(output.contains("example.paper"));
    }

    #[test]
    fn inspect_one_deployment() {
        let runtime_root = temp_dir("inspect");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("deployments.json"),
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("write deployments");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_deployment(&client, "example.paper").expect("deployment");
        assert!(output.contains("example.paper"));
    }
}
