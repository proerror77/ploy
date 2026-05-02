import csv
import json
import tempfile
import unittest
from pathlib import Path

from scripts.analyze_edge_matrix_artifact import build_report


def write_csv(path: Path, rows: list[dict[str, object]]) -> None:
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        for row in rows:
            writer.writerow(row)


class EdgeMatrixArtifactAnalysisTests(unittest.TestCase):
    def test_min_trades_below_strict_threshold_stays_diagnostic_only(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "edge-matrix-summary.json").write_text(
                json.dumps(
                    {
                        "snapshot_hash": "hash",
                        "source_rows": 10,
                        "v2_rows": 20,
                        "train_start": "2026-04-24T00:00:00Z",
                        "train_end": "2026-04-27T00:00:00Z",
                        "val_start": "2026-04-27T00:00:00Z",
                        "val_end": "2026-04-28T00:00:00Z",
                        "symbols": ["BTCUSDT"],
                        "hypothesis_count": 1,
                        "min_trades": 20,
                    }
                ),
                encoding="utf-8",
            )
            write_csv(
                root / "strategy-matrix-results.csv",
                [
                    {
                        "hypothesis": "inverted_entry_only_pm_none_wide_ev0.05",
                        "split": "validation",
                        "direction_mode": "Inverted",
                        "fill_mode": "EntryOnlyExecutable",
                        "pm_mode": "None",
                        "trades": 21,
                        "selected": 21,
                        "trade_attempts_after_throttle": 21,
                        "rejected_duplicate": 0,
                        "rejected_cooldown": 0,
                        "rejected_non_executable": 0,
                        "net_pnl": 100,
                        "selection_rate": 1,
                        "fill_rate": 1,
                        "duplicate_rate": 0,
                        "cooldown_rate": 0,
                        "non_executable_rate": 0,
                        "win_rate": 1,
                        "avg_realized_return_per_stake": 0.3,
                        "avg_expected_value_per_stake": 0.4,
                        "expectancy_calibration_gap": 0.1,
                        "positive_day_rate": 1,
                        "positive_symbol_rate": 1,
                        "underpowered": "false",
                        "deployable_candidate": "true",
                    }
                ],
            )
            write_csv(
                root / "selection-audit.csv",
                [
                    {
                        "hypothesis": "inverted_entry_only_pm_none_wide_ev0.05",
                        "split": "validation",
                        "direction_mode": "Inverted",
                        "fill_mode": "EntryOnlyExecutable",
                        "pm_mode": "None",
                        "event_id": "event",
                        "symbol": "BTCUSDT",
                        "tick_ts": "2026-04-27T00:00:00Z",
                        "side": "UP",
                        "raw_side_model_prob": 0.4,
                        "transformed_probability": 0.6,
                        "calibrated_probability": 0.56,
                        "entry_ask": 0.5,
                        "expected_value_per_stake": 0.4,
                        "side_distance_over_sigma": 1,
                        "settlement_win": 1,
                        "executable_pnl_15u": 10,
                        "selection_status": "accepted",
                    }
                ],
            )
            write_csv(
                root / "gate-attrition.csv",
                [
                    {
                        "hypothesis": "inverted_entry_only_pm_none_wide_ev0.05",
                        "split": "validation",
                        "gate_index": 0,
                        "gate": "base",
                        "rows": 1,
                        "event_sides": 1,
                        "executable_pnl_rows": 1,
                        "full_depth_pnl_rows": 1,
                        "entry_fill_rate": 1,
                        "roundtrip_fill_rate": 1,
                        "total_executable_pnl": 10,
                        "avg_executable_pnl": 10,
                    }
                ],
            )

            report = build_report(root, "run")

        self.assertEqual(report["run_threshold_validation_candidates"], 1)
        self.assertEqual(report["strict_deployable_validation_candidates"], 0)
        self.assertEqual(report["decision"], "diagnostic-only-continue-research")
        self.assertIn("sample_power:21<80", report["top_validation_rows"][0]["blockers"])


if __name__ == "__main__":
    unittest.main()
