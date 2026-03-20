use crate::client::ControlPlaneClient;

pub fn render_system_status(client: &ControlPlaneClient) -> String {
    client.system_status()
}
