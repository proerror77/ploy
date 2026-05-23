from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[1]
MIGRATION_042 = ROOT / "migrations" / "042_factor_research_os_registry.sql"
MIGRATION_043 = ROOT / "migrations" / "043_research_os_trace_constraints.sql"


def research_os_sql() -> str:
    return "\n".join(
        [
            MIGRATION_042.read_text(encoding="utf-8"),
            MIGRATION_043.read_text(encoding="utf-8"),
        ]
    )


class FactorResearchOsRegistryMigrationTest(unittest.TestCase):
    def test_factor_registry_tables_and_statuses_exist(self) -> None:
        sql = research_os_sql()
        for table in [
            "factor_registry",
            "research_dataset_snapshots",
            "factor_evaluations",
            "experiment_trace",
        ]:
            self.assertIn(f"CREATE TABLE IF NOT EXISTS {table}", sql)
        self.assertRegex(
            re.sub(r"\s+", " ", sql),
            r"status TEXT NOT NULL CHECK .*draft.*compiled.*evaluated.*candidate.*dry_run.*approved.*production.*deprecated",
        )
        self.assertIn("dsl_hash TEXT NOT NULL", sql)
        self.assertIn("idx_factor_registry_dsl_target_horizon", sql)
        self.assertIn("ON factor_registry(dsl_hash, target, horizon)", sql)
        self.assertIn("ast_json JSONB NOT NULL", sql)
        self.assertIn("runtime_contract JSONB NOT NULL DEFAULT '{}'::jsonb", sql)
        self.assertIn("source_surfaces_json JSONB NOT NULL DEFAULT '[]'::jsonb", sql)
        self.assertIn("input_artifacts_json JSONB NOT NULL DEFAULT '[]'::jsonb", sql)
        self.assertIn("dataset_start_ts TIMESTAMPTZ", sql)
        self.assertIn("dataset_end_ts TIMESTAMPTZ", sql)
        self.assertIn("evidence_stage TEXT NOT NULL DEFAULT 'factor_attribution'", sql)
        self.assertIn("evaluation_kind TEXT NOT NULL DEFAULT 'alpha_search_preview'", sql)
        self.assertIn("candidate_replay_id TEXT", sql)
        self.assertIn("promotion_decision TEXT NOT NULL DEFAULT 'not_evaluated'", sql)
        self.assertIn("promotion_status TEXT NOT NULL DEFAULT 'blocked'", sql)
        self.assertIn("blockers_json JSONB NOT NULL DEFAULT '[]'::jsonb", sql)
        self.assertIn("artifact_kind TEXT NOT NULL DEFAULT 'artifact'", sql)
        self.assertIn("hash_prev TEXT", sql)
        self.assertIn("hash_current TEXT NOT NULL", sql)

    def test_experiment_trace_is_append_only_by_trigger(self) -> None:
        sql = research_os_sql()
        self.assertIn("prevent_experiment_trace_update", sql)
        self.assertIn("prevent_experiment_trace_delete", sql)
        self.assertIn("RAISE EXCEPTION 'experiment_trace is append-only'", sql)

    def test_evaluation_trace_constraints_exist(self) -> None:
        sql = research_os_sql()
        required_snippets = [
            "fk_factor_evaluations_data_snapshot",
            "REFERENCES research_dataset_snapshots(data_snapshot_id)",
            "chk_factor_evaluations_dataset_window",
            "dataset_start_ts IS NOT NULL",
            "dataset_end_ts IS NOT NULL",
            "dataset_start_ts < dataset_end_ts",
            "chk_factor_evaluations_evidence_stage",
            "chk_factor_evaluations_evaluation_kind",
            "chk_factor_evaluations_promotion_decision",
            "chk_factor_evaluations_promotion_status",
            "chk_factor_evaluations_blockers_array",
            "jsonb_typeof(blockers_json) = 'array'",
            "chk_experiment_trace_evidence_stage",
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, sql)

    def test_tango_deploy_includes_research_os_migrations(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "deploy-tango-1-1.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("042_factor_research_os_registry.sql", workflow)
        self.assertIn("043_research_os_trace_constraints.sql", workflow)


if __name__ == "__main__":
    unittest.main()
