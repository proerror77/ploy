#!/usr/bin/env python3
"""
Optuna TPE optimization for pm_5m_directional strategy.

Walk-forward: trains on first N days, validates on last M days.
Per-symbol mode: runs separate backtests per symbol, averages Sharpe.

Usage:
    python scripts/optimize_pm5.py \
      --trials 100 \
      --from 2026-03-18T00:00:00Z --to 2026-03-26T00:00:00Z \
      --train-days 6 --test-days 2 \
      --binary /root/ploy/bin/ploy \
      --db-url postgresql://postgres:postgres@localhost:5432/ploy \
      --per-symbol --output results/pm5_optim.json

Deps: optuna (pip install optuna)
"""

import argparse
import json
import os
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from statistics import mean
from typing import Any

import optuna
from optuna.pruners import MedianPruner
from optuna.samplers import TPESampler

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

SYMBOLS = ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
MIN_TRADES_DEFAULT = 30

# ---------------------------------------------------------------------------
# Search space
# ---------------------------------------------------------------------------


def suggest_params(trial: optuna.Trial) -> dict[str, Any]:
    """Suggest global hyperparameters from the TPE sampler."""
    return {
        # Global
        "min_edge": trial.suggest_float("min_edge", 0.01, 0.12),
        "p_entry": trial.suggest_float("p_entry", 0.55, 0.75),
        "min_abs_z": trial.suggest_float("min_abs_z", 0.15, 0.60),
        "vol_floor": trial.suggest_float("vol_floor", 0.0005, 0.005),
        "cooldown_secs": trial.suggest_int("cooldown_secs", 15, 120),
        "max_concurrent": trial.suggest_int("max_concurrent", 1, 5),
        # L2 weights
        "obi_weight": trial.suggest_float("obi_weight", 0.2, 1.5),
        "flow_weight": trial.suggest_float("flow_weight", 0.3, 2.5),
        "microgap_weight": trial.suggest_float("microgap_weight", 0.1, 1.0),
        "min_obi": trial.suggest_float("min_obi", 0.01, 0.20),
        # No-trade zone
        "no_trade_min": trial.suggest_float("no_trade_min", 0.40, 0.50),
        "no_trade_max": trial.suggest_float("no_trade_max", 0.50, 0.60),
        "no_trade_override_z": trial.suggest_float("no_trade_override_z", 0.5, 1.5),
        "no_trade_override_flow": trial.suggest_float("no_trade_override_flow", 0.2, 0.8),
    }


def suggest_symbol_multipliers(
    trial: optuna.Trial, symbol: str
) -> dict[str, float]:
    """Per-symbol multipliers on global min_edge and p_entry."""
    tag = symbol.lower()
    return {
        "min_edge_mult": trial.suggest_float(f"{tag}_min_edge_mult", 0.7, 1.3),
        "p_entry_mult": trial.suggest_float(f"{tag}_p_entry_mult", 0.9, 1.1),
    }


def apply_symbol_multipliers(
    params: dict[str, Any], mults: dict[str, float]
) -> dict[str, Any]:
    """Return a copy of params with per-symbol multipliers applied."""
    p = dict(params)
    p["min_edge"] = p["min_edge"] * mults["min_edge_mult"]
    p["p_entry"] = p["p_entry"] * mults["p_entry_mult"]
    return p


# ---------------------------------------------------------------------------
# Backtest runner
# ---------------------------------------------------------------------------


def build_cmd(
    binary: str,
    db_url: str,
    symbols: str,
    params: dict[str, Any],
    dt_from: str,
    dt_to: str,
    capital: int = 10000,
) -> list[str]:
    """Build the CLI command list for a single backtest run."""
    cmd = [
        binary,
        "strategy",
        "backtest",
        "directional",
        "--from", dt_from,
        "--to", dt_to,
        "--symbols", symbols,
        "--capital", str(capital),
        "--pm5-auto-trim-window",
        "--json",
    ]
    flag_map = {
        "min_edge": "--pm5-min-edge",
        "p_entry": "--pm5-p-entry",
        "min_abs_z": "--pm5-min-abs-z",
        "obi_weight": "--pm5-obi-weight",
        "flow_weight": "--pm5-flow-weight",
        "microgap_weight": "--pm5-microgap-weight",
        "min_obi": "--pm5-min-obi",
        "no_trade_min": "--pm5-no-trade-min",
        "no_trade_max": "--pm5-no-trade-max",
        "no_trade_override_z": "--pm5-no-trade-override-z",
        "no_trade_override_flow": "--pm5-no-trade-override-flow",
        "vol_floor": "--pm5-vol-floor",
        "cooldown_secs": "--pm5-cooldown-secs",
        "max_concurrent": "--pm5-max-concurrent",
    }
    for key, flag in flag_map.items():
        if key in params:
            cmd.extend([flag, str(params[key])])
    return cmd


