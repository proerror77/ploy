from pathlib import Path
import re
import unittest


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "scripts" / "ploy_maintenance.sh"
DB_RETENTION_VARS = [
    "RETENTION_CLOB_TICKS_DAYS",
    "RETENTION_CLOB_BOOK_DAYS",
    "RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS",
    "RETENTION_CLOB_TRADES_DAYS",
    "RETENTION_CLOB_ALERTS_DAYS",
    "RETENTION_BINANCE_TICKS_DAYS",
    "RETENTION_BINANCE_LOB_DAYS",
    "RETENTION_NBA_OBS_DAYS",
    "RETENTION_ORDER_EXEC_DAYS",
]


def _defaults_by_var() -> dict[str, str]:
    script = SCRIPT_PATH.read_text()
    matches = re.findall(
        r'^(RETENTION_[A-Z_]+_DAYS)="\$\{[A-Z0-9_]+:-([0-9]+)\}"$',
        script,
        re.MULTILINE,
    )
    return dict(matches)


class PloyMaintenanceDefaultsTest(unittest.TestCase):
    def test_db_retention_defaults_are_seven_days(self) -> None:
        defaults = _defaults_by_var()
        actual = {var: defaults[var] for var in DB_RETENTION_VARS}
        expected = {var: "7" for var in DB_RETENTION_VARS}
        self.assertEqual(actual, expected)


if __name__ == "__main__":
    unittest.main()
