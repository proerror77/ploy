use ployctl::{client::ControlPlaneClient, deployments, proposals, research, system, trading};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    SystemStatus,
    SystemMetrics,
    SystemAlerts,
    SystemAudit,
    SystemDiagnose,
    TradingStatus,
    TradingInspect(String),
    TradingDiagnose(String),
    TradingCancel(String, String),
    TradingReplace(
        String,
        String,
        rust_decimal::Decimal,
        Option<rust_decimal::Decimal>,
    ),
    DeploymentsList,
    DeploymentsInspect(String),
    DeploymentsApply(String),
    DeploymentsPause(String),
    DeploymentsResume(String),
    DeploymentsStop(String),
    DeploymentsDrain(String),
    DeploymentsEnable(String),
    DeploymentsDisable(String),
    DeploymentsArchive(String),
    ProposalsList,
    ProposalsApprove(String, Option<String>),
    ProposalsReject(String, Option<String>),
    ResearchReplay(String),
    ResearchBacktest {
        config_path: String,
        db_url: Option<String>,
        start_date: Option<String>,
        end_date: Option<String>,
    },
    ResearchCompare(String, String),
    ResearchOversight,
}

impl Command {
    fn parse(args: &[String]) -> Result<Self, String> {
        match args {
            [_bin, system, status] if system == "system" && status == "status" => {
                Ok(Self::SystemStatus)
            }
            [_bin, system, metrics] if system == "system" && metrics == "metrics" => {
                Ok(Self::SystemMetrics)
            }
            [_bin, system, alerts] if system == "system" && alerts == "alerts" => {
                Ok(Self::SystemAlerts)
            }
            [_bin, system, audit] if system == "system" && audit == "audit" => {
                Ok(Self::SystemAudit)
            }
            [_bin, system, diagnose] if system == "system" && diagnose == "diagnose" => {
                Ok(Self::SystemDiagnose)
            }
            [_bin, trading, status] if trading == "trading" && status == "status" => {
                Ok(Self::TradingStatus)
            }
            [_bin, trading, inspect, deployment_id]
                if trading == "trading" && inspect == "inspect" =>
            {
                Ok(Self::TradingInspect(deployment_id.clone()))
            }
            [_bin, trading, diagnose, deployment_id]
                if trading == "trading" && diagnose == "diagnose" =>
            {
                Ok(Self::TradingDiagnose(deployment_id.clone()))
            }
            [_bin, trading, cancel, deployment_id, order_id]
                if trading == "trading" && cancel == "cancel" =>
            {
                Ok(Self::TradingCancel(
                    deployment_id.clone(),
                    order_id.clone(),
                ))
            }
            [_bin, trading, replace, deployment_id, order_id, quantity, limit_price]
                if trading == "trading" && replace == "replace" =>
            {
                let quantity = quantity
                    .parse()
                    .map_err(|err| format!("invalid quantity `{quantity}`: {err}"))?;
                let limit_price = if limit_price == "-" {
                    None
                } else {
                    Some(
                        limit_price
                            .parse()
                            .map_err(|err| format!("invalid limit price `{limit_price}`: {err}"))?,
                    )
                };
                Ok(Self::TradingReplace(
                    deployment_id.clone(),
                    order_id.clone(),
                    quantity,
                    limit_price,
                ))
            }
            [_bin, deployments, list] if deployments == "deployments" && list == "list" => {
                Ok(Self::DeploymentsList)
            }
            [_bin, deployments, inspect, deployment_id]
                if deployments == "deployments" && inspect == "inspect" =>
            {
                Ok(Self::DeploymentsInspect(deployment_id.clone()))
            }
            [_bin, deployments, apply, manifest_path]
                if deployments == "deployments" && apply == "apply" =>
            {
                Ok(Self::DeploymentsApply(manifest_path.clone()))
            }
            [_bin, deployments, pause, deployment_id]
                if deployments == "deployments" && pause == "pause" =>
            {
                Ok(Self::DeploymentsPause(deployment_id.clone()))
            }
            [_bin, deployments, resume, deployment_id]
                if deployments == "deployments" && resume == "resume" =>
            {
                Ok(Self::DeploymentsResume(deployment_id.clone()))
            }
            [_bin, deployments, stop, deployment_id]
                if deployments == "deployments" && stop == "stop" =>
            {
                Ok(Self::DeploymentsStop(deployment_id.clone()))
            }
            [_bin, deployments, drain, deployment_id]
                if deployments == "deployments" && drain == "drain" =>
            {
                Ok(Self::DeploymentsDrain(deployment_id.clone()))
            }
            [_bin, deployments, enable, deployment_id]
                if deployments == "deployments" && enable == "enable" =>
            {
                Ok(Self::DeploymentsEnable(deployment_id.clone()))
            }
            [_bin, deployments, disable, deployment_id]
                if deployments == "deployments" && disable == "disable" =>
            {
                Ok(Self::DeploymentsDisable(deployment_id.clone()))
            }
            [_bin, deployments, archive, deployment_id]
                if deployments == "deployments" && archive == "archive" =>
            {
                Ok(Self::DeploymentsArchive(deployment_id.clone()))
            }
            [_bin, proposals, list] if proposals == "proposals" && list == "list" => {
                Ok(Self::ProposalsList)
            }
            [..] if args.len() >= 4 && args[1] == "proposals" && args[2] == "approve" => Ok(
                Self::ProposalsApprove(args[3].clone(), flag_value(args, "--note").map(str::to_string)),
            ),
            [..] if args.len() >= 4 && args[1] == "proposals" && args[2] == "reject" => Ok(
                Self::ProposalsReject(args[3].clone(), flag_value(args, "--note").map(str::to_string)),
            ),
            [_bin, research, replay, deployment_id]
                if research == "research" && replay == "replay" =>
            {
                Ok(Self::ResearchReplay(deployment_id.clone()))
            }
            [..] if args.len() >= 3 && args[1] == "research" && args[2] == "backtest" => {
                let config_path = flag_value(args, "--config")
                    .map(str::to_string)
                    .unwrap_or_else(|| research::default_config_path().display().to_string());
                Ok(Self::ResearchBacktest {
                    config_path,
                    db_url: flag_value(args, "--db-url").map(str::to_string),
                    start_date: flag_value(args, "--start-date").map(str::to_string),
                    end_date: flag_value(args, "--end-date").map(str::to_string),
                })
            }
            [_bin, research, compare, left_path, right_path]
                if research == "research" && compare == "compare" =>
            {
                Ok(Self::ResearchCompare(
                    left_path.clone(),
                    right_path.clone(),
                ))
            }
            [_bin, research, oversight] if research == "research" && oversight == "oversight" => {
                Ok(Self::ResearchOversight)
            }
            _ => Err("usage: ployctl system status | ployctl system metrics | ployctl system alerts | ployctl system audit | ployctl system diagnose | ployctl trading status | ployctl trading inspect <deployment-id> | ployctl trading diagnose <deployment-id> | ployctl trading cancel <deployment-id> <order-id> | ployctl trading replace <deployment-id> <order-id> <quantity> <limit-price|-> | ployctl deployments list | ployctl deployments inspect <deployment-id> | ployctl deployments apply <manifest.json> | ployctl deployments pause <deployment-id> | ployctl deployments resume <deployment-id> | ployctl deployments stop <deployment-id> | ployctl deployments drain <deployment-id> | ployctl deployments enable <deployment-id> | ployctl deployments disable <deployment-id> | ployctl deployments archive <deployment-id> | ployctl proposals list | ployctl proposals approve <proposal-id> [--note <text>] | ployctl proposals reject <proposal-id> [--note <text>] | ployctl research replay <deployment-id> | ployctl research backtest [--config <path>] [--db-url <postgres-url>] [--start-date <YYYY-MM-DD>] [--end-date <YYYY-MM-DD>] | ployctl research compare <left.toml> <right.toml> | ployctl research oversight".to_string()),
        }
    }
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|idx| args.get(idx + 1))
        .map(String::as_str)
}

