//! Canonical label/horizon contracts for Research OS evidence.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelAccountingMode {
    EventLevelOneDecision,
    RowLevelDiagnostic,
}

impl LabelAccountingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            LabelAccountingMode::EventLevelOneDecision => "event_level_one_decision",
            LabelAccountingMode::RowLevelDiagnostic => "row_level_diagnostic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyLane {
    SettlementProbability,
    Repricing,
}

impl StrategyLane {
    pub fn as_str(self) -> &'static str {
        match self {
            StrategyLane::SettlementProbability => "settlement_probability",
            StrategyLane::Repricing => "repricing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LabelHorizonContract {
    pub horizon_id: &'static str,
    pub label_family: &'static str,
    pub strategy_lane: StrategyLane,
    pub accounting_mode: LabelAccountingMode,
    pub promotion_stage: &'static str,
    pub target_names: &'static [&'static str],
}

impl LabelHorizonContract {
    pub fn event_level(self) -> bool {
        self.accounting_mode == LabelAccountingMode::EventLevelOneDecision
    }

    pub fn diagnostic_only(self) -> bool {
        self.accounting_mode == LabelAccountingMode::RowLevelDiagnostic
            && self.strategy_lane == StrategyLane::Repricing
    }
}

pub const PM5D_SETTLEMENT: LabelHorizonContract = LabelHorizonContract {
    horizon_id: "pm5d_settlement",
    label_family: "settlement",
    strategy_lane: StrategyLane::SettlementProbability,
    accounting_mode: LabelAccountingMode::EventLevelOneDecision,
    promotion_stage: "executable_replay_required",
    target_names: &[
        "settlement_win",
        "settlement_executable_pnl",
        "full_depth_settlement_executable_pnl",
        "tradeable_full_depth_settlement_pnl",
    ],
};

pub const REPRICING_30S: LabelHorizonContract = LabelHorizonContract {
    horizon_id: "repricing_30s",
    label_family: "repricing",
    strategy_lane: StrategyLane::Repricing,
    accounting_mode: LabelAccountingMode::RowLevelDiagnostic,
    promotion_stage: "diagnostic_only_until_repricing_runtime_lane",
    target_names: &[
        "reprice_pnl_30s",
        "full_depth_reprice_pnl_30s",
        "reprice_bid_change_30s",
        "abs_reprice_bid_change_30s",
    ],
};

pub const REPRICING_60S: LabelHorizonContract = LabelHorizonContract {
    horizon_id: "repricing_60s",
    label_family: "repricing",
    strategy_lane: StrategyLane::Repricing,
    accounting_mode: LabelAccountingMode::RowLevelDiagnostic,
    promotion_stage: "diagnostic_only_until_repricing_runtime_lane",
    target_names: &[
        "reprice_pnl_60s",
        "full_depth_reprice_pnl_60s",
        "reprice_bid_change_60s",
        "abs_reprice_bid_change_60s",
    ],
};

pub const REPRICING_5M: LabelHorizonContract = LabelHorizonContract {
    horizon_id: "repricing_5m",
    label_family: "repricing",
    strategy_lane: StrategyLane::Repricing,
    accounting_mode: LabelAccountingMode::RowLevelDiagnostic,
    promotion_stage: "diagnostic_only_until_repricing_runtime_lane",
    target_names: &["repricing_5m"],
};

pub const REPRICING_15M: LabelHorizonContract = LabelHorizonContract {
    horizon_id: "repricing_15m",
    label_family: "repricing",
    strategy_lane: StrategyLane::Repricing,
    accounting_mode: LabelAccountingMode::RowLevelDiagnostic,
    promotion_stage: "diagnostic_only_until_repricing_runtime_lane",
    target_names: &["repricing_15m"],
};

