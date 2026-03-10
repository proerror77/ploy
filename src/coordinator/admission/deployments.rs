use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::control_plane::{MarketSelector, StrategyDeployment};
use crate::coordinator::capital::CryptoHorizon;
use crate::coordinator::OrderIntent;

pub(in crate::coordinator) fn buy_intent_missing_deployment_reason(
    intent: &OrderIntent,
) -> Option<String> {
    if !intent.is_buy {
        return None;
    }

    if intent.deployment_id().is_some() {
        None
    } else {
        Some("BUY intent missing required metadata field 'deployment_id'".to_string())
    }
}

pub(super) fn deployment_gate_required() -> bool {
    match std::env::var("PLOY_DEPLOYMENT_GATE_REQUIRED")
        .ok()
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
    {
        Some(v) => !matches!(v.as_str(), "0" | "false" | "no" | "off"),
        None => true,
    }
}

pub(super) fn load_strategy_deployments() -> HashMap<String, StrategyDeployment> {
    let raw = std::env::var("PLOY_STRATEGY_DEPLOYMENTS_JSON")
        .or_else(|_| std::env::var("PLOY_DEPLOYMENTS_JSON"))
        .unwrap_or_default();
    if !raw.trim().is_empty() {
        return parse_strategy_deployments(&raw);
    }

    let repo_state_path = Path::new("data/state/deployments.json");
    let container_data_path = Path::new("/opt/ploy/data/state/deployments.json");
    let candidates = [
        deployments_state_path(),
        repo_state_path.to_path_buf(),
        container_data_path.to_path_buf(),
        Path::new("deployment/deployments.json").to_path_buf(),
        Path::new("/opt/ploy/deployment/deployments.json").to_path_buf(),
    ];

    for path in candidates {
        if let Ok(contents) = std::fs::read_to_string(path) {
            let parsed = parse_strategy_deployments(&contents);
            if !parsed.is_empty() {
                return parsed;
            }
        }
    }

    HashMap::new()
}

pub(super) fn metadata_value<'a>(
    metadata: &'a HashMap<String, String>,
    keys: &[&str],
) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| metadata.get(*k))
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

pub(super) fn enforce_deployment_gate_with_snapshot(
    account_id: &str,
    dry_run: bool,
    deployments: &HashMap<String, StrategyDeployment>,
    intent: &mut OrderIntent,
) -> std::result::Result<(), String> {
    if !intent.is_buy || dry_run || !deployment_gate_required() {
        return Ok(());
    }

    if deployments.is_empty() {
        return Err("deployment registry is empty while deployment gate is required".to_string());
    }

    if let Some(deployment_id) = metadata_value(&intent.metadata, &["deployment_id"]) {
        let Some(deployment) = deployments.get(deployment_id) else {
            return Err(format!("unknown deployment_id: {}", deployment_id));
        };
        if !deployment_runtime_eligible(deployment, account_id, dry_run, intent) {
            return Err(format!(
                "deployment {} is not eligible for runtime/account/domain/timeframe/selector binding",
                deployment.id
            ));
        }
        apply_deployment_metadata(intent, deployment);
        return Ok(());
    }

    let strategy = metadata_value(&intent.metadata, &["strategy", "deployment_strategy"])
        .ok_or_else(|| "strategy metadata is required for live BUY intents".to_string())?;

    let mut candidates: Vec<&StrategyDeployment> = deployments
        .values()
        .filter(|deployment| {
            deployment_runtime_eligible(deployment, account_id, dry_run, intent)
                && strategy_matches(strategy, deployment.strategy.as_str())
        })
        .collect();

    if candidates.is_empty() {
        let mut domain_candidates: Vec<&StrategyDeployment> = deployments
            .values()
            .filter(|deployment| {
                deployment_runtime_eligible(deployment, account_id, dry_run, intent)
            })
            .collect();
        domain_candidates.sort_by(|a, b| a.id.cmp(&b.id));

        if domain_candidates.len() == 1 {
            let deployment = domain_candidates[0];
            apply_deployment_metadata(intent, deployment);
            intent.metadata.insert(
                "deployment_resolution".to_string(),
                "domain_singleton_fallback".to_string(),
            );
            return Ok(());
        }

        return Err(format!(
            "no eligible deployment found for strategy={} domain={} market={}",
            strategy, intent.domain, intent.market_slug
        ));
    }

    candidates.sort_by(|a, b| a.id.cmp(&b.id));

    if candidates.len() > 1 {
        let ids = candidates
            .iter()
            .map(|d| d.id.clone())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "ambiguous deployment resolution for strategy={} market={}: {}",
            strategy, intent.market_slug, ids
        ));
    }

    let deployment = candidates[0];
    apply_deployment_metadata(intent, deployment);
    Ok(())
}

pub(super) fn infer_time_bucket_seconds(intent: &OrderIntent) -> i64 {
    if let Some(raw) = intent.metadata.get("event_window_secs") {
        if let Ok(v) = raw.trim().parse::<i64>() {
            if v > 0 {
                return v;
            }
        }
    }

    let mut hints: Vec<&str> = Vec::new();
    if let Some(h) = intent.metadata.get("timeframe") {
        hints.push(h.as_str());
    }
    if let Some(h) = intent.metadata.get("horizon") {
        hints.push(h.as_str());
    }
    if let Some(h) = intent.metadata.get("series_id") {
        hints.push(h.as_str());
    }

    for raw in hints {
        if let Some(horizon) = CryptoHorizon::from_hint(raw) {
            return match horizon {
                CryptoHorizon::M15 => 15 * 60,
                CryptoHorizon::M5 => 5 * 60,
                CryptoHorizon::Other => 5 * 60,
            };
        }
    }

    5 * 60
}

