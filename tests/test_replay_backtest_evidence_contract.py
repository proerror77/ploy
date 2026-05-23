import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]


class ReplayBacktestEvidenceContractTest(unittest.TestCase):
    def test_run_backtest_artifact_names_promotion_blockers(self):
        source = (ROOT / "crates/ploy-strategy-bundles/examples/run_backtest.rs").read_text()

        required_snippets = [
            '"evidence_stage"',
            '"promotion_ready": false',
            '"promotion_decision": "pending replay/dry-run parity review"',
            '"source"',
            '"data_surfaces"',
            '"blocking_risk_flags"',
            '"advisory_flags"',
            '"missing_full_depth_clob_fillability"',
            '"missing_replay_dryrun_parity"',
            '"missing_runtime_scorer_parity"',
            '"parquet_stream_uses_quote_ticks_not_full_clob_lake"',
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, source)

    def test_backtest_workflow_surfaces_gate_fields(self):
        workflow = (ROOT / ".github/workflows/backtest.yml").read_text()

        required_snippets = [
            "not full-depth lake replay",
            "legacy quote-tick Parquet",
            "evidence_stage",
            "promotion_ready",
            "promotion_decision",
            "blocking_risk_flags",
            "Blocking promotion flags",
            "Assert backtest remains non-promotion evidence",
            "backtest promotion_ready must remain false",
            "parity:blocked",
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, workflow)

    def test_research_runbook_preserves_backtest_caveat(self):
        runbook = (ROOT / "docs/runbooks/strategy-research-cicd.md").read_text()

        required_snippets = [
            "not as dry-run or live promotion evidence",
            "/opt/ploy/data/lake/orderbook_snapshots",
            "full-depth CLOB fillability",
            "`evidence_stage`, `promotion_ready`, `blocking_risk_flags`, and",
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, runbook)


if __name__ == "__main__":
    unittest.main()
