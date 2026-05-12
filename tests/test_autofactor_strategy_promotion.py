import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "evaluate_autofactor_strategy_promotion.py"


READY_GATE = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=true stake_usd=15.00 min_entry_fill_rate=0.0500 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=true include_deribit=true data_quality_mode=event_complete event_complete_events=2488 event_complete_rows=51989 replay_parity_ready=true
gate,passed,evidence
data_quality,true,mode=event_complete event_complete_events=2488 event_complete_rows=51989
recorded_replay_parity,true,blocking_flags=<none>
"""

BLOCKED_GATE = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=false stake_usd=15.00 min_entry_fill_rate=0.0500 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=true include_deribit=true data_quality_mode=event_complete event_complete_events=0 event_complete_rows=0 replay_parity_ready=false
gate,passed,evidence
data_quality,false,mode=event_complete event_complete_events=0 event_complete_rows=0
recorded_replay_parity,false,missing replay parity
"""

MODEL_BLOCKED_GATE = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=false stake_usd=15.00 min_entry_fill_rate=0.0500 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=true include_deribit=true data_quality_mode=event_complete event_complete_events=292 event_complete_rows=468 replay_parity_ready=true
gate,passed,evidence
data_quality,true,mode=event_complete event_complete_events=292 event_complete_rows=468
deribit_vol_surface,true,require_deribit=true include_deribit=true
recorded_replay_parity,true,blocking_flags=<none>
symbol_holdout,false,no non-naive model passes all symbol holdouts
walk_forward_oos,false,no non-naive model has non-empty OOS windows with positive_window_ratio >= 0.60
"""

AUTOFACTOR_REPORT = """
# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable repricing PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,monotonicity,top_bucket_avg_label,top_bucket_positive_label_rate,complexity
1,spread_adjusted_external_move,full_depth_settlement_executable_pnl,candidate,passed,63255,0.112526,0.058830,54,0.948193,0.8889,0.5000,1.902254,0.6656,5
2,settlement_fair_edge,full_depth_settlement_executable_pnl,reject,nonpositive_rank_ic,57777,-0.208894,-0.007501,45,-2.321308,0.0222,0.5000,-2.981937,0.5035,1

# AutoFactor target=full_depth_reprice_pnl_10s
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable repricing PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,monotonicity,top_bucket_avg_label,top_bucket_positive_label_rate,complexity
1,spread_adjusted_external_move,full_depth_reprice_pnl_10s,candidate,passed,49789,0.327294,0.123771,40,3.301626,1.0000,0.5000,1.625037,0.5354,5
"""

AUTOFACTOR_SETTLEMENT_AUTO_REPORT = """
# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_avg_label,top_bucket_positive_label_rate,complexity
1,auto_settlement_conservative_settlement_edge,full_depth_settlement_executable_pnl,candidate,passed,49831,0.110842,0.150273,43,1.064178,0.9535,6,0.8333,1.0000,2.666226,0.6836,1
2,auto_settlement_conservative_settlement_edge_x_near_strike,full_depth_settlement_executable_pnl,candidate,passed,49831,0.107526,0.157904,43,1.044245,0.9302,6,0.8333,1.0000,2.631575,0.6790,3
3,auto_settlement_conservative_settlement_edge_x_entry_price_quality,full_depth_settlement_executable_pnl,candidate,passed,49831,0.106112,0.155901,43,1.031294,0.9070,6,0.8333,1.0000,2.512771,0.6712,3
4,auto_settlement_full_depth_settlement_edge_x_external_pressure,full_depth_settlement_executable_pnl,reject,nonpositive_rank_ic,57777,-0.021993,0.069182,45,0.217558,0.4667,6,0.5000,0.5000,-0.301991,0.4785,3
"""

CORE_SUITE_REPORT = f"""
# Snapshot
snapshot_schema=1
snapshot_data_audit_status=critical

=== Factor Walk-Forward V2 ===
factor,target,decision

=== Full Depth Execution Matrix ===
candidate,bucket,count

{READY_GATE}