fn deployments_state_path() -> PathBuf {
    if let Ok(path) = std::env::var("PLOY_DEPLOYMENTS_FILE") {
        return PathBuf::from(path);
    }

    let container_data_root = Path::new("/opt/ploy/data");
    if container_data_root.exists() {
        return container_data_root.join("state/deployments.json");
    }

    let repo_state_deployment = Path::new("data/state/deployments.json");
    if repo_state_deployment.exists() {
        return repo_state_deployment.to_path_buf();
    }

    let repo_root_deployment = Path::new("deployment/deployments.json");
    if repo_root_deployment.exists() {
        return repo_root_deployment.to_path_buf();
    }

    let container_deployment = Path::new("/opt/ploy/deployment/deployments.json");
    if container_deployment.exists() {
        return container_deployment.to_path_buf();
    }

    PathBuf::from("data/state/deployments.json")
}

fn parse_strategy_deployments(raw: &str) -> HashMap<String, StrategyDeployment> {
    let mut out = HashMap::new();
    if let Ok(items) = serde_json::from_str::<Vec<StrategyDeployment>>(raw) {
        for mut dep in items {
            let id = dep.id.trim().to_string();
            if id.is_empty() {
                continue;
            }
            dep.id = id.clone();
            dep.normalize_account_ids_in_place();
            out.insert(id, dep);
        }
    }
    out
}

fn normalized_token(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn strategy_matches(intent_strategy: &str, deployment_strategy: &str) -> bool {
    let intent = normalized_token(intent_strategy);
    let dep = normalized_token(deployment_strategy);
    if intent.is_empty() || dep.is_empty() {
        return false;
    }
    intent == dep || intent.contains(&dep) || dep.contains(&intent)
}

fn selector_matches_intent(
    deployment: &StrategyDeployment,
    market_slug: &str,
    metadata: &HashMap<String, String>,
) -> bool {
    match &deployment.market_selector {
        MarketSelector::Static {
            symbol,
            series_id,
            market_slug: expected_market_slug,
        } => {
            if let Some(expected) = expected_market_slug
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                if !market_slug.eq_ignore_ascii_case(expected) {
                    return false;
                }
            }

            if let Some(expected) = symbol.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                if let Some(actual) = metadata_value(metadata, &["symbol"]) {
                    if !actual.eq_ignore_ascii_case(expected) {
                        return false;
                    }
                }
            }

            if let Some(expected) = series_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                if let Some(actual) = metadata_value(metadata, &["series_id", "event_series_id"]) {
                    if !actual.eq_ignore_ascii_case(expected) {
                        return false;
                    }
                }
            }

            true
        }
        MarketSelector::Dynamic { domain, .. } => *domain == deployment.domain,
    }
}

fn timeframe_hint(intent: &OrderIntent) -> Option<String> {
    if let Some(raw) = metadata_value(&intent.metadata, &["timeframe", "horizon"]) {
        if let Some(h) = CryptoHorizon::from_hint(raw) {
            return Some(h.as_str().to_string());
        }
        return Some(raw.to_ascii_lowercase());
    }

    if let Some(raw) = metadata_value(&intent.metadata, &["series_id", "event_series_id"]) {
        if let Some(h) = CryptoHorizon::from_hint(raw) {
            return Some(h.as_str().to_string());
        }
    }

    CryptoHorizon::from_hint(&intent.market_slug).map(|h| h.as_str().to_string())
}

fn deployment_matches_timeframe(deployment: &StrategyDeployment, intent: &OrderIntent) -> bool {
    let Some(timeframe) = timeframe_hint(intent) else {
        return true;
    };
    timeframe.eq_ignore_ascii_case(deployment.timeframe.as_str())
}

fn deployment_runtime_eligible(
    deployment: &StrategyDeployment,
    account_id: &str,
    dry_run: bool,
    intent: &OrderIntent,
) -> bool {
    deployment.is_enabled_for_runtime(account_id, dry_run)
        && deployment.domain == intent.domain
        && deployment_matches_timeframe(deployment, intent)
        && selector_matches_intent(deployment, &intent.market_slug, &intent.metadata)
}

fn apply_deployment_metadata(intent: &mut OrderIntent, deployment: &StrategyDeployment) {
    intent
        .metadata
        .insert("deployment_id".to_string(), deployment.id.clone());
    intent
        .metadata
        .entry("timeframe".to_string())
        .or_insert_with(|| deployment.timeframe.as_str().to_string());
    intent
        .metadata
        .entry("allocator_profile".to_string())
        .or_insert_with(|| deployment.allocator_profile.clone());
    intent
        .metadata
        .entry("risk_profile".to_string())
        .or_insert_with(|| deployment.risk_profile.clone());
    intent
        .metadata
        .entry("deployment_strategy".to_string())
        .or_insert_with(|| deployment.strategy.clone());
    intent
        .metadata
        .entry("deployment_priority".to_string())
        .or_insert_with(|| deployment.priority.to_string());
    intent
        .metadata
        .entry("deployment_cooldown_secs".to_string())
        .or_insert_with(|| deployment.cooldown_secs.to_string());

    if let MarketSelector::Static {
        symbol,
        series_id,
        market_slug,
    } = &deployment.market_selector
    {
        if let Some(value) = symbol.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            intent
                .metadata
                .entry("symbol".to_string())
                .or_insert_with(|| value.to_string());
        }
        if let Some(value) = series_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            intent
                .metadata
                .entry("series_id".to_string())
                .or_insert_with(|| value.to_string());
            intent
                .metadata
                .entry("event_series_id".to_string())
                .or_insert_with(|| value.to_string());
        }
        if let Some(value) = market_slug
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            intent
                .metadata
                .entry("selector_market_slug".to_string())
                .or_insert_with(|| value.to_string());
        }
    }
}
