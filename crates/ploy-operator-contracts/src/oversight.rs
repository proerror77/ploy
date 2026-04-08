use crate::{DeploymentSummary, DesiredState, ObservedState, SystemStatus, TradingStateSnapshot};
use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OversightSignal {
    pub severity: String,
    pub kind: String,
    pub deployment_id: Option<String>,
    pub message: String,
    pub recommended_action: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OversightAction {
    pub kind: String,
    pub target: String,
    pub rationale: String,
    pub operator_command: String,
    pub config_hint: Option<String>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OversightReport {
    pub timestamp: String,
    pub platform_status: String,
    pub deployments_reviewed: usize,
    pub signal_count: usize,
    pub signals: Vec<OversightSignal>,
    pub recommended_actions: Vec<OversightAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OversightSnapshotEvent {
    pub oversight: OversightReport,
}

pub fn compute_oversight_report(
    system: &SystemStatus,
    deployments: &[DeploymentSummary],
    trading: &[TradingStateSnapshot],
) -> OversightReport {
    let signals = collect_oversight_signals(system, deployments, trading);
    OversightReport {
        timestamp: Utc::now().to_rfc3339(),
        platform_status: system.status.clone(),
        deployments_reviewed: deployments.len(),
        signal_count: signals.len(),
        recommended_actions: build_oversight_actions(&signals),
        signals,
    }
}

pub fn collect_oversight_signals(
    system: &SystemStatus,
    deployments: &[DeploymentSummary],
    trading: &[TradingStateSnapshot],
) -> Vec<OversightSignal> {
    let system_error_threshold = env_i64("SIDECAR_ALERT_SYSTEM_ERRORS_1H", 1);
    let pending_intent_threshold = env_usize("SIDECAR_ALERT_PENDING_INTENTS", 3);
    let active_order_threshold = env_usize("SIDECAR_ALERT_ACTIVE_ORDERS", 4);
    let open_position_threshold = env_usize("SIDECAR_ALERT_OPEN_POSITIONS", 3);
    let gross_exposure_threshold = env_decimal("SIDECAR_ALERT_GROSS_EXPOSURE", Decimal::new(50, 1));
    let net_pnl_drawdown_threshold =
        env_decimal("SIDECAR_ALERT_NET_PNL_DRAWDOWN", Decimal::new(-20, 1));

    let deployments_by_id: BTreeMap<_, _> = deployments
        .iter()
        .map(|deployment| (deployment.deployment_id.as_str(), deployment))
        .collect();
    let mut signals = Vec::new();

    if system.error_count_1h >= system_error_threshold {
        signals.push(OversightSignal {
            severity: if system.error_count_1h >= system_error_threshold * 3 {
                "critical".to_string()
            } else {
                "warning".to_string()
            },
            kind: "system_errors".to_string(),
            deployment_id: None,
            message: format!(
                "system reported {} errors in the last hour",
                system.error_count_1h
            ),
            recommended_action: "human_follow_up".to_string(),
            evidence: vec![
                format!("system.status={}", system.status),
                format!("error_count_1h={}", system.error_count_1h),
            ],
        });
    }

    for deployment in deployments {
        if deployment.desired_state == DesiredState::Running
            && deployment.observed_state != ObservedState::Running
        {
            signals.push(OversightSignal {
                severity: if matches!(
                    deployment.observed_state,
                    ObservedState::Degraded | ObservedState::Failed
                ) {
                    "critical".to_string()
                } else {
                    "warning".to_string()
                },
                kind: "state_mismatch".to_string(),
                deployment_id: Some(deployment.deployment_id.clone()),
                message: format!(
                    "deployment expected running but observed {:?}",
                    deployment.observed_state
                ),
                recommended_action: "human_follow_up".to_string(),
                evidence: vec![
                    format!("desired_state={:?}", deployment.desired_state),
                    format!("observed_state={:?}", deployment.observed_state),
                    format!("bundle_id={}", deployment.bundle_id),
                    format!("runtime_mode={}", deployment.runtime_mode),
                ],
            });
        }
    }

    for snapshot in trading {
        let pending_intents = snapshot.risk.pending_intents;
        let active_orders = snapshot.risk.active_orders;
        let open_positions = snapshot.risk.open_positions;
        let gross_exposure = snapshot.risk.gross_exposure;
        let net_pnl = snapshot.pnl.net_pnl;
        let deployment = deployments_by_id.get(snapshot.deployment_id.as_str());
        let desired_state = deployment
            .map(|item| format!("{:?}", item.desired_state))
            .unwrap_or_else(|| "Unknown".to_string());
        let bundle_id = deployment
            .map(|item| item.bundle_id.as_str())
            .unwrap_or("unknown");

        if pending_intents >= pending_intent_threshold {
            signals.push(OversightSignal {
                severity: if pending_intents >= pending_intent_threshold * 2 {
                    "critical".to_string()
                } else {
                    "warning".to_string()
                },
                kind: "order_buildup".to_string(),
                deployment_id: Some(snapshot.deployment_id.clone()),
                message: format!("pending intents elevated at {pending_intents}"),
                recommended_action: "replay".to_string(),
                evidence: vec![
                    format!("pending_intents={pending_intents}"),
                    format!("desired_state={desired_state}"),
                    format!("bundle_id={bundle_id}"),
                ],
            });
        }

        if active_orders >= active_order_threshold {
            signals.push(OversightSignal {
                severity: if active_orders >= active_order_threshold * 2 {
                    "critical".to_string()
                } else {
                    "warning".to_string()
                },
                kind: "order_buildup".to_string(),
                deployment_id: Some(snapshot.deployment_id.clone()),
                message: format!("active orders elevated at {active_orders}"),
                recommended_action: "replay".to_string(),
                evidence: vec![
                    format!("active_orders={active_orders}"),
                    format!("runtime_mode={}", snapshot.runtime_mode),
                    format!("bundle_id={bundle_id}"),
                ],
            });
        }

        if open_positions >= open_position_threshold {
            signals.push(OversightSignal {
                severity: if open_positions >= open_position_threshold * 2 {
                    "critical".to_string()
                } else {
                    "warning".to_string()
                },
                kind: "position_buildup".to_string(),
                deployment_id: Some(snapshot.deployment_id.clone()),
                message: format!("open positions elevated at {open_positions}"),
                recommended_action: "compare_configs".to_string(),
                evidence: vec![
                    format!("open_positions={open_positions}"),
                    format!("runtime_mode={}", snapshot.runtime_mode),
                    format!("bundle_id={bundle_id}"),
                ],
            });
        }

        if gross_exposure >= gross_exposure_threshold {
            signals.push(OversightSignal {
                severity: if gross_exposure >= gross_exposure_threshold * Decimal::new(2, 0) {
                    "critical".to_string()
                } else {
                    "warning".to_string()
                },
                kind: "exposure_watch".to_string(),
                deployment_id: Some(snapshot.deployment_id.clone()),
                message: format!("gross exposure elevated at {gross_exposure}"),
                recommended_action: "pause_review".to_string(),
                evidence: vec![
                    format!("gross_exposure={gross_exposure}"),
                    format!("runtime_mode={}", snapshot.runtime_mode),
                    format!("bundle_id={bundle_id}"),
                ],
            });
        }

        if net_pnl <= net_pnl_drawdown_threshold {
            signals.push(OversightSignal {
                severity: if net_pnl <= net_pnl_drawdown_threshold * Decimal::new(2, 0) {
                    "critical".to_string()
                } else {
                    "warning".to_string()
                },
                kind: "pnl_regression".to_string(),
                deployment_id: Some(snapshot.deployment_id.clone()),
                message: format!("net pnl deteriorated to {net_pnl}"),
                recommended_action: "backtest".to_string(),
                evidence: vec![
                    format!("net_pnl={net_pnl}"),
                    format!("runtime_mode={}", snapshot.runtime_mode),
                    format!("bundle_id={bundle_id}"),
                ],
            });
        }
    }

    signals
}

pub fn build_oversight_actions(signals: &[OversightSignal]) -> Vec<OversightAction> {
    let mut actions: BTreeMap<(String, String), OversightAction> = BTreeMap::new();

    for signal in signals {
        let target = signal
            .deployment_id
            .clone()
            .unwrap_or_else(|| "platform".to_string());
        let key = (signal.recommended_action.clone(), target.clone());
        let config_hint = resolve_strategy_config_hint(signal);
        actions.entry(key).or_insert_with(|| OversightAction {
            kind: signal.recommended_action.clone(),
            target: target.clone(),
            rationale: signal.message.clone(),
            operator_command: build_operator_command(
                &signal.recommended_action,
                &target,
                config_hint.as_deref(),
            ),
            config_hint,
            evidence: signal.evidence.clone(),
        });
    }

    actions.into_values().collect()
}

pub fn build_operator_command(kind: &str, target: &str, config_hint: Option<&str>) -> String {
    match kind {
        "human_follow_up" if target == "platform" => "ployctl system audit".to_string(),
        "human_follow_up" => format!("ployctl deployments inspect {target}"),
        "replay" => format!("ployctl research replay {target}"),
        "compare_configs" => config_hint
            .map(|path| format!("ployctl research compare {path} <other-config>"))
            .unwrap_or_else(|| "ployctl research compare <left-config> <right-config>".to_string()),
        "pause_review" => format!("ployctl trading inspect {target}"),
        "backtest" => config_hint
            .map(|path| {
                format!(
                    "ployctl research backtest --config {path} --db-url <DATABASE_URL> --start-date <YYYY-MM-DD> --end-date <YYYY-MM-DD>"
                )
            })
            .unwrap_or_else(|| {
                "ployctl research backtest --config <strategy-config> --db-url <DATABASE_URL> --start-date <YYYY-MM-DD> --end-date <YYYY-MM-DD>".to_string()
            }),
        _ => "ployctl system status".to_string(),
    }
}

fn resolve_strategy_config_hint(signal: &OversightSignal) -> Option<String> {
    let bundle_id = extract_evidence_value(&signal.evidence, "bundle_id")?;
    let runtime_mode = extract_evidence_value(&signal.evidence, "runtime_mode");

    match (bundle_id.as_str(), runtime_mode.as_deref()) {
        ("example", _) | ("openclaw", _) | ("pm5-directional", _) | ("pm5d", _) => {
            Some("config/strategies/02-pm5d.unified.toml".to_string())
        }
        ("momentum", Some("live")) => {
            Some("config/strategies/01-momentum.live-aws.toml".to_string())
        }
        ("momentum", _) => Some("config/strategies/01-momentum.default.toml".to_string()),
        ("pattern-memory", _) | ("pattern_memory", _) => {
            Some("config/strategies/03-pattern-memory.default.toml".to_string())
        }
        ("staggered-arb", _) | ("staggered_arb", _) => {
            Some("config/strategies/04-staggered-arb.live.toml".to_string())
        }
        ("split-arb", _) | ("split_arb", _) => {
            Some("config/strategies/05-split-arb.default.toml".to_string())
        }
        ("gamma-scalping", _) | ("gamma_scalping", _) => {
            Some("config/strategies/06-gamma-scalping.default.toml".to_string())
        }
        ("liquidity-vacuum", _) | ("liquidity_vacuum", _) => {
            Some("config/strategies/07-liquidity-vacuum.template.toml".to_string())
        }
        _ => None,
    }
}

fn extract_evidence_value(evidence: &[String], key: &str) -> Option<String> {
    evidence.iter().find_map(|entry| {
        let (lhs, rhs) = entry.split_once('=')?;
        (lhs == key).then(|| rhs.to_string())
    })
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_decimal(key: &str, default: Decimal) -> Decimal {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<Decimal>().ok())
        .unwrap_or(default)
}
