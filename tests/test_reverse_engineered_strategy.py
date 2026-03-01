import unittest

from scripts.reverse_engineered_strategy_dry_run import (
    ProfileSnapshot,
    TradeEvent,
    infer_strategy_params,
    run_reverse_strategy_dry_run,
)


class ReverseEngineeredStrategyTests(unittest.TestCase):
    def test_infer_bias_from_positions(self) -> None:
        snapshot = ProfileSnapshot(
            address="0xabc",
            activity=[],
            positions=[
                {"eventSlug": "e1", "outcome": "Down", "currentValue": 80, "curPrice": 0.8, "size": 100},
                {"eventSlug": "e2", "outcome": "Up", "currentValue": 20, "curPrice": 0.2, "size": 100},
            ],
        )
        params = infer_strategy_params(
            snapshot,
            min_trade_usdc=5.0,
            max_event_usdc=100.0,
            max_total_usdc=500.0,
        )
        self.assertGreaterEqual(params.bias_down_target, 0.75)

    def test_reverse_strategy_runs_without_copying_side(self) -> None:
        # Synthetic tape for one 5m event. Event starts at ts=1000000000 and ends at 1000000300.
        # We keep raw side mostly SELL to ensure our engine does not mirror it.
        event_slug = "btc-updown-5m-1000000000"
        activity = [
            TradeEvent(event_slug, "Down", "SELL", 0.22, 10, 2.2, 1000000260, "Bitcoin Up or Down", "TRADE"),
            TradeEvent(event_slug, "Down", "SELL", 0.24, 10, 2.4, 1000000270, "Bitcoin Up or Down", "TRADE"),
            TradeEvent(event_slug, "Down", "SELL", 0.28, 10, 2.8, 1000000285, "Bitcoin Up or Down", "TRADE"),
            TradeEvent(event_slug, "Down", "SELL", 0.30, 10, 3.0, 1000000290, "Bitcoin Up or Down", "TRADE"),
        ]
        snapshot = ProfileSnapshot(
            address="0xabc",
            activity=activity,
            positions=[{"eventSlug": event_slug, "outcome": "Down", "curPrice": 0.31}],
        )
        params = infer_strategy_params(
            snapshot,
            min_trade_usdc=5.0,
            max_event_usdc=100.0,
            max_total_usdc=500.0,
        )
        result = run_reverse_strategy_dry_run(
            snapshot=snapshot,
            params=params,
            target_assets=("Bitcoin",),
        )
        self.assertGreaterEqual(result.executed_buys, 1)
        self.assertGreaterEqual(result.executed_sells, 1)


if __name__ == "__main__":
    unittest.main()