fn main() {
    let command =
        Command::parse(&std::env::args().collect::<Vec<_>>()).expect("valid ployctl command");
    let client = ControlPlaneClient::default();

    match execute(command, &client) {
        Ok(output) => eprintln!("{output}"),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

fn execute(command: Command, client: &ControlPlaneClient) -> Result<String, String> {
    match command {
        Command::SystemStatus => system::render_system_status(client),
        Command::SystemMetrics => system::render_system_metrics(client),
        Command::SystemAlerts => system::render_system_alerts(client),
        Command::SystemAudit => system::render_audit_log(client),
        Command::SystemDiagnose => system::render_system_diagnostics(client),
        Command::TradingStatus => trading::render_trading_state(client),
        Command::TradingInspect(deployment_id) => {
            trading::render_one_trading_state(client, &deployment_id)
        }
        Command::TradingDiagnose(deployment_id) => {
            trading::render_deployment_diagnostics(client, &deployment_id)
        }
        Command::TradingCancel(deployment_id, order_id) => {
            trading::cancel_order(client, &deployment_id, &order_id)
        }
        Command::TradingReplace(deployment_id, order_id, quantity, limit_price) => {
            trading::replace_order(client, &deployment_id, &order_id, quantity, limit_price)
        }
        Command::DeploymentsList => deployments::render_deployments(client),
        Command::DeploymentsInspect(deployment_id) => {
            deployments::render_deployment(client, &deployment_id)
        }
        Command::DeploymentsApply(manifest_path) => {
            deployments::apply_deployment_file(client, std::path::Path::new(&manifest_path))
        }
        Command::DeploymentsPause(deployment_id) => deployments::control_deployment(
            client,
            &deployment_id,
            ploy_operator_contracts::DesiredState::Paused,
        ),
        Command::DeploymentsResume(deployment_id) => deployments::control_deployment(
            client,
            &deployment_id,
            ploy_operator_contracts::DesiredState::Running,
        ),
        Command::DeploymentsStop(deployment_id) => deployments::control_deployment(
            client,
            &deployment_id,
            ploy_operator_contracts::DesiredState::Stopped,
        ),
        Command::DeploymentsDrain(deployment_id) => deployments::set_lifecycle_state(
            client,
            &deployment_id,
            ploy_operator_contracts::DeploymentState::Draining,
        ),
        Command::DeploymentsEnable(deployment_id) => deployments::set_lifecycle_state(
            client,
            &deployment_id,
            ploy_operator_contracts::DeploymentState::Enabled,
        ),
        Command::DeploymentsDisable(deployment_id) => deployments::set_lifecycle_state(
            client,
            &deployment_id,
            ploy_operator_contracts::DeploymentState::Disabled,
        ),
        Command::DeploymentsArchive(deployment_id) => deployments::set_lifecycle_state(
            client,
            &deployment_id,
            ploy_operator_contracts::DeploymentState::Archived,
        ),
        Command::ProposalsList => proposals::render_proposals(client),
        Command::ProposalsApprove(proposal_id, note) => {
            proposals::approve_proposal(client, &proposal_id, note)
        }
        Command::ProposalsReject(proposal_id, note) => {
            proposals::reject_proposal(client, &proposal_id, note)
        }
        Command::ResearchReplay(deployment_id) => research::render_replay(client, &deployment_id),
        Command::ResearchBacktest {
            config_path,
            db_url,
            start_date,
            end_date,
        } => research::run_backtest(&research::BacktestRequest {
            config_path,
            db_url,
            start_date,
            end_date,
        }),
        Command::ResearchCompare(left_path, right_path) => {
            research::compare_configs(Path::new(&left_path), Path::new(&right_path))
        }
        Command::ResearchOversight => research::render_oversight(client),
    }
}

#[cfg(test)]
mod tests {
    use super::{execute, Command};
    use ployctl::client::ControlPlaneClient;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployctl-main-{label}-{unique}"))
    }

    #[test]
    fn parses_system_status_command() {
        let command =
            Command::parse(&["ployctl", "system", "status"].map(str::to_string)).expect("command");
        assert_eq!(command, Command::SystemStatus);
    }

    #[test]
    fn parses_system_audit_command() {
        let command =
            Command::parse(&["ployctl", "system", "audit"].map(str::to_string)).expect("command");
        assert_eq!(command, Command::SystemAudit);
    }

    #[test]
    fn parses_system_diagnose_command() {
        let command = Command::parse(&["ployctl", "system", "diagnose"].map(str::to_string))
            .expect("command");
        assert_eq!(command, Command::SystemDiagnose);
    }

    #[test]
    fn parses_system_metrics_command() {
        let command =
            Command::parse(&["ployctl", "system", "metrics"].map(str::to_string)).expect("command");
        assert_eq!(command, Command::SystemMetrics);
    }

    #[test]
    fn parses_system_alerts_command() {
        let command =
            Command::parse(&["ployctl", "system", "alerts"].map(str::to_string)).expect("command");
        assert_eq!(command, Command::SystemAlerts);
    }

    #[test]
    fn parses_trading_status_command() {
        let command =
            Command::parse(&["ployctl", "trading", "status"].map(str::to_string)).expect("command");
        assert_eq!(command, Command::TradingStatus);
    }

    #[test]
    fn parses_trading_inspect_command() {
        let command =
            Command::parse(&["ployctl", "trading", "inspect", "example.paper"].map(str::to_string))
                .expect("command");
        assert_eq!(
            command,
            Command::TradingInspect("example.paper".to_string())
        );
    }

    #[test]
    fn parses_trading_diagnose_command() {
        let command = Command::parse(
            &["ployctl", "trading", "diagnose", "example.paper"].map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            command,
            Command::TradingDiagnose("example.paper".to_string())
        );
    }

    #[test]
    fn parses_trading_cancel_command() {
        let command = Command::parse(
            &["ployctl", "trading", "cancel", "example.live", "order-1"].map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            command,
            Command::TradingCancel("example.live".to_string(), "order-1".to_string())
        );
    }

    #[test]
    fn parses_trading_replace_command() {
        let command = Command::parse(
            &[
                "ployctl",
                "trading",
                "replace",
                "example.live",
                "order-1",
                "2.5",
                "0.57",
            ]
            .map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            command,
            Command::TradingReplace(
                "example.live".to_string(),
                "order-1".to_string(),
                rust_decimal::Decimal::new(25, 1),
                Some(rust_decimal::Decimal::new(57, 2)),
            )
        );
    }

    #[test]
    fn parses_deployment_inspect_command() {
        let command = Command::parse(
            &["ployctl", "deployments", "inspect", "example.paper"].map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            command,
            Command::DeploymentsInspect("example.paper".to_string())
        );
    }

    #[test]
    fn parses_deployment_apply_command() {
        let command = Command::parse(
            &["ployctl", "deployments", "apply", "example.paper.json"].map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            command,
            Command::DeploymentsApply("example.paper.json".to_string())
        );
    }

    #[test]
    fn parses_deployment_pause_command() {
        let command = Command::parse(
            &["ployctl", "deployments", "pause", "example.paper"].map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            command,
            Command::DeploymentsPause("example.paper".to_string())
        );
    }

    #[test]
    fn parses_deployment_drain_command() {
        let command = Command::parse(
            &["ployctl", "deployments", "drain", "example.paper"].map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            command,
            Command::DeploymentsDrain("example.paper".to_string())
        );
    }

    #[test]
    fn parses_research_replay_command() {
        let command =
            Command::parse(&["ployctl", "research", "replay", "example.paper"].map(str::to_string))
                .expect("command");
        assert_eq!(format!("{command:?}"), "ResearchReplay(\"example.paper\")");
    }

    #[test]
    fn parses_research_backtest_command() {
        let command = Command::parse(
            &[
                "ployctl",
                "research",
                "backtest",
                "--config",
                "config/strategies/02-pm5d.unified.toml",
                "--db-url",
                "postgres://localhost/ploy",
                "--start-date",
                "2026-04-01",
                "--end-date",
                "2026-04-03",
            ]
            .map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            format!("{command:?}"),
            "ResearchBacktest { config_path: \"config/strategies/02-pm5d.unified.toml\", db_url: Some(\"postgres://localhost/ploy\"), start_date: Some(\"2026-04-01\"), end_date: Some(\"2026-04-03\") }"
        );
    }

    #[test]
    fn parses_research_compare_command() {
        let command = Command::parse(
            &[
                "ployctl",
                "research",
                "compare",
                "config/strategies/a.toml",
                "config/strategies/b.toml",
            ]
            .map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            format!("{command:?}"),
            "ResearchCompare(\"config/strategies/a.toml\", \"config/strategies/b.toml\")"
        );
    }

    #[test]
    fn parses_research_oversight_command() {
        let command = Command::parse(&["ployctl", "research", "oversight"].map(str::to_string))
            .expect("command");
        assert_eq!(command, Command::ResearchOversight);
    }

    #[test]
    fn parses_proposals_list_command() {
        let command =
            Command::parse(&["ployctl", "proposals", "list"].map(str::to_string)).expect("command");
        assert_eq!(command, Command::ProposalsList);
    }

    #[test]
    fn parses_proposals_approve_command() {
        let command = Command::parse(
            &[
                "ployctl",
                "proposals",
                "approve",
                "proposal-1",
                "--note",
                "approved",
            ]
            .map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            command,
            Command::ProposalsApprove("proposal-1".to_string(), Some("approved".to_string()))
        );
    }

    #[test]
    fn parses_proposals_reject_command() {
        let command = Command::parse(
            &["ployctl", "proposals", "reject", "proposal-1"].map(str::to_string),
        )
        .expect("command");
        assert_eq!(
            command,
            Command::ProposalsReject("proposal-1".to_string(), None)
        );
    }

    #[test]
    fn execute_returns_error_for_missing_deployment_instead_of_panicking() {
        let runtime_root = temp_dir("missing-deployment");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(runtime_root.join("deployments.json"), "[]").expect("write deployments");
        let client = ControlPlaneClient::from_runtime_root(&runtime_root);

        let error = execute(
            Command::DeploymentsInspect("missing.paper".to_string()),
            &client,
        )
        .expect_err("missing deployment should error");
        assert!(error.contains("missing.paper"));
    }
}
