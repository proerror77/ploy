use alloy::primitives::Address;
use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use crate::{ClaimerError, RedeemablePosition};

pub(super) struct EligiblePositions {
    pub(super) positions: Vec<RedeemablePosition>,
    pub(super) skipped_small: usize,
}

fn ignored_condition_patterns() -> Vec<String> {
    let raw = crate::env_string_any(&["CLAIMER_IGNORE_CONDITION_IDS", "CLAIMER_IGNORE_CONDITIONS"]);
    let Some(raw) = raw else {
        return Vec::new();
    };
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .filter_map(normalize_condition_id)
        .collect()
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

pub(crate) fn condition_is_ignored(condition_id: &str, patterns: &[String]) -> bool {
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
    lookup_address: Address,
    min_claim_size: Decimal,
) -> Result<EligiblePositions, ClaimerError> {
    let positions = get_redeemable_positions(lookup_address).await?;

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
pub(crate) fn collapse_positions_by_condition(
    positions: Vec<RedeemablePosition>,
) -> Vec<RedeemablePosition> {
    let mut merged: std::collections::BTreeMap<String, RedeemablePosition> =
        std::collections::BTreeMap::new();

    for pos in positions {
        if let Some(existing) = merged.get_mut(&pos.condition_id) {
            existing.size += pos.size;
            existing.payout += pos.payout;
            existing.neg_risk = existing.neg_risk || pos.neg_risk;
            if existing.claim_amounts.len() < pos.claim_amounts.len() {
                existing
                    .claim_amounts
                    .resize(pos.claim_amounts.len(), Decimal::ZERO);
            }
            for (idx, amount) in pos.claim_amounts.iter().enumerate() {
                existing.claim_amounts[idx] += *amount;
            }
            if existing.outcome.is_empty() && !pos.outcome.is_empty() {
                existing.outcome = pos.outcome;
            }
            continue;
        }
        merged.insert(pos.condition_id.clone(), pos);
    }

    merged.into_values().collect()
}

/// Fetch redeemable positions from Polymarket Data API.
pub(crate) async fn get_redeemable_positions(
    lookup_address: Address,
) -> Result<Vec<RedeemablePosition>, ClaimerError> {
    use polymarket_client_sdk::data::{Client, types::request::PositionsRequest};

    let client = Client::new("https://data-api.polymarket.com")
        .map_err(|e| ClaimerError::Network(format!("Failed to create data client: {e}")))?;

    let request = PositionsRequest::builder()
        .user(lookup_address)
        .redeemable(true)
        .build();

    let positions = client
        .positions(&request)
        .await
        .map_err(|e| ClaimerError::Network(format!("Failed to fetch positions: {e}")))?;

    let ignored_patterns = ignored_condition_patterns();
    let allow_price_fallback = crate::env_flag("CLAIMER_ALLOW_PRICE_FALLBACK", true);

    let mut redeemable = Vec::new();

    for p in positions {
        if p.size <= Decimal::ZERO {
            continue;
        }

        let is_winner = p.cur_price > rust_decimal::Decimal::try_from(0.99_f64).unwrap_or(rust_decimal::Decimal::ONE);

        // API says redeemable, or price fallback for near-certain winners
        if !p.redeemable && !(allow_price_fallback && is_winner) {
            continue;
        }
        if !p.redeemable && allow_price_fallback && is_winner {
            debug!(
                "Using price fallback for condition {:?} (cur_price={})",
                p.condition_id, p.cur_price
            );
        }

        let condition_id = format!("{:#x}", p.condition_id);
        if condition_id.trim().is_empty() || condition_id == "0x0000000000000000000000000000000000000000000000000000000000000000" {
            warn!(
                "Skipping redeemable position with zero condition_id (outcome={}, size={})",
                p.outcome, p.size
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

        let token_id = format!("{}", p.asset);
        let payout = p.size;
        let outcome_index = p.outcome_index.max(0) as usize;
        let mut claim_amounts = vec![Decimal::ZERO; outcome_index + 1];
        claim_amounts[outcome_index] = p.size;

        redeemable.push(RedeemablePosition {
            condition_id: condition_id.clone(),
            token_id,
            outcome: p.outcome.clone(),
            outcome_index,
            size: p.size,
            payout,
            claim_amounts,
            neg_risk: p.negative_risk,
        });

        info!(
            "Found redeemable position: {} {} shares, condition={}",
            p.outcome,
            p.size,
            condition_id.chars().take(16).collect::<String>()
        );
    }

    Ok(collapse_positions_by_condition(redeemable))
}
