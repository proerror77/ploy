import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "run_factor_walk_forward_sweep.py"
WORKFLOW = ROOT / ".github" / "workflows" / "factor-walk-forward-v2-hosted-artifact.yml"


FAKE_REPORT = r'''
print("""=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=true stake_usd=15.00 min_entry_fill_rate=0.0500 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=true include_deribit=true data_quality_mode=event_complete event_complete_events=2488 event_complete_rows=51989 replay_parity_ready=true
gate,passed,evidence
data_quality,true,mode=event_complete event_complete_events=2488 event_complete_rows=51989
recorded_replay_parity,true,blocking_flags=<none>

# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_avg_label,top_bucket_positive_label_rate,complexity
1,auto_settlement_conservative_settlement_edge,full_depth_settlement_executable_pnl,candidate,passed,49831,0.110842,0.150273,43,1.064178,0.9535,6,0.8333,1.0000,2.666226,0.6836,1
""")
'''


class FactorWalkForwardSweepTests(unittest.TestCase):
    def base_args(self, tmp: Path, binary: Path) -> list[str]:
        return [
            sys.executable,
            str(SCRIPT),
            "--binary",
            str(binary),
            "--snapshot-dir",
            str(tmp / "snapshot"),
            "--output-dir",
            str(tmp / "out"),
            "--symbols",
            "BTCUSDT,ETHUSDT",
            "--start-ts",
            "2026-04-24T00:00:00Z",
            "--end-ts",
            "2026-05-01T00:00:00Z",
            "--stake-usd",
            "15",
            "--train-window-days",
            "2",
            "--test-window-days",
            "1",
            "--step-days",
            "1",
            "--lob-sample-secs",
            "30",
            "--observation-sample-secs",
            "30",
            "--max-quote-age-secs",
            "30",
            "--top-n",
            "20",
            "--min-observations",
            "20",
            "--top-quantile",
            "0.2",
            "--factor-name-filter",
            "",
            "--report-suite",
            "core",
            "--data-quality-mode",
            "event_complete",
            "--min-event-complete-events",
            "20",
            "--min-event-complete-rows",
            "40",
            "--cwd",
            str(ROOT),
        ]

    def fake_binary(self, tmp: Path) -> Path:
        binary = tmp / "fake_factor_walk_forward.py"
        binary.write_text(
            "#!/usr/bin/env python3\n"
            "import sys\n"
            f"{FAKE_REPORT}\n",
            encoding="utf-8",
        )
        binary.chmod(0o755)
        return binary

    def test_dry_run_expands_sweep_variants(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = self.fake_binary(tmp)
            sweep_json = json.dumps(
                [
                    {"label": "settlement", "factor_name_filter": "auto_settlement"},
                    {"label": "quantile-10", "top_quantile": 0.1},
                ]
            )
            subprocess.run(
                [*self.base_args(tmp, binary), "--sweep-json", sweep_json, "--dry-run"],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            summary = json.loads((tmp / "out" / "sweep-summary.json").read_text(encoding="utf-8"))

        self.assertTrue(summary["dry_run"])
        self.assertEqual([item["label"] for item in summary["variants"]], ["settlement", "quantile-10"])
        self.assertEqual(summary["variants"][0]["variant"]["factor_name_filter"], "auto_settlement")
        self.assertEqual(summary["variants"][1]["variant"]["top_quantile"], "0.1")

    def test_runs_variants_once_artifacts_are_prepared(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = self.fake_binary(tmp)
            sweep_json = json.dumps(
                [
                    {"label": "base"},
                    {"label": "settlement-only", "factor_name_filter": "auto_settlement"},
                ]
            )
            result = subprocess.run(
                [*self.base_args(tmp, binary), "--sweep-json", sweep_json],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            summary = json.loads((tmp / "out" / "sweep-summary.json").read_text(encoding="utf-8"))
            summary_md = (tmp / "out" / "sweep-summary.md").read_text(encoding="utf-8")

        self.assertEqual(result.returncode, 0)
        self.assertEqual(summary["variant_count"], 2)
        self.assertEqual(summary["completed_count"], 2)
        self.assertEqual(summary["variants"][0]["decision"], "qualified")
        self.assertEqual(summary["variants"][0]["qualified_count"], 1)
        self.assertIn("auto_settlement_conservative_settlement_edge", summary_md)

    def test_hosted_workflow_passes_empty_factor_filter_to_sweep_runner(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")

        self.assertIn("--sweep-json \"${SWEEP_JSON}\"", workflow)
        self.assertIn("--factor-name-filter \"${WALK_FACTOR_NAME_FILTER}\"", workflow)

    def test_alpha_search_prior_and_state_args_pass_through_to_factor_binary(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = tmp / "capture_factor_args.py"
            capture = tmp / "captured_args.json"
            binary.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                f"open({str(capture)!r}, 'w', encoding='utf-8').write(json.dumps(sys.argv[1:]))\n"
                f"{FAKE_REPORT}\n",
                encoding="utf-8",
            )
            binary.chmod(0o755)

            subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--alpha-search-output-dir",
                    "artifacts/alpha",
                    "--alpha-search-plan-json",
                    "artifacts/plan/mcts-expansion-plan.json",
                    "--alpha-search-state-json",
                    "artifacts/plan/mcts-state.json",
                    "--alpha-search-llm-prior-json",
                    "tasks/alpha_search_priors/pm5d_settlement_liquidity_prior_20260512.json",
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            captured = json.loads(capture.read_text(encoding="utf-8"))

        self.assertIn("--alpha-search-output-dir", captured)
        self.assertIn("--alpha-search-plan-json", captured)
        self.assertIn("--alpha-search-state-json", captured)
        self.assertIn("--alpha-search-llm-prior-json", captured)


if __name__ == "__main__":
    unittest.main()
