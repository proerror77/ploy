import unittest
from datetime import datetime, timezone

from scripts.backfill_settlements import parse_utc_ts, settlement_rows_from_gamma


class BackfillSettlementsTest(unittest.TestCase):
    def test_parse_utc_ts_returns_timezone_aware_datetime(self) -> None:
        parsed = parse_utc_ts("2026-05-16T00:00:00Z")

        self.assertEqual(datetime(2026, 5, 16, tzinfo=timezone.utc), parsed)
        self.assertIs(timezone.utc, parsed.tzinfo)

    def test_parse_utc_ts_accepts_empty_optional_end(self) -> None:
        self.assertIsNone(parse_utc_ts(""))

    def test_settlement_rows_from_gamma_requires_closed_binary_payouts(self) -> None:
        rows = settlement_rows_from_gamma(
            {
                "closed": True,
                "clobTokenIds": '["up-token", "down-token"]',
                "outcomePrices": '["1", "0"]',
            },
            up_token="up-token",
            down_token="down-token",
        )

        self.assertEqual(
            [("up-token", "winner", 1.0), ("down-token", "loser", 0.0)],
            rows,
        )


if __name__ == "__main__":
    unittest.main()
