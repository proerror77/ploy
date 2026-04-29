#!/usr/bin/env python3
"""Validate the deployed dry-run report API payload contract."""

from __future__ import annotations

import json
import sys


REQUIRED = {
    "metrics.sharpe_basis": "closed_trade_pnl_sqrt_n",
    "metrics.daily_sharpe_basis": "daily_net_pnl_sqrt_365",
    "execution_diagnostics.basis": "strategy_runtime_orders",
}


def nested_value(payload: dict, path: str):
    value = payload
    for part in path.split("."):
        if part == "strategies[0]":
            strategies = value.get("strategies") or []
            value = strategies[0] if strategies else {}
            continue
        if not isinstance(value, dict):
            return None
        value = value.get(part)
    return value


def main() -> int:
    payload = json.load(sys.stdin)
    failures = {
        path: {"expected": expected, "actual": nested_value(payload, path)}
        for path, expected in REQUIRED.items()
        if nested_value(payload, path) != expected
    }

    strategy_diagnostics = [
        (strategy.get("deployment_id"), nested_value(strategy, "execution_diagnostics.basis"))
        for strategy in payload.get("strategies") or []
        if strategy.get("execution_diagnostics") is not None
    ]
    failures.update(
        {
            f"strategies[{deployment_id}].execution_diagnostics.basis": {
                "expected": "strategy_runtime_orders",
                "actual": basis,
            }
            for deployment_id, basis in strategy_diagnostics
            if basis != "strategy_runtime_orders"
        }
    )
    if failures:
        print(json.dumps({"dry_run_report_contract_failures": failures}, sort_keys=True), file=sys.stderr)
        return 1

    print(
        json.dumps(
            {
                "dry_run_report_contract": "ok",
                "total_trades": nested_value(payload, "summary.total_trades"),
                "sharpe_basis": nested_value(payload, "metrics.sharpe_basis"),
                "execution_diagnostics": nested_value(payload, "execution_diagnostics.basis"),
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