def run_backtest(
    binary: str,
    db_url: str,
    symbols: str,
    params: dict[str, Any],
    dt_from: str,
    dt_to: str,
    capital: int = 10000,
    timeout: int = 300,
) -> dict[str, Any] | None:
    """Execute a backtest and return parsed JSON, or None on failure."""
    cmd = build_cmd(binary, db_url, symbols, params, dt_from, dt_to, capital)
    env = os.environ.copy()
    env["DATABASE_URL"] = db_url
    env["PGPASSWORD"] = db_url.split(":")[-1].split("@")[0] if "@" in db_url else ""

    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
        )
    except subprocess.TimeoutExpired:
        print(f"  [WARN] Backtest timed out ({timeout}s): {symbols}")
        return None

    if result.returncode != 0:
        stderr_snippet = result.stderr[:200] if result.stderr else "(no stderr)"
        print(f"  [WARN] Backtest failed (rc={result.returncode}): {stderr_snippet}")
        return None

    # The --json output may be preceded by log lines; extract the last JSON object.
    stdout = result.stdout.strip()
    # Find the last line starting with '{' — that's the JSON output start
    lines = stdout.split("\n")
    json_start_idx = None
    for i in range(len(lines) - 1, -1, -1):
        if lines[i].lstrip().startswith("{"):
            json_start_idx = i
            break
    if json_start_idx is None:
        print("  [WARN] No JSON found in backtest output")
        return None

    json_text = "\n".join(lines[json_start_idx:])
    try:
        return json.loads(json_text)
    except json.JSONDecodeError as e:
        print(f"  [WARN] JSON parse error: {e}")
        return None


# ---------------------------------------------------------------------------
# Date helpers
# ---------------------------------------------------------------------------


def parse_dt(s: str) -> datetime:
    """Parse an ISO-8601 datetime string."""
    s = s.replace("Z", "+00:00")
    return datetime.fromisoformat(s)


def fmt_dt(dt: datetime) -> str:
    """Format datetime as ISO-8601 with Z suffix."""
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


def split_walk_forward(
    dt_from: str, dt_to: str, train_days: int, test_days: int
) -> tuple[str, str, str, str]:
    """Split [from, to) into train and test windows."""
    start = parse_dt(dt_from)
    end = parse_dt(dt_to)
    total = (end - start).days
    if train_days + test_days > total:
        raise ValueError(
            f"train_days ({train_days}) + test_days ({test_days}) "
            f"> total range ({total} days)"
        )
    train_end = start + timedelta(days=train_days)
    test_start = train_end
    test_end = test_start + timedelta(days=test_days)
    return fmt_dt(start), fmt_dt(train_end), fmt_dt(test_start), fmt_dt(test_end)


# ---------------------------------------------------------------------------
# Objective
# ---------------------------------------------------------------------------


