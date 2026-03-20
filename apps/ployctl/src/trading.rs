use crate::client::ControlPlaneClient;

pub fn render_trading_state(client: &ControlPlaneClient) -> Result<String, String> {
    Ok(client
        .trading_state()?
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
        .join("\n"))
}

pub fn render_one_trading_state(
    client: &ControlPlaneClient,
    deployment_id: &str,
) -> Result<String, String> {
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

pub fn cancel_order(
    client: &ControlPlaneClient,
    deployment_id: &str,
    order_id: &str,
) -> Result<String, String> {
    client
        .cancel_order(deployment_id, order_id)
        .map(|response| {
            format!(
                "{} order={} state={} filled_qty={} venue_order_id={}",
                response.deployment_id,
                response.order_id,
                response.state,
                response.filled_qty,
                response.venue_order_id.unwrap_or_else(|| "-".to_string()),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{cancel_order, render_one_trading_state, render_trading_state};
    use crate::client::ControlPlaneClient;
    use ploy_operator_contracts::{OrderControlResponse, TradingStateSnapshot};
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
        let output = render_trading_state(&client).expect("trading state");
        assert!(output.contains("example.paper"));
        assert!(output.contains("net_pnl=0"));
        assert!(render_one_trading_state(&client, "example.paper").is_ok());
    }

    #[test]
    fn renders_order_cancel_response() {
        let runtime_root = temp_dir("cancel");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::to_string(&OrderControlResponse {
                deployment_id: "example.live".to_string(),
                order_id: "order-1".to_string(),
                state: "canceled".to_string(),
                venue_order_id: Some("venue-1".to_string()),
                rejection_reason: None,
                last_error: None,
                filled_qty: Default::default(),
            })
            .expect("serialize response");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let client = ControlPlaneClient {
            control_plane_addr: addr.to_string(),
            runtime_root,
        };
        let output = cancel_order(&client, "example.live", "order-1").expect("cancel output");
        assert!(output.contains("state=canceled"));
        assert!(output.contains("venue_order_id=venue-1"));
    }
}
