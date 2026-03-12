use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use crate::adapters::PolymarketClient;
use crate::error::Result;

use super::RedeemablePosition;

pub(super) struct EligiblePositions {
    pub(super) positions: Vec<RedeemablePosition>,
    pub(super) skipped_small: usize,
}

fn json_value_to_boolish(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(v) => Some(*v),
        serde_json::Value::Number(n) => {
            if n.as_i64() == Some(0) {
                Some(false)
            } else if n.as_i64() == Some(1) {
                Some(true)
            } else {
                None
            }
        }
        serde_json::Value::String(s) => {
            let s = s.trim().to_ascii_lowercase();
            match s.as_str() {
                "true" | "1" | "yes" | "y" => Some(true),
                "false" | "0" | "no" | "n" => Some(false),
                _ => None,
            }
        }
        _ => None,
    }
}

fn extra_truthy_flag(
    extra: &std::collections::HashMap<String, serde_json::Value>,
    keys: &[&str],
) -> bool {
    keys.iter().any(|key| {
        extra
            .get(*key)
            .and_then(json_value_to_boolish)
            .unwrap_or(false)
    })
}

fn extra_status_settled(extra: &std::collections::HashMap<String, serde_json::Value>) -> bool {
    const STATUS_KEYS: [&str; 5] = [
        "status",
        "marketStatus",
        "conditionStatus",
        "resolutionStatus",
        "state",
    ];

    STATUS_KEYS.iter().any(|key| {
        extra
            .get(*key)
            .and_then(|v| v.as_str())
            .map(|s| {
                matches!(
                    s.trim().to_ascii_lowercase().as_str(),
                    "resolved"
                        | "settled"
                        | "finalized"
                        | "closed"
                        | "redeemable"
                        | "claimable"
                        | "payout_ready"
                )
            })
            .unwrap_or(false)
    })
}