def make_objective(
    binary: str,
    db_url: str,
    symbols: list[str],
    train_from: str,
    train_to: str,
    per_symbol: bool,
    min_trades: int,
    capital: int,
    timeout: int,
):
    """Return a closure suitable as an Optuna objective."""

    def objective(trial: optuna.Trial) -> float:
        params = suggest_params(trial)

        # Enforce no_trade_min < no_trade_max
        if params["no_trade_min"] >= params["no_trade_max"]:
            return float("-inf")

        if per_symbol:
            sharpes = []
            total_trades = 0
            for sym in symbols:
                mults = suggest_symbol_multipliers(trial, sym)
                sym_params = apply_symbol_multipliers(params, mults)
                r = run_backtest(
                    binary, db_url, sym, sym_params,
                    train_from, train_to, capital, timeout,
                )
                if r is None:
                    return float("-inf")
                total_trades += r.get("total_trades", 0)
                sharpes.append(r.get("sharpe_ratio", 0.0))
            if total_trades < min_trades:
                print(f"  Trial {trial.number}: rejected ({total_trades} trades < {min_trades})")
                return float("-inf")
            obj = mean(sharpes)
        else:
            sym_str = ",".join(symbols)
            r = run_backtest(
                binary, db_url, sym_str, params,
                train_from, train_to, capital, timeout,
            )
            if r is None:
                return float("-inf")
            total_trades = r.get("total_trades", 0)
            if total_trades < min_trades:
                print(f"  Trial {trial.number}: rejected ({total_trades} trades < {min_trades})")
                return float("-inf")
            obj = r.get("sharpe_ratio", 0.0)

        print(f"  Trial {trial.number}: Sharpe={obj:.4f}, trades={total_trades}")
        return obj

    return objective


# ---------------------------------------------------------------------------
# Validation (test window)
# ---------------------------------------------------------------------------


def validate_best(
    best_params: dict[str, Any],
    binary: str,
    db_url: str,
    symbols: list[str],
    train_from: str,
    train_to: str,
    test_from: str,
    test_to: str,
    per_symbol: bool,
    capital: int,
    timeout: int,
) -> tuple[dict[str, Any] | None, dict[str, Any] | None]:
    """Run best params on both train and test windows. Returns (train_result, test_result)."""

    def _run_window(dt_from: str, dt_to: str) -> dict[str, Any] | None:
        if per_symbol:
            merged: dict[str, Any] = {
                "total_trades": 0,
                "sharpe_ratio": [],
                "total_pnl": 0.0,
                "win_rate": [],
                "profit_factor": [],
                "max_drawdown": 0.0,
            }
            for sym in symbols:
                # Reconstruct per-symbol params from best trial
                sym_params = dict(best_params)
                tag = sym.lower()
                me_mult = best_params.get(f"{tag}_min_edge_mult", 1.0)
                pe_mult = best_params.get(f"{tag}_p_entry_mult", 1.0)
                sym_params["min_edge"] = best_params["min_edge"] * me_mult
                sym_params["p_entry"] = best_params["p_entry"] * pe_mult
                r = run_backtest(binary, db_url, sym, sym_params, dt_from, dt_to, capital, timeout)
                if r is None:
                    return None
                merged["total_trades"] += r.get("total_trades", 0)
                merged["sharpe_ratio"].append(r.get("sharpe_ratio", 0.0))
                merged["total_pnl"] += float(r.get("total_pnl", "0"))
                merged["win_rate"].append(r.get("win_rate", 0.0))
                merged["profit_factor"].append(r.get("profit_factor", 0.0))
                merged["max_drawdown"] = max(
                    merged["max_drawdown"], float(r.get("max_drawdown", "0"))
                )
            merged["sharpe_ratio"] = mean(merged["sharpe_ratio"])
            merged["win_rate"] = mean(merged["win_rate"])
            merged["profit_factor"] = mean(merged["profit_factor"])
            return merged
        else:
            return run_backtest(
                binary, db_url, ",".join(symbols), best_params,
                dt_from, dt_to, capital, timeout,
            )

    train_r = _run_window(train_from, train_to)
    test_r = _run_window(test_from, test_to)
    return train_r, test_r


# ---------------------------------------------------------------------------
# Output formatting
# ---------------------------------------------------------------------------

# PLACEHOLDER_TOML_AND_MAIN


