use crate::client::ControlPlaneClient;
use ploy_operator_contracts::{DeploymentApplyRequest, DesiredState};
use std::fs;
use std::path::Path;

pub fn render_deployments(client: &ControlPlaneClient) -> String {
    client
        .deployment_summaries()
        .unwrap_or_default()
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

pub fn render_deployment(
    client: &ControlPlaneClient,
    deployment_id: &str,
) -> Result<String, String> {
    client.inspect_deployment(deployment_id).map(|deployment| {
        format!(
            "{} desired={:?} observed={:?}",
            deployment.deployment_id, deployment.desired_state, deployment.observed_state
        )
    })
}

pub fn apply_deployment_file(
    client: &ControlPlaneClient,
    manifest_path: &Path,
) -> Result<String, String> {
    let body = fs::read_to_string(manifest_path).map_err(|err| {
        format!(
            "read deployment manifest {}: {err}",
            manifest_path.display()
        )
    })?;
    let request: DeploymentApplyRequest =
        serde_json::from_str(&body).map_err(|err| format!("parse deployment manifest: {err}"))?;
    let deployment = client.apply_deployment(&request)?;
    Ok(format!(
        "{} desired={:?} observed={:?}",
        deployment.deployment_id, deployment.desired_state, deployment.observed_state
    ))
}

pub fn control_deployment(
    client: &ControlPlaneClient,
    deployment_id: &str,
    desired_state: DesiredState,
) -> Result<String, String> {
    let deployment = client.set_desired_state(deployment_id, desired_state)?;
    Ok(format!(
        "{} desired={:?} observed={:?}",
        deployment.deployment_id, deployment.desired_state, deployment.observed_state
    ))
}

#[cfg(test)]
mod tests {
    use super::{apply_deployment_file, control_deployment, render_deployment, render_deployments};
    use crate::client::ControlPlaneClient;
    use ploy_operator_contracts::DesiredState;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;
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

    #[test]
    fn apply_and_control_deployment_commands() {
        let runtime_root = temp_dir("mutate");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        let manifest = runtime_root.join("example.paper.json");
        fs::write(
            &manifest,
            serde_json::json!({
                "deployment_id": "example.paper",
                "bundle_id": "example",
                "runtime_mode": "paper",
                "desired_state": "running"
            })
            .to_string(),
        )
        .expect("write manifest");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0_u8; 2048];
                let bytes = stream.read(&mut request).expect("read request");
                let request = String::from_utf8_lossy(&request[..bytes]);
                let body = if request.starts_with("PUT /api/deployments/example.paper") {
                    serde_json::json!({
                        "deployment_id": "example.paper",
                        "desired_state": "running",
                        "observed_state": "starting"
                    })
                    .to_string()
                } else {
                    serde_json::json!({
                        "deployment_id": "example.paper",
                        "desired_state": "paused",
                        "observed_state": "paused"
                    })
                    .to_string()
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write response");
            }
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let applied = apply_deployment_file(&client, &manifest).expect("apply command");
        assert!(applied.contains("example.paper"));

        let paused =
            control_deployment(&client, "example.paper", DesiredState::Paused).expect("pause");
        assert!(paused.contains("Paused"));
    }
}