fn normalize_condition_id(input: &str) -> Option<String> {
    let normalized = input
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .to_ascii_lowercase();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn ignored_condition_patterns() -> Vec<String> {
    let raw = super::env_string_any(&["CLAIMER_IGNORE_CONDITION_IDS", "CLAIMER_IGNORE_CONDITIONS"]);
    let Some(raw) = raw else {
        return Vec::new();
    };

    raw.split(|c: char| c == ',' || c.is_whitespace())
        .filter_map(normalize_condition_id)
        .collect()
}

pub(super) fn condition_is_ignored(condition_id: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let Some(normalized) = normalize_condition_id(condition_id) else {
        return false;
    };

    patterns
        .iter()
        .any(|pattern| normalized.starts_with(pattern))
}

pub(super) async fn discover_eligible_positions(
    client: &PolymarketClient,
    min_claim_size: Decimal,
) -> Result<EligiblePositions> {
    let positions = collapse_positions_by_condition(get_redeemable_positions(client).await?);

    if positions.is_empty() {
        debug!("No redeemable positions found");
        return Ok(EligiblePositions {
            positions: vec![],
            skipped_small: 0,
        });
    }

    let mut eligible = Vec::new();
    let mut skipped_small = 0usize;
    for pos in positions {
        if pos.payout < min_claim_size {
            skipped_small += 1;
            continue;
        }
        eligible.push(pos);
    }

    if eligible.is_empty() {
        debug!(
            min_claim_size = %min_claim_size,
            skipped_small,
            "No redeemable positions above min_claim_size"
        );
    }

    Ok(EligiblePositions {
        positions: eligible,
        skipped_small,
    })
}

/// Merge multiple redeemable rows for the same condition into one claim attempt.
///
/// A condition-level redeem burns all balances for the provided index sets, so sending
/// one transaction per condition avoids duplicate claims for split rows in Data API output.
pub(super) fn collapse_positions_by_condition(
    positions: Vec<RedeemablePosition>,
) -> Vec<RedeemablePosition> {
    let mut merged: std::collections::BTreeMap<String, RedeemablePosition> =
        std::collections::BTreeMap::new();

    for pos in positions {
        if let Some(existing) = merged.get_mut(&pos.condition_id) {
            existing.size += pos.size;
            existing.payout += pos.payout;
            existing.neg_risk = existing.neg_risk || pos.neg_risk;
            if existing.outcome.is_empty() && !pos.outcome.is_empty() {
                existing.outcome = pos.outcome;
            }
            continue;
        }
        merged.insert(pos.condition_id.clone(), pos);
    }

    merged.into_values().collect()
}

/// Get list of redeemable positions from Polymarket.
pub(super) async fn get_redeemable_positions(
    client: &PolymarketClient,
) -> Result<Vec<RedeemablePosition>> {
    let positions = client.get_positions().await?;
    let allow_price_fallback = super::env_flag("CLAIMER_ALLOW_PRICE_FALLBACK", true);
    let ignored_patterns = ignored_condition_patterns();

    let mut redeemable = Vec::new();

    for p in positions {
        let size: Decimal = match p.size.parse() {
            Ok(s) if s > Decimal::ZERO => s,
            _ => continue,
        };

        let is_winner = p
            .cur_price
            .as_ref()
            .and_then(|price_str| price_str.parse::<f64>().ok())
            .map(|price| price > 0.99)
            .unwrap_or(false);

        let api_says_redeemable = p.is_redeemable()
            || extra_truthy_flag(
                &p.extra,
                &[
                    "redeemable",
                    "isRedeemable",
                    "claimable",
                    "isClaimable",
                    "canRedeem",
                    "can_redeem",
                    "readyToClaim",
                    "ready_to_claim",
                ],
            );

        let settled_hint = extra_truthy_flag(
            &p.extra,
            &[
                "resolved",
                "isResolved",
                "settled",
                "isSettled",
                "finalized",
                "isFinalized",
                "marketResolved",
                "marketFinalized",
            ],
        ) || extra_status_settled(&p.extra);

        if !api_says_redeemable && !(allow_price_fallback && is_winner && settled_hint) {
            continue;
        }
        if !api_says_redeemable && allow_price_fallback && is_winner && settled_hint {
            debug!(
                "Using settlement fallback for condition {:?} (cur_price={:?})",
                p.condition_id, p.cur_price
            );
        }

        let condition_id = p
            .condition_id
            .clone()
            .or_else(|| {
                p.extra
                    .get("conditionId")
                    .and_then(|v| v.as_str().map(ToString::to_string))
            })
            .or_else(|| {
                p.extra
                    .get("condition_id")
                    .and_then(|v| v.as_str().map(ToString::to_string))
            })
            .unwrap_or_default();
        if condition_id.trim().is_empty() {
            warn!(
                "Skipping redeemable position with missing condition_id (outcome={}, size={})",
                p.outcome.clone().unwrap_or_default(),
                size
            );
            continue;
        }

        if condition_is_ignored(&condition_id, &ignored_patterns) {
            debug!(
                "Ignoring redeemable position by condition filter: {}",
                condition_id.chars().take(16).collect::<String>()
            );
            continue;
        }

        let token_id = p
            .token_id
            .clone()
            .or_else(|| {
                p.extra
                    .get("tokenId")
                    .and_then(|v| v.as_str().map(ToString::to_string))
            })
            .or_else(|| {
                p.extra
                    .get("token_id")
                    .and_then(|v| v.as_str().map(ToString::to_string))
            })
            .unwrap_or_else(|| p.asset_id.clone());

        let outcome = p
            .outcome
            .clone()
            .or_else(|| {
                p.extra
                    .get("outcome")
                    .and_then(|v| v.as_str().map(ToString::to_string))
            })
            .unwrap_or_default();

        let payout = size;

        redeemable.push(RedeemablePosition {
            condition_id: condition_id.clone(),
            token_id,
            outcome: outcome.clone(),
            size,
            payout,
            neg_risk: p
                .negative_risk
                .or_else(|| {
                    p.extra
                        .get("neg_risk")
                        .or_else(|| p.extra.get("negRisk"))
                        .and_then(json_value_to_boolish)
                })
                .unwrap_or(false),
        });

        info!(
            "Found redeemable position: {} {} shares, condition={}",
            outcome,
            size,
            condition_id.chars().take(16).collect::<String>()
        );
    }

    Ok(redeemable)
}