const LEGACY_REPRICING_10S: LabelHorizonContract = LabelHorizonContract {
    horizon_id: "repricing_30s",
    label_family: "repricing",
    strategy_lane: StrategyLane::Repricing,
    accounting_mode: LabelAccountingMode::RowLevelDiagnostic,
    promotion_stage: "legacy_sub_30s_diagnostic_only_until_repricing_runtime_lane",
    target_names: &[
        "reprice_pnl_5s",
        "full_depth_reprice_pnl_5s",
        "reprice_bid_change_5s",
        "abs_reprice_bid_change_5s",
        "reprice_pnl_10s",
        "full_depth_reprice_pnl_10s",
        "reprice_bid_change_10s",
        "abs_reprice_bid_change_10s",
    ],
};

pub const LABEL_HORIZON_CONTRACTS: &[LabelHorizonContract] = &[
    PM5D_SETTLEMENT,
    REPRICING_30S,
    REPRICING_60S,
    REPRICING_5M,
    REPRICING_15M,
];

const TARGET_LOOKUP_CONTRACTS: &[LabelHorizonContract] = &[
    PM5D_SETTLEMENT,
    LEGACY_REPRICING_10S,
    REPRICING_30S,
    REPRICING_60S,
    REPRICING_5M,
    REPRICING_15M,
];

pub fn label_contract_for_horizon(horizon_id: &str) -> Option<LabelHorizonContract> {
    LABEL_HORIZON_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.horizon_id == horizon_id)
}

pub fn label_contract_for_target(target_name: &str) -> Option<LabelHorizonContract> {
    TARGET_LOOKUP_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.target_names.contains(&target_name))
}

pub fn target_requires_event_level_accounting(target_name: &str) -> bool {
    label_contract_for_target(target_name)
        .map(LabelHorizonContract::event_level)
        .unwrap_or(false)
}

pub fn target_is_diagnostic_repricing(target_name: &str) -> bool {
    label_contract_for_target(target_name)
        .map(LabelHorizonContract::diagnostic_only)
        .unwrap_or(false)
}

pub fn label_contract_markdown() -> String {
    let mut lines = vec![
        "## Label Horizon Contract".to_string(),
        "".to_string(),
        "| horizon | lane | accounting | promotion stage | targets |".to_string(),
        "| --- | --- | --- | --- | --- |".to_string(),
    ];
    for contract in LABEL_HORIZON_CONTRACTS {
        lines.push(format!(
            "| `{}` | `{}` | `{}` | `{}` | `{}` |",
            contract.horizon_id,
            contract.strategy_lane.as_str(),
            contract.accounting_mode.as_str(),
            contract.promotion_stage,
            contract.target_names.join(", ")
        ));
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_contract_defines_required_horizons() {
        for horizon in [
            "pm5d_settlement",
            "repricing_30s",
            "repricing_60s",
            "repricing_5m",
            "repricing_15m",
        ] {
            assert!(
                label_contract_for_horizon(horizon).is_some(),
                "missing horizon contract {horizon}"
            );
        }
    }

    #[test]
    fn label_contract_enforces_settlement_event_level_accounting() {
        let settlement = label_contract_for_target("full_depth_settlement_executable_pnl").unwrap();
        assert!(settlement.event_level());
        assert_eq!(
            settlement.accounting_mode,
            LabelAccountingMode::EventLevelOneDecision
        );
        assert!(target_requires_event_level_accounting(
            "tradeable_full_depth_settlement_pnl"
        ));
    }

    #[test]
    fn label_contract_keeps_repricing_diagnostic() {
        let repricing = label_contract_for_target("full_depth_reprice_pnl_30s").unwrap();
        assert_eq!(repricing.horizon_id, "repricing_30s");
        assert!(repricing.diagnostic_only());
        assert!(!target_requires_event_level_accounting(
            "full_depth_reprice_pnl_30s"
        ));
        assert!(target_is_diagnostic_repricing("reprice_pnl_10s"));
        assert!(target_is_diagnostic_repricing("full_depth_reprice_pnl_5s"));
        assert_eq!(
            label_contract_for_target("reprice_bid_change_60s")
                .unwrap()
                .horizon_id,
            "repricing_60s"
        );
        assert_eq!(
            label_contract_for_target("settlement_win")
                .unwrap()
                .accounting_mode,
            LabelAccountingMode::EventLevelOneDecision
        );
    }
}
