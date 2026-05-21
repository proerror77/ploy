from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[1]
MIGRATION = ROOT / "migrations" / "042_factor_research_os_registry.sql"


class FactorResearchOsRegistryMigrationTest(unittest.TestCase):
    def test_factor_registry_tables_and_statuses_exist(self) -> None:
        sql = MIGRATION.read_text(encoding="utf-8")
        for table in [
            "factor_registry",
            "factor_evaluations",
            "experiment_trace",
        ]:
            self.assertIn(f"CREATE TABLE IF NOT EXISTS {table}", sql)
        self.assertRegex(
            re.sub(r"\s+", " ", sql),
            r"status TEXT NOT NULL CHECK .*draft.*compiled.*evaluated.*candidate.*dry_run.*approved.*production.*deprecated",
        )
        self.assertIn("dsl_hash TEXT NOT NULL", sql)
        self.assertIn("ast_json JSONB NOT NULL", sql)
        self.assertIn("hash_prev TEXT", sql)
        self.assertIn("hash_current TEXT NOT NULL", sql)

    def test_experiment_trace_is_append_only_by_trigger(self) -> None:
        sql = MIGRATION.read_text(encoding="utf-8")
        self.assertIn("prevent_experiment_trace_update", sql)
        self.assertIn("prevent_experiment_trace_delete", sql)
        self.assertIn("RAISE EXCEPTION 'experiment_trace is append-only'", sql)


if __name__ == "__main__":
    unittest.main()
