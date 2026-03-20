mod client;
mod deployments;
mod system;

use client::ControlPlaneClient;

fn main() {
    let client = ControlPlaneClient;
    eprintln!("{}", system::render_system_status(&client));
    eprintln!("{}", deployments::render_deployments(&client));
}