def params_to_toml(params: dict[str, Any], symbols: list[str], per_symbol: bool) -> str:
    """Format best params as a TOML snippet for config/strategies/."""
    lines = [
        "# Auto-generated by optimize_pm5.py",
        "# Paste into [strategy.pm_5m_directional] section",
        "",
        f'min_edge = {params["min_edge"]:.6f}',
        f'p_entry = {params["p_entry"]:.6f}',
        f'min_abs_z = {params["min_abs_z"]:.6f}',
        f'vol_floor = {params["vol_floor"]:.6f}',
        f'cooldown_secs = {params["cooldown_secs"]}',
        f'max_concurrent = {params["max_concurrent"]}',
        "",
        "# L2 weights",
        f'obi_weight = {params["obi_weight"]:.6f}',
        f'flow_weight = {params["flow_weight"]:.6f}',
        f'microgap_weight = {params["microgap_weight"]:.6f}',
        f'min_obi = {params["min_obi"]:.6f}',
        "",
        "# No-trade zone",
        f'no_trade_min = {params["no_trade_min"]:.6f}',
        f'no_trade_max = {params["no_trade_max"]:.6f}',
        f'no_trade_override_z = {params["no_trade_override_z"]:.6f}',
        f'no_trade_override_flow = {params["no_trade_override_flow"]:.6f}',
    ]
    if per_symbol:
        lines.append("")
        lines.append("# Per-symbol multipliers")
        for sym in symbols:
            tag = sym.lower()
            me = params.get(f"{tag}_min_edge_mult", 1.0)
            pe = params.get(f"{tag}_p_entry_mult", 1.0)
            lines.append(f"# {sym}: min_edge_mult={me:.4f}, p_entry_mult={pe:.4f}")
    return "\n".join(lines)


def print_comparison(
    train_r: dict[str, Any] | None,
    test_r: dict[str, Any] | None,
) -> None:
    """Print train vs test comparison table."""
    print("\n" + "=" * 60)
    print("WALK-FORWARD VALIDATION")
    print("=" * 60)
    header = f"{'Metric':<22} {'Train':>12} {'Test':>12} {'Delta':>10}"
    print(header)
    print("-" * 60)

    if train_r is None or test_r is None:
        print("  (backtest failed on one or both windows)")
        return

    metrics = [
        ("Sharpe Ratio", "sharpe_ratio", ".4f"),
        ("Total Trades", "total_trades", "d"),
        ("Win Rate", "win_rate", ".2%"),
        ("Total PnL", "total_pnl", ".2f"),
        ("Profit Factor", "profit_factor", ".2f"),
        ("Max Drawdown", "max_drawdown", ".2f"),
    ]
    for label, key, fmt in metrics:
        tv = train_r.get(key, 0)
        ev = test_r.get(key, 0)
        if isinstance(tv, str):
            tv = float(tv)
        if isinstance(ev, str):
            ev = float(ev)
        delta = ev - tv
        if fmt == "d":
            print(f"  {label:<20} {tv:>12d} {ev:>12d} {delta:>+10d}")
        elif fmt == ".2%":
            print(f"  {label:<20} {tv:>12.2%} {ev:>12.2%} {delta:>+10.2%}")
        else:
            print(f"  {label:<20} {tv:>12{fmt}} {ev:>12{fmt}} {delta:>+10{fmt}}")
    print("=" * 60)


def print_top_trials(study: optuna.Study, n: int = 10) -> None:
    """Print summary of top N trials."""
    trials = sorted(study.trials, key=lambda t: t.value if t.value is not None else float("-inf"), reverse=True)
    print(f"\nTop {min(n, len(trials))} trials:")
    print(f"  {'#':<6} {'Sharpe':>10} {'min_edge':>10} {'p_entry':>10} {'min_abs_z':>10} {'cooldown':>10}")
    print("  " + "-" * 58)
    for t in trials[:n]:
        if t.value is None or t.value == float("-inf"):
            continue
        p = t.params
        print(
            f"  {t.number:<6} {t.value:>10.4f} "
            f"{p.get('min_edge', 0):>10.4f} "
            f"{p.get('p_entry', 0):>10.4f} "
            f"{p.get('min_abs_z', 0):>10.4f} "
            f"{p.get('cooldown_secs', 0):>10d}"
        )


