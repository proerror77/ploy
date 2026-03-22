use crate::client::ControlPlaneClient;

pub fn render_claim_statuses(client: &ControlPlaneClient) -> Result<String, String> {
    Ok(client
        .claim_statuses()?
        .into_iter()
        .map(|status| {
            format!(
                "{} enabled={} mode={} loop={} pending_count={} pending_notional={} last_claim={} next_retry={} last_error={}",
                status.account_id,
                status.enabled,
                status.runtime_mode,
                format!("{:?}", status.loop_state).to_lowercase(),
                status.pending_redeemable_count,
                status.pending_redeemable_notional,
                status
                    .last_claim_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
                status
                    .next_retry_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
                status.last_error.unwrap_or_else(|| "-".to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn render_claim_detail(
    client: &ControlPlaneClient,
    account_id: &str,
) -> Result<String, String> {
    client.inspect_claims(account_id).map(|detail| {
        format!(
            "{} enabled={} loop={} pending={} history={}",
            detail.status.account_id,
            detail.status.enabled,
            format!("{:?}", detail.status.loop_state).to_lowercase(),
            detail.redeemable_positions.len(),
            detail.claim_history.len(),
        )
    })
}

pub fn run_claims(client: &ControlPlaneClient, account_id: &str) -> Result<String, String> {
    client.run_claims(account_id).map(render_action)
}

pub fn rescan_claims(client: &ControlPlaneClient, account_id: &str) -> Result<String, String> {
    client.rescan_claims(account_id).map(render_action)
}

pub fn pause_claims(client: &ControlPlaneClient, account_id: &str) -> Result<String, String> {
    client.pause_claims(account_id).map(render_action)
}

pub fn resume_claims(client: &ControlPlaneClient, account_id: &str) -> Result<String, String> {
    client.resume_claims(account_id).map(render_action)
}

fn render_action(response: ploy_operator_contracts::AccountClaimActionResponse) -> String {
    format!(
        "{} state={} message={}",
        response.account_id,
        format!("{:?}", response.state).to_lowercase(),
        response.message,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        pause_claims, render_claim_detail, render_claim_statuses, rescan_claims, resume_claims,
        run_claims,
    };
    use crate::client::ControlPlaneClient;
    use chrono::Utc;
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
        std::env::temp_dir().join(format!("ployctl-claims-{label}-{unique}"))
    }

    #[test]
    fn renders_snapshot_backed_claim_statuses() {
        let runtime_root = temp_dir("status");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("account-claims.json"),
            serde_json::json!([{
                "status": {
                    "account_id": "acct-live",
                    "enabled": true,
                    "runtime_mode": "live",
                    "loop_state": "running",
                    "last_scan_at": null,
                    "last_claim_at": null,
                    "last_error": null,
                    "consecutive_failures": 0,
                    "next_retry_at": null,
                    "pending_redeemable_count": 1,
                    "pending_redeemable_notional": "5.00"
                },
                "redeemable_positions": [],
                "claim_history": []
            }])
            .to_string(),
        )
        .expect("write claim state");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_claim_statuses(&client).expect("claim statuses");
        assert!(output.contains("acct-live"));
        assert!(output.contains("pending_notional=5.00"));
    }

    #[test]
    fn renders_claim_detail_from_http() {
        let runtime_root = temp_dir("detail");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::json!({
                "status": {
                    "account_id": "acct-live",
                    "enabled": true,
                    "runtime_mode": "live",
                    "loop_state": "running",
                    "last_scan_at": null,
                    "last_claim_at": null,
                    "last_error": null,
                    "consecutive_failures": 0,
                    "next_retry_at": null,
                    "pending_redeemable_count": 1,
                    "pending_redeemable_notional": "5.00"
                },
                "redeemable_positions": [{
                    "account_id": "acct-live",
                    "condition_id": "condition-1",
                    "market_id": "market-1",
                    "token_ids": ["1"],
                    "outcome_labels": ["YES"],
                    "redeemable_size": "5.00",
                    "estimated_payout": "5.00",
                    "detected_at": Utc::now(),
                    "claim_state": "detected"
                }],
                "claim_history": []
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();
        let output = render_claim_detail(&client, "acct-live").expect("claim detail");
        assert!(output.contains("acct-live"));
        assert!(output.contains("pending=1"));
    }

    #[test]
    fn renders_claim_actions() {
        for (path, runner) in [
            (
                "/api/accounts/acct-live/claims/run",
                run_claims as fn(&ControlPlaneClient, &str) -> Result<String, String>,
            ),
            ("/api/accounts/acct-live/claims/rescan", rescan_claims),
            ("/api/accounts/acct-live/claims/pause", pause_claims),
            ("/api/accounts/acct-live/claims/resume", resume_claims),
        ] {
            let runtime_root = temp_dir("action");
            fs::create_dir_all(&runtime_root).expect("create runtime root");

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
            let addr = listener.local_addr().expect("local addr");
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0_u8; 2048];
                let bytes = stream.read(&mut request).expect("read request");
                let request = String::from_utf8_lossy(&request[..bytes]);
                assert!(request.starts_with(&format!("POST {path}")));
                let body = serde_json::json!({
                    "account_id": "acct-live",
                    "state": "accepted",
                    "message": "ok"
                })
                .to_string();
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
                admin_token: None,
                runtime_root,
            };
            let output = runner(&client, "acct-live").expect("action output");
            assert!(output.contains("acct-live"));
            assert!(output.contains("state=accepted"));
        }
    }
}
