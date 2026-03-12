use super::*;
use crate::control_plane::deployment_files::{deployment_file_candidates, deployments_state_path};

pub(super) fn lob_levels_json(
    state: &crate::collector::OrderBookState,
    is_bids: bool,
    max_levels: usize,
) -> Vec<(String, String)> {
    let max_levels = max_levels.max(1);

    if is_bids {
        state
            .bids
            .iter()
            .rev()
            .take(max_levels)
            .map(|(price_cents, qty)| {
                let price =
                    rust_decimal::Decimal::from(*price_cents) / rust_decimal::Decimal::from(100);
                (price.to_string(), qty.to_string())
            })
            .collect()
    } else {
        state
            .asks
            .iter()
            .take(max_levels)
            .map(|(price_cents, qty)| {
                let price =
                    rust_decimal::Decimal::from(*price_cents) / rust_decimal::Decimal::from(100);
                (price.to_string(), qty.to_string())
            })
            .collect()
    }
}

pub(super) fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

pub(super) fn env_bool(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

pub(super) fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

pub(super) fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(default)
}

pub(super) fn env_decimal(name: &str, default: rust_decimal::Decimal) -> rust_decimal::Decimal {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<rust_decimal::Decimal>().ok())
        .unwrap_or(default)
}

pub(super) fn env_decimal_opt(name: &str) -> Option<rust_decimal::Decimal> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<rust_decimal::Decimal>().ok())
}

pub(super) fn parse_strategy_deployments(raw: &str) -> Vec<StrategyDeployment> {
    let mut out = Vec::new();
    match serde_json::from_str::<Vec<StrategyDeployment>>(raw) {
        Ok(items) => {
            for mut dep in items {
                if dep.id.trim().is_empty() {
                    continue;
                }
                dep.normalize_account_ids_in_place();
                out.push(dep);
            }
        }
        Err(e) => {
            warn!(error = %e, "failed to parse strategy deployments JSON");
        }
    }
    out
}

pub(super) fn load_strategy_deployments() -> Vec<StrategyDeployment> {
    let raw = std::env::var("PLOY_STRATEGY_DEPLOYMENTS_JSON")
        .or_else(|_| std::env::var("PLOY_DEPLOYMENTS_JSON"))
        .unwrap_or_default();
    if !raw.trim().is_empty() {
        return parse_strategy_deployments(&raw);
    }

    for path in deployment_file_candidates(&deployments_state_path()) {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let items = parse_strategy_deployments(&contents);
            if !items.is_empty() {
                return items;
            }
        }
    }
    Vec::new()
}
