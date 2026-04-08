use crate::client::ControlPlaneClient;
use ploy_operator_contracts::{ProposalDecisionRequest, SafetyProposal};

pub fn render_proposals(client: &ControlPlaneClient) -> Result<String, String> {
    Ok(client
        .list_proposals()?
        .into_iter()
        .map(render_proposal_line)
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn approve_proposal(
    client: &ControlPlaneClient,
    proposal_id: &str,
    decision_note: Option<String>,
) -> Result<String, String> {
    client
        .approve_proposal(proposal_id, &ProposalDecisionRequest { decision_note })
        .map(render_proposal_line)
}

pub fn reject_proposal(
    client: &ControlPlaneClient,
    proposal_id: &str,
    decision_note: Option<String>,
) -> Result<String, String> {
    client
        .reject_proposal(proposal_id, &ProposalDecisionRequest { decision_note })
        .map(render_proposal_line)
}

fn render_proposal_line(proposal: SafetyProposal) -> String {
    format!(
        "{} action={} target={} status={} created_at={} source_run_id={} proposed_limit={} note={} rationale={}",
        proposal.proposal_id,
        proposal_action_kind_wire(proposal.action_kind),
        proposal.target_deployment_id,
        proposal_status_wire(proposal.status),
        proposal.created_at.to_rfc3339(),
        proposal.source_run_id.unwrap_or_else(|| "-".to_string()),
        proposal
            .proposed_max_gross_exposure
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        proposal.decision_note.unwrap_or_else(|| "-".to_string()),
        proposal.rationale,
    )
}

fn proposal_action_kind_wire(
    action_kind: ploy_operator_contracts::ProposalActionKind,
) -> &'static str {
    match action_kind {
        ploy_operator_contracts::ProposalActionKind::PauseDeployment => "pause_deployment",
        ploy_operator_contracts::ProposalActionKind::DrainDeployment => "drain_deployment",
        ploy_operator_contracts::ProposalActionKind::ReduceMaxExposure => {
            "reduce_max_exposure"
        }
    }
}

fn proposal_status_wire(status: ploy_operator_contracts::ProposalStatus) -> &'static str {
    match status {
        ploy_operator_contracts::ProposalStatus::Pending => "pending",
        ploy_operator_contracts::ProposalStatus::Approved => "approved",
        ploy_operator_contracts::ProposalStatus::Rejected => "rejected",
        ploy_operator_contracts::ProposalStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::render_proposals;
    use crate::client::ControlPlaneClient;
    use chrono::Utc;
    use ploy_operator_contracts::{ProposalActionKind, ProposalStatus, SafetyProposal};
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
        std::env::temp_dir().join(format!("ployctl-proposals-{label}-{unique}"))
    }

    #[test]
    fn renders_proposals_from_http() {
        let runtime_root = temp_dir("list");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::to_string(&vec![SafetyProposal {
                proposal_id: "proposal-1".to_string(),
                action_kind: ProposalActionKind::PauseDeployment,
                target_deployment_id: "example.paper".to_string(),
                status: ProposalStatus::Pending,
                rationale: "pnl regression crossed threshold".to_string(),
                evidence: vec!["net_pnl=-2.50".to_string()],
                source_run_id: Some("run-1".to_string()),
                proposed_max_gross_exposure: None,
                created_at: Utc::now(),
                decided_at: None,
                decision_note: None,
            }])
            .expect("serialize");
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
            operator_token: None,
            sidecar_token: None,
            runtime_root,
        };

        let output = render_proposals(&client).expect("render proposals");
        assert!(output.contains("proposal-1"));
        assert!(output.contains("pause_deployment"));
    }
}