{AUTOFACTOR_SETTLEMENT_AUTO_REPORT}
"""


class AutoFactorStrategyPromotionTests(unittest.TestCase):
    def run_script(self, report, *extra_args, check=True):
        with tempfile.TemporaryDirectory() as tmp:
            report_path = Path(tmp) / "report.txt"
            output_json = Path(tmp) / "promotion.json"
            output_registry = Path(tmp) / "registry.json"
            output_handoff = Path(tmp) / "handoff.json"
            output_handoff_md = Path(tmp) / "handoff.md"
            report_path.write_text(report, encoding="utf-8")
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--report",
                    str(report_path),
                    "--output-json",
                    str(output_json),
                    "--output-registry-json",
                    str(output_registry),
                    "--output-handoff-json",
                    str(output_handoff),
                    "--output-handoff-md",
                    str(output_handoff_md),
                    *extra_args,
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=check,
            )
            payload = json.loads(output_json.read_text(encoding="utf-8"))
            registry = json.loads(output_registry.read_text(encoding="utf-8"))
            handoff = json.loads(output_handoff.read_text(encoding="utf-8"))
            handoff_md = output_handoff_md.read_text(encoding="utf-8")
            return result, payload, registry, handoff, handoff_md

    def test_blocks_candidate_when_runtime_profile_is_not_required_profile(self):
        _, payload, registry, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_REPORT
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(registry["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        self.assertEqual(handoff["recommended_action"], "do_not_promote")
        self.assertIn("No dry-run handoff issue or config", handoff_md)
        spread = next(
            item
            for item in payload["evaluated_factors"]
            if item["factor"]["name"] == "spread_adjusted_external_move"
            and item["factor"]["target"] == "full_depth_settlement_executable_pnl"
        )
        self.assertIn(
            "runtime_profile_mismatch:repricing_momentum!=settlement_probability",
            spread["blockers"],
        )
        fair_edge = next(
            item
            for item in payload["evaluated_factors"]
            if item["factor"]["name"] == "settlement_fair_edge"
            and item["factor"]["target"] == "full_depth_settlement_executable_pnl"
        )
        self.assertIn("autofactor_not_candidate:reject:nonpositive_rank_ic", fair_edge["blockers"])
        self.assertIn("empty_runtime_strategy_profile", fair_edge["blockers"])

    def test_qualifies_settlement_native_auto_factors_when_gate_is_ready(self):
        _, payload, registry, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(handoff["blocked_factor_count"], 1)
        self.assertEqual(len(handoff["strategies"]), 3)
        self.assertEqual(
            handoff["strategies"][0]["runtime_score"],
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
        )
        self.assertEqual(
            handoff["strategies"][0]["strategy_profile"],
            "settlement_probability",
        )
        self.assertIn(
            "autofactor_formula:auto_settlement_conservative_settlement_edge_x_near_strike",
            handoff_md,
        )
        self.assertIn(
            "autofactor_formula:auto_settlement_conservative_settlement_edge_x_entry_price_quality",
            handoff_md,
        )
        self.assertIn("top bucket avg label", handoff_md)
        self.assertNotIn("top bucket pnl", handoff_md.lower())
        rejected = next(
            item
            for item in payload["evaluated_factors"]
            if item["factor"]["name"] == "auto_settlement_full_depth_settlement_edge_x_external_pressure"
        )
        self.assertFalse(rejected["qualified"])
        self.assertIn("autofactor_not_candidate:reject:nonpositive_rank_ic", rejected["blockers"])

    def test_core_suite_report_is_sufficient_for_handoff_evaluation(self):
        self.assertNotIn("=== Fillability Review V1 Data Health ===", CORE_SUITE_REPORT)
        self.assertNotIn("=== Liquidity Gate V1 ===", CORE_SUITE_REPORT)
        self.assertNotIn("=== Meta Label Walk-Forward V1 ===", CORE_SUITE_REPORT)

        _, payload, registry, handoff, handoff_md = self.run_script(CORE_SUITE_REPORT)

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(
            handoff["strategies"][0]["runtime_score"],
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
        )
        self.assertIn("Promote AutoFactor strategy handoff to dry-run", handoff_md)

    def test_qualifies_when_allowed_target_and_runtime_profile_match(self):
        _, payload, registry, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_REPORT,
            "--allowed-target",
            "full_depth_reprice_pnl_10s",
            "--required-strategy-profile",
            "repricing_momentum",
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(handoff["recommended_action"], "create_dry_run_handoff")
        self.assertEqual(
            handoff["strategies"][0]["runtime_score"],
            "spread_adjusted_external_move_score",
        )
        self.assertIn("Promote AutoFactor strategy handoff to dry-run", handoff_md)
        self.assertIn("spread_adjusted_external_move_score", handoff_md)
        self.assertEqual(
            payload["qualified_strategies"][0]["factor"]["name"],
            "spread_adjusted_external_move",
        )

    def test_blocks_when_promotion_gate_is_not_ready(self):
        result, payload, _, handoff, _ = self.run_script(
            BLOCKED_GATE + AUTOFACTOR_REPORT,
            "--allowed-target",
            "full_depth_reprice_pnl_10s",
            "--required-strategy-profile",
            "repricing_momentum",
            "--fail-if-blocked",
            check=False,
        )

        self.assertEqual(result.returncode, 3)
        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        self.assertIn("promotion_gate_not_ready", payload["evaluated_factors"][0]["blockers"])

    def test_formula_candidates_use_formula_specific_model_gates(self):
        _, payload, _, handoff, _ = self.run_script(
            MODEL_BLOCKED_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(len(handoff["strategies"]), 3)
        first = payload["qualified_strategies"][0]["factor"]
        self.assertEqual(first["symbol_count"], 6)
        self.assertEqual(first["symbol_positive_ratio"], 0.8333)

    def test_formula_candidates_require_symbol_stability(self):
        weak_symbol_report = AUTOFACTOR_SETTLEMENT_AUTO_REPORT.replace(
            ",6,0.8333,",
            ",6,0.5000,",
        )

        _, payload, _, handoff, _ = self.run_script(
            MODEL_BLOCKED_GATE + weak_symbol_report
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        first = payload["evaluated_factors"][0]
        self.assertIn("formula_symbol_holdout_unstable:0.5000<0.60", first["blockers"])


if __name__ == "__main__":
    unittest.main()
