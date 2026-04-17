use alloy::primitives::Address;
use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use crate::{ClaimerError, RedeemablePosition};

pub(super) struct EligiblePositions {
    pub(super) positions: Vec<RedeemablePosition>,
    pub(super) skipped_small: usize,
    pub(super) skipped_cycle_limited: usize,
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
            skipped_cycle_limited: 0,
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

    let (eligible, skipped_cycle_limited) = limit_positions_for_cycle(
        eligible,
        crate::max_claims_per_cycle(),
        crate::max_claim_payout_per_cycle(),
    );

    if eligible.is_empty() {
        debug!(
            min_claim_size = %min_claim_size,
            skipped_small,
            skipped_cycle_limited,
            "No redeemable positions above min_claim_size"
        );
    }

    Ok(EligiblePositions {
        positions: eligible,
        skipped_small,
        skipped_cycle_limited,
    })
}

pub(crate) fn limit_positions_for_cycle(
    mut positions: Vec<RedeemablePosition>,
    max_claims_per_cycle: Option<usize>,
    max_payout_per_cycle_usdc: Option<Decimal>,
) -> (Vec<RedeemablePosition>, usize) {
    positions.sort_by(|left, right| {
        right
            .payout
            .cmp(&left.payout)
            .then_with(|| left.condition_id.cmp(&right.condition_id))
    });

    let mut selected = Vec::with_capacity(positions.len());
    let mut skipped = 0usize;
    let mut running_payout = Decimal::ZERO;

    for pos in positions {
        if max_claims_per_cycle.is_some_and(|cap| selected.len() >= cap) {
            skipped += 1;
            continue;
        }

        if let Some(cap) = max_payout_per_cycle_usdc {
            let next_total = running_payout + pos.payout;
            if next_total > cap {
                skipped += 1;
                continue;
            }
            running_payout = next_total;
        } else {
            running_payout += pos.payout;
        }

        selected.push(pos);
    }

    (selected, skipped)
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
    let allow_price_fallback = crate::claim_allow_price_fallback();

    let mut redeemable = Vec::new();

    for p in positions {
        if p.size <= Decimal::ZERO {
            continue;
        }

        let is_winner = p.cur_price
            > rust_decimal::Decimal::try_from(0.99_f64).unwrap_or(rust_decimal::Decimal::ONE);

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

        // Only redeem winning positions to avoid burning Builder API quota on
        // losing/dust positions from high-volume 5-min markets.
        // Set CLAIMER_WINNERS_ONLY=false to redeem all redeemable positions.
        if crate::env_flag("CLAIMER_WINNERS_ONLY", true) && !is_winner {
            debug!(
                "Skipping non-winning redeemable position: cur_price={}, condition={:?}",
                p.cur_price, p.condition_id
            );
            continue;
        }

        let condition_id = format!("{:#x}", p.condition_id);
        if condition_id.trim().is_empty()
            || condition_id == "0x0000000000000000000000000000000000000000000000000000000000000000"
        {
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

#[cfg(test)]
mod tests {
    use super::limit_positions_for_cycle;
    use crate::RedeemablePosition;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    fn pos(condition_id: &str, payout: Decimal) -> RedeemablePosition {
        RedeemablePosition {
            condition_id: condition_id.to_string(),
            token_id: format!("token-{condition_id}"),
            outcome: "YES".to_string(),
            outcome_index: 0,
            size: payout,
            payout,
            claim_amounts: vec![payout],
            neg_risk: false,
        }
    }

    #[test]
    fn cycle_limits_prioritize_highest_payouts() {
        let (selected, skipped) = limit_positions_for_cycle(
            vec![
                pos("small", dec!(3)),
                pos("large", dec!(10)),
                pos("mid", dec!(5)),
            ],
            Some(2),
            None,
        );

        assert_eq!(skipped, 1);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].condition_id, "large");
        assert_eq!(selected[1].condition_id, "mid");
    }

    #[test]
    fn cycle_limits_respect_total_payout_cap() {
        let (selected, skipped) = limit_positions_for_cycle(
            vec![
                pos("large", dec!(10)),
                pos("mid", dec!(5)),
                pos("small", dec!(3)),
            ],
            None,
            Some(dec!(13)),
        );

        assert_eq!(skipped, 1);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].condition_id, "large");
        assert_eq!(selected[1].condition_id, "small");
    }
}