def save_study_json(study: optuna.Study, path: str) -> None:
    """Serialize study results to JSON."""
    out_dir = os.path.dirname(path)
    if out_dir:
        os.makedirs(out_dir, exist_ok=True)

    trials_data = []
    for t in study.trials:
        trials_data.append({
            "number": t.number,
            "value": t.value,
            "params": t.params,
            "state": t.state.name,
        })

    best = study.best_trial
    data = {
        "best_trial": best.number,
        "best_value": best.value,
        "best_params": best.params,
        "n_trials": len(study.trials),
        "trials": trials_data,
    }
    with open(path, "w") as f:
        json.dump(data, f, indent=2, default=str)
    print(f"\nStudy saved to {path}")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="Optuna TPE optimization for pm_5m_directional"
    )
    p.add_argument("--trials", type=int, default=100, help="Number of Optuna trials")
    p.add_argument("--from", dest="dt_from", required=True, help="Start datetime (ISO-8601)")
    p.add_argument("--to", dest="dt_to", required=True, help="End datetime (ISO-8601)")
    p.add_argument("--train-days", type=int, default=6, help="Training window days")
    p.add_argument("--test-days", type=int, default=2, help="Test window days")
    p.add_argument("--binary", default="/root/ploy/bin/ploy", help="Path to ploy binary")
    p.add_argument(
        "--db-url",
        default="postgresql://postgres:postgres@localhost:5432/ploy",
        help="PostgreSQL connection URL",
    )
    p.add_argument("--symbols", default=",".join(SYMBOLS), help="Comma-separated symbols")
    p.add_argument("--per-symbol", action="store_true", help="Per-symbol optimization mode")
    p.add_argument("--capital", type=int, default=10000, help="Backtest capital")
    p.add_argument("--min-trades", type=int, default=MIN_TRADES_DEFAULT, help="Min trades to accept trial")
    p.add_argument("--timeout", type=int, default=300, help="Per-backtest timeout (seconds)")
    p.add_argument("--output", default="results/pm5_optim.json", help="Output JSON path")
    p.add_argument("--seed", type=int, default=42, help="Random seed for TPE sampler")
    return p.parse_args()


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main() -> None:
    args = parse_args()
    symbols = [s.strip() for s in args.symbols.split(",")]

    # Walk-forward split
    train_from, train_to, test_from, test_to = split_walk_forward(
        args.dt_from, args.dt_to, args.train_days, args.test_days
    )
    print(f"Train window: {train_from} -> {train_to}")
    print(f"Test  window: {test_from} -> {test_to}")
    print(f"Symbols: {symbols}")
    print(f"Per-symbol mode: {args.per_symbol}")
    print(f"Trials: {args.trials}, min trades: {args.min_trades}")
    print()

    # Create study
    sampler = TPESampler(seed=args.seed)
    pruner = MedianPruner(n_startup_trials=10, n_warmup_steps=5)
    study = optuna.create_study(
        direction="maximize",
        sampler=sampler,
        pruner=pruner,
        study_name="pm5_directional_optim",
    )

    # Optimize
    objective = make_objective(
        binary=args.binary,
        db_url=args.db_url,
        symbols=symbols,
        train_from=train_from,
        train_to=train_to,
        per_symbol=args.per_symbol,
        min_trades=args.min_trades,
        capital=args.capital,
        timeout=args.timeout,
    )
    study.optimize(objective, n_trials=args.trials, show_progress_bar=True)

    # Results
    best = study.best_trial
    print(f"\nBest trial #{best.number}: Sharpe = {best.value:.4f}")
    print(f"Best params: {best.params}")

    # Top trials
    print_top_trials(study)

    # TOML snippet
    toml = params_to_toml(best.params, symbols, args.per_symbol)
    print("\n" + "=" * 60)
    print("TOML CONFIG SNIPPET")
    print("=" * 60)
    print(toml)
    print("=" * 60)

    # Walk-forward validation
    print("\nRunning validation on test window...")
    train_r, test_r = validate_best(
        best_params=best.params,
        binary=args.binary,
        db_url=args.db_url,
        symbols=symbols,
        train_from=train_from,
        train_to=train_to,
        test_from=test_from,
        test_to=test_to,
        per_symbol=args.per_symbol,
        capital=args.capital,
        timeout=args.timeout,
    )
    print_comparison(train_r, test_r)

    # Save study
    save_study_json(study, args.output)


if __name__ == "__main__":
    main()
