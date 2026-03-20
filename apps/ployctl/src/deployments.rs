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

#[cfg(test)]
mod tests {
    use super::render_deployments;
    use crate::client::ControlPlaneClient;

    #[test]
    fn list_deployments() {
        let output = render_deployments(&ControlPlaneClient);
        assert!(output.contains("example.paper"));
    }
}
