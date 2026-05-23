from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
EXAMPLE = ROOT / "crates" / "ploy-research" / "examples" / "persist_research_trace.rs"
TRACE_PLAN = ROOT / "crates" / "ploy-research" / "examples" / "research_trace_plan.rs"
CARGO = ROOT / "crates" / "ploy-research" / "Cargo.toml"
RUNBOOK = ROOT / "docs" / "runbooks" / "strategy-research-cicd.md"
HOSTED_WALK = ROOT / ".github" / "workflows" / "factor-walk-forward-v2-hosted-artifact.yml"
TANGO_DEPLOY = ROOT / ".github" / "workflows" / "deploy-tango-1-1.yml"
LEGACY_FACTOR_REVIEW = ROOT / ".github" / "workflows" / "factor-review-v2.yml"
LEGACY_WALK_FORWARD = ROOT / ".github" / "workflows" / "factor-walk-forward-v2.yml"
RESEARCH_SNAPSHOT = ROOT / "crates" / "ploy-research" / "src" / "research_snapshot.rs"


class PersistResearchTraceContractTest(unittest.TestCase):
    def test_example_is_registered_with_db_feature(self) -> None:
        cargo = CARGO.read_text(encoding="utf-8")
        self.assertIn('name = "persist_research_trace"', cargo)
        self.assertIn('path = "examples/persist_research_trace.rs"', cargo)
        self.assertIn('name = "research_trace_plan"', cargo)
        self.assertIn('path = "examples/research_trace_plan.rs"', cargo)
        self.assertIn('required-features = ["db"]', cargo)

    def test_writer_persists_all_research_os_tables(self) -> None:
        source = EXAMPLE.read_text(encoding="utf-8")
        for table in [
            "research_dataset_snapshots",
            "factor_registry",
            "factor_evaluations",
            "experiment_trace",
        ]:
            self.assertIn(table, source)
        self.assertIn("ON CONFLICT (data_snapshot_id) DO UPDATE", source)
        self.assertIn("ON CONFLICT (dsl_hash, target, horizon) DO UPDATE", source)
        self.assertIn('const EVIDENCE_STAGE: &str = "factor_attribution"', source)
        self.assertIn('const EVALUATION_KIND: &str = "alpha_search_preview"', source)
        self.assertIn("SELECT eval_id::text", source)
        self.assertIn("trace_hash(", source)
        self.assertIn('"promotion_registry" | "autofactor_promotion" | "strategy_handoff" => "walk_forward"', source)

    def test_promotion_mapping_is_fail_closed(self) -> None:
        source = EXAMPLE.read_text(encoding="utf-8")
        self.assertIn('promotion_decision: "continue"', source)
        self.assertIn('promotion_status: "candidate"', source)
        self.assertNotIn('promotion_decision: "dry_run_candidate"', source)
        self.assertNotIn('promotion_status: "dry_run"', source)
        self.assertNotIn('promotion_decision: "live_candidate"', source)
        self.assertIn('promotion_decision: "blocked"', source)
        self.assertIn('promotion_status: "blocked"', source)

    def test_runbook_documents_writer_as_trace_not_promotion(self) -> None:
        runbook = RUNBOOK.read_text(encoding="utf-8")
        self.assertIn("persist_research_trace", runbook)
        self.assertIn("does not create dry-run", runbook)
        self.assertIn("or live promotion evidence", runbook)
        self.assertIn("Durable research trace", runbook)

    def test_snapshot_surfaces_use_canonical_gate_categories(self) -> None:
        source = RESEARCH_SNAPSHOT.read_text(encoding="utf-8")
        self.assertIn("gate_category", source)
        for category in [
            "required_for_prediction",
            "required_for_execution",
            "optional_context",
        ]:
            self.assertIn(category, source)

    def test_hosted_walk_forward_can_persist_trace_when_explicitly_enabled(self) -> None:
        workflow = HOSTED_WALK.read_text(encoding="utf-8")
        required_snippets = [
            '"persist_research_trace": "false"',
            "WALK_PERSIST_RESEARCH_TRACE",
            "--example persist_research_trace",
            "Persist Research OS trace",
            "RESEARCH_OS_DATABASE_URL",
            "PLOY_DATABASE_URL",
            "PLOY_RESEARCH_DATABASE_URL",
            "PLOY_DB_URL",
            "Research trace DB host ${db_host} is private",
            "tango-1-1:${remote_dir}/input.tar",
            "/opt/ploy/bin/persist-research-trace",
            'DATABASE_URL="${db_url}" "${persist_bin}"',
            "--alpha-search-dir artifacts/factor-walk-forward-v2/alpha-search",
            "--registry-json artifacts/factor-walk-forward-v2/autofactor-factor-registry.json",
            "--promotion-json artifacts/factor-walk-forward-v2/autofactor-strategy-promotion.json",
            "--handoff-json artifacts/factor-walk-forward-v2/autofactor-strategy-handoff.json",
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, workflow)

    def test_tango_deploy_installs_persist_trace_binary(self) -> None:
        workflow = TANGO_DEPLOY.read_text(encoding="utf-8")
        deploy_helper = (ROOT / "scripts" / "ci" / "deploy_tango_cloud_assist.py").read_text(
            encoding="utf-8"
        )
        for source in [workflow, deploy_helper]:
            self.assertIn("persist-research-trace", source)
            self.assertIn("research-trace-plan", source)
        self.assertIn("--example persist_research_trace", workflow)
        self.assertIn("--example research_trace_plan", workflow)

    def test_research_trace_plan_reads_durable_tables(self) -> None:
        source = TRACE_PLAN.read_text(encoding="utf-8")
        for table in [
            "experiment_trace",
            "factor_registry",
            "factor_evaluations",
            "research_dataset_snapshots",
        ]:
            self.assertIn(table, source)
        self.assertIn("plan_next_research", source)
        self.assertIn('"research_trace_plan.v1"', source)
        self.assertIn("GROUP BY run_id", source)
        self.assertIn("source_surface_blockers", source)
        self.assertIn("missing_blocks_promotion", source)

    def test_legacy_db_workflows_are_debug_only(self) -> None:
        for path in [LEGACY_FACTOR_REVIEW, LEGACY_WALK_FORWARD]:
            workflow = path.read_text(encoding="utf-8")
            self.assertIn("Reject legacy DB", workflow)
            self.assertIn("direct-DB branch is disabled by default", workflow)
            self.assertIn("allow_direct_db_debug=true", workflow)
            self.assertIn("snapshot_run_id", workflow)


if __name__ == "__main__":
    unittest.main()
