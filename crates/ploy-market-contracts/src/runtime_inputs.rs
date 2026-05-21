//! Shared runtime-input contract for research/runtime AutoFactor formulas.

use serde::Serialize;

pub const LLM_PREFIX_NOT_SUPPORTED_BY_RUNTIME: &str =
    "unsupported_runtime_formula_semantics:llm_prefix_not_supported_by_runtime";
pub const POLY_LAG_PRESSURE_RUNTIME_INPUT_MISMATCH: &str =
    "unsupported_runtime_formula_semantics:poly_lag_pressure_runtime_input_mismatch";
pub const EXTERNAL_PRESSURE_RUNTIME_INPUT_MISMATCH: &str =
    "unsupported_runtime_formula_semantics:external_pressure_runtime_input_mismatch";
pub const IV_CHANGE_RUNTIME_INPUT_MISSING: &str =
    "unsupported_runtime_formula_semantics:iv_change_runtime_input_missing";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RuntimeInputContract {
    pub name: &'static str,
    pub semantic_family: &'static str,
    pub source_surface: &'static str,
    pub runtime_supported: bool,
    pub research_supported: bool,
    pub blockers: &'static [&'static str],
}

pub const RUNTIME_INPUT_CONTRACTS: &[RuntimeInputContract] = &[
    RuntimeInputContract {
        name: "full_depth_settlement_edge",
        semantic_family: "settlement_edge",
        source_surface: "polymarket_full_depth_and_fair_probability",
        runtime_supported: true,
        research_supported: true,
        blockers: &[],
    },
    RuntimeInputContract {
        name: "conservative_settlement_edge",
        semantic_family: "settlement_edge",
        source_surface: "polymarket_conservative_depth_and_fair_probability",
        runtime_supported: true,
        research_supported: true,
        blockers: &[],
    },
    RuntimeInputContract {
        name: "model_full_depth_settlement_edge",
        semantic_family: "settlement_edge",
        source_surface: "polymarket_full_depth_and_external_model_probability",
        runtime_supported: true,
        research_supported: true,
        blockers: &[],
    },
    RuntimeInputContract {
        name: "model_conservative_settlement_edge",
        semantic_family: "settlement_edge",
        source_surface: "polymarket_conservative_depth_and_external_model_probability",
        runtime_supported: true,
        research_supported: true,
        blockers: &[],
    },
    RuntimeInputContract {
        name: "near_strike_score",
        semantic_family: "event_geometry",
        source_surface: "event_distance_to_strike",
        runtime_supported: true,
        research_supported: true,
        blockers: &[],
    },
    RuntimeInputContract {
        name: "entry_capacity_score",
        semantic_family: "execution_capacity",
        source_surface: "polymarket_full_depth",
        runtime_supported: true,
        research_supported: true,
        blockers: &[],
    },
    RuntimeInputContract {
        name: "entry_price_quality_score",
        semantic_family: "execution_price_quality",
        source_surface: "polymarket_top_of_book",
        runtime_supported: true,
        research_supported: true,
        blockers: &[],
    },
    RuntimeInputContract {
        name: "side_spread",
        semantic_family: "execution_cost",
        source_surface: "polymarket_top_of_book",
        runtime_supported: true,
        research_supported: true,
        blockers: &[],
    },
    RuntimeInputContract {
        name: "cex_return_30s_side",
        semantic_family: "external_price_momentum",
        source_surface: "binance_spot_ticks",
        runtime_supported: true,
        research_supported: true,
        blockers: &[],
    },
    RuntimeInputContract {
        name: "sigma_horizon_pos",
        semantic_family: "event_volatility_amplitude",
        source_surface: "event_volatility_state",
        runtime_supported: true,
        research_supported: true,
        blockers: &[],
    },
    RuntimeInputContract {
        name: "external_move_since_poly_update",
        semantic_family: "external_price_momentum",
        source_surface: "binance_spot_ticks_plus_polymarket_quote_age",
        runtime_supported: true,
        research_supported: true,
        blockers: &[],
    },
    RuntimeInputContract {
        name: "external_pressure",
        semantic_family: "external_microstructure_pressure",
        source_surface: "research_lob_composite",
        runtime_supported: false,
        research_supported: true,
        blockers: &[EXTERNAL_PRESSURE_RUNTIME_INPUT_MISMATCH],
    },
    RuntimeInputContract {
        name: "iv_change_1m",
        semantic_family: "implied_volatility_change",
        source_surface: "deribit_volatility_surface",
        runtime_supported: false,
        research_supported: true,
        blockers: &[IV_CHANGE_RUNTIME_INPUT_MISSING],
    },
    RuntimeInputContract {
        name: "iv_change",
        semantic_family: "implied_volatility_change",
        source_surface: "deribit_volatility_surface",
        runtime_supported: false,
        research_supported: true,
        blockers: &[IV_CHANGE_RUNTIME_INPUT_MISSING],
    },
    RuntimeInputContract {
        name: "poly_lag_pressure",
        semantic_family: "polymarket_staleness_pressure",
        source_surface: "research_composite",
        runtime_supported: false,
        research_supported: true,
        blockers: &[POLY_LAG_PRESSURE_RUNTIME_INPUT_MISMATCH],
    },
];

