use ploy_operator_contracts::SystemStatus;

pub fn render_status(status: &SystemStatus) -> String {
    format!(
        "status={} uptime={}s version={}",
        status.status, status.uptime_seconds, status.version
    )
}
