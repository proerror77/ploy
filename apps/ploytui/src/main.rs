use ployctl::client::ControlPlaneClient;
use ploytui::{render_dashboard, DashboardSnapshot};
use std::thread;
use std::time::Duration;

fn parse_watch_flag(args: &[String]) -> bool {
    args.iter().skip(1).any(|arg| arg == "--watch")
}

fn build_snapshot(client: &ControlPlaneClient) -> DashboardSnapshot {
    DashboardSnapshot {
        system: client.system_snapshot().unwrap_or_else(|_| {
            ploy_operator_contracts::SystemStatus {
                status: "unavailable".to_string(),
                uptime_seconds: 0,
                version: "unknown".to_string(),
                strategy: "platform".to_string(),
                last_trade_time: None,
                websocket_connected: false,
                database_connected: false,
                error_count_1h: 0,
                live_reconcile_failures: 0,
                next_live_reconcile_at: None,
                last_live_reconcile_error: None,
            }
        }),
        deployments: client.list_deployments(),
        trading: client.trading_state().unwrap_or_default(),
        recent_events: client.recent_events(6).unwrap_or_default(),
    }
}

fn main() {
    let watch = parse_watch_flag(&std::env::args().collect::<Vec<_>>());
    let client = ControlPlaneClient::default();

    loop {
        let snapshot = build_snapshot(&client);
        print!("\x1B[2J\x1B[H{}", render_dashboard(&snapshot));
        if !watch {
            break;
        }
        thread::sleep(Duration::from_secs(2));
    }
}