pub fn runtime_input_contract(name: &str) -> Option<&'static RuntimeInputContract> {
    RUNTIME_INPUT_CONTRACTS
        .iter()
        .find(|contract| contract.name == name)
}

pub fn runtime_input_blockers<'a>(
    input_names: impl IntoIterator<Item = &'a str>,
) -> Vec<&'static str> {
    let mut blockers = Vec::new();
    for input_name in input_names {
        if let Some(contract) = runtime_input_contract(input_name) {
            if !contract.runtime_supported {
                blockers.extend(contract.blockers.iter().copied());
            }
        }
    }
    dedup_blockers(blockers)
}

pub fn autofactor_formula_name_blockers(name: &str) -> Vec<&'static str> {
    let normalized = normalized_autofactor_formula_name(name);
    let mut blockers = Vec::new();
    if normalized.starts_with("llm_") {
        blockers.push(LLM_PREFIX_NOT_SUPPORTED_BY_RUNTIME);
    }
    if normalized.starts_with("poly_lag_pressure") {
        blockers.push(POLY_LAG_PRESSURE_RUNTIME_INPUT_MISMATCH);
    }
    if normalized.contains("external_pressure") {
        blockers.push(EXTERNAL_PRESSURE_RUNTIME_INPUT_MISMATCH);
    }
    if normalized.contains("iv_change") {
        blockers.push(IV_CHANGE_RUNTIME_INPUT_MISSING);
    }
    dedup_blockers(blockers)
}

pub fn normalized_autofactor_formula_name(mut name: &str) -> &str {
    if let Some(stripped) = name.strip_prefix("autofactor_formula:") {
        name = stripped;
    }
    loop {
        if let Some(stripped) = name.strip_prefix("mut2_") {
            name = stripped;
        } else if let Some(stripped) = name.strip_prefix("mut_") {
            name = stripped;
        } else if let Some(stripped) = name.strip_prefix("mcts_") {
            name = stripped;
        } else {
            return name;
        }
    }
}

fn dedup_blockers(mut blockers: Vec<&'static str>) -> Vec<&'static str> {
    blockers.sort_unstable();
    blockers.dedup();
    blockers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_input_contract_blocks_research_only_inputs() {
        let blockers = runtime_input_blockers([
            "model_full_depth_settlement_edge",
            "external_pressure",
            "iv_change_1m",
        ]);

        assert_eq!(
            blockers,
            vec![
                EXTERNAL_PRESSURE_RUNTIME_INPUT_MISMATCH,
                IV_CHANGE_RUNTIME_INPUT_MISSING,
            ]
        );
    }

    #[test]
    fn formula_name_blockers_match_composed_formula_inputs() {
        assert_eq!(
            autofactor_formula_name_blockers(
                "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted",
            ),
            vec![EXTERNAL_PRESSURE_RUNTIME_INPUT_MISMATCH]
        );
        assert_eq!(
            autofactor_formula_name_blockers(
                "autofactor_formula:mut_poly_lag_pressure_spread_adjusted"
            ),
            vec![POLY_LAG_PRESSURE_RUNTIME_INPUT_MISMATCH]
        );
        assert!(autofactor_formula_name_blockers(
            "autofactor_formula:mut_spread_adjusted_external_move_full_depth_entry_gate"
        )
        .is_empty());
    }
}
