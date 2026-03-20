use crate::client::ControlPlaneClient;

pub fn render_trading_state(client: &ControlPlaneClient) -> String {
    client
        .trading_state()
        .unwrap_or_default()
        .into_iter()
        .map(|state| {
            format!(
                "{} runtime={} intents={} orders={} fills={} positions={} active_orders={} gross_exposure={} net_pnl={}",
                state.deployment_id,
                state.runtime_mode,
                state.intents.len(),
                state.orders.len(),
                state.fills.len(),
                state.positions.len(),
                state.risk.active_orders,
                state.risk.gross_exposure,
                state.pnl.net_pnl,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_one_trading_state(
    client: &ControlPlaneClient,
    deployment_id: &str,
) -> Option<String> {
    client.inspect_trading_state(deployment_id).map(|state| {
        format!(
            "{} runtime={} intents={} orders={} fills={} positions={} active_orders={} gross_exposure={} net_pnl={}",
            state.deployment_id,
            state.runtime_mode,
            state.intents.len(),
            state.orders.len(),
            state.fills.len(),
            state.positions.len(),
            state.risk.active_orders,
            state.risk.gross_exposure,
            state.pnl.net_pnl,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{render_one_trading_state, render_trading_state};
    use crate::client::ControlPlaneClient;
    use ploy_operator_contracts::TradingStateSnapshot;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployctl-trading-{label}-{unique}"))
    }

    #[test]
    fn renders_snapshot_backed_trading_state() {
        let runtime_root = temp_dir("status");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("trading-state.json"),
            serde_json::to_string(&vec![TradingStateSnapshot {
                deployment_id: "example.paper".to_string(),
                runtime_mode: "paper".to_string(),
                ..TradingStateSnapshot::default()
            }])
            .expect("trading json"),
        )
        .expect("write trading state");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_trading_state(&client);
        assert!(output.contains("example.paper"));
        assert!(output.contains("net_pnl=0"));
        assert!(render_one_trading_state(&client, "example.paper").is_some());
    }
}
