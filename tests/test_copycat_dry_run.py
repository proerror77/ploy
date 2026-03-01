import unittest

from scripts.copycat_dry_run import (
    TradeEvent,
    extract_profile_snapshot,
    run_dry_run,
)


class CopycatDryRunTests(unittest.TestCase):
    def test_extract_profile_snapshot_from_payload(self) -> None:
        payload = {
            "props": {
                "pageProps": {
                    "proxyAddress": "0xabc",
                    "dehydratedState": {
                        "queries": [
                            {
                                "queryKey": ["profile", "activity", "0xabc", "1"],
                                "state": {
                                    "data": {
                                        "pages": [
                                            [
                                                {
                                                    "type": "TRADE",
                                                    "side": "BUY",
                                                    "price": 0.2,
                                                    "size": 10,
                                                    "usdcSize": 2,
                                                    "outcome": "Down",
                                                    "eventSlug": "btc-updown-5m-x",
                                                    "timestamp": 1,
                                                }
                                            ]
                                        ]
                                    }
                                },
                            },
                            {
                                "queryKey": [
                                    "profile",
                                    "positions",
                                    "0xabc",
                                    "CURRENT",
                                    "DESC",
                                    "",
                                ],
                                "state": {
                                    "data": {
                                        "pages": [
                                            [
                                                {
                                                    "eventSlug": "btc-updown-5m-x",
                                                    "outcome": "Down",
                                                    "curPrice": 0.4,
                                                }
                                            ]
                                        ]
                                    }
                                },
                            },
                        ]
                    },
                }
            }
        }

        snapshot = extract_profile_snapshot(payload)
        self.assertEqual(snapshot.address, "0xabc")
        self.assertEqual(len(snapshot.activity), 1)
        self.assertEqual(len(snapshot.positions), 1)
        self.assertEqual(snapshot.activity[0].side, "BUY")

    def test_run_dry_run_generates_realized_and_unrealized(self) -> None:
        trades = [
            TradeEvent(
                event_slug="btc-updown-5m-x",
                outcome="Down",
                side="BUY",
                price=0.2,
                size=100.0,
                usdc_size=20.0,
                timestamp=1,
                title="Bitcoin Up or Down",
                raw_type="TRADE",
            ),
            TradeEvent(
                event_slug="btc-updown-5m-x",
                outcome="Down",
                side="SELL",
                price=0.3,
                size=40.0,
                usdc_size=12.0,
                timestamp=2,
                title="Bitcoin Up or Down",
                raw_type="TRADE",
            ),
        ]
        mark_prices = {("btc-updown-5m-x", "Down"): 0.5}

        result = run_dry_run(
            activity=trades,
            mark_prices=mark_prices,
            scale=1.0,
            max_event_usdc=1_000.0,
            max_total_usdc=1_000.0,
            target_assets=("Bitcoin",),
        )

        # realized: (0.3 - 0.2) * 40 = 4
        self.assertAlmostEqual(result.realized_pnl, 4.0, places=6)
        # remaining 60 shares unrealized at 0.5: (0.5 - 0.2) * 60 = 18
        self.assertAlmostEqual(result.unrealized_pnl, 18.0, places=6)
        self.assertEqual(result.executed_trades, 2)


if __name__ == "__main__":
    unittest.main()
