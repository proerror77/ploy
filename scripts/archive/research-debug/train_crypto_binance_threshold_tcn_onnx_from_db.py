#!/usr/bin/env python3
"""
Train a Binance-first TCN to predict Polymarket crypto threshold settlement (YES wins).

Goal
  Predict whether the underlying spot will be ABOVE the market threshold at the
  scheduled resolution time, using Binance L2 (binance_lob_ticks) features only.

Policy / EV (binary options, fixed stake=1.0)
  At decision time t, you observe Polymarket outcome asks:
    yes_cost = yes_ask + (fee_buffer + slippage_buffer)
    no_cost  = no_ask  + (fee_buffer + slippage_buffer)

  If you stake 1.0 notional:
    win ROI  = 1 / entry_cost - 1
    lose ROI = -1

  With model p_yes = P(YES wins):
    EV_ROI(YES) = p_yes / yes_cost - 1
    EV_ROI(NO)  = (1 - p_yes) / no_cost - 1

  To avoid unrealistic over-trading, evaluation is "one trade per event":
    for each condition_id, pick the single timestamp inside the entry window
    with the highest expected ROI that clears edge_threshold.

Data requirements
  - pm_token_settlements (resolved labels, raw_market JSONB uses camelCase: endDate, groupItemThreshold, etc.)
  - binance_lob_ticks (Binance L2 snapshots)
  - clob_orderbook_history_ticks OR clob_orderbook_snapshots (Polymarket asks for EV only)
  - collector_token_targets (optional; helps map token_id -> binance symbol, threshold)

Install deps:
  python3 -m pip install torch psycopg2-binary

Optional:
  python3 -m pip install pandas pyarrow
"""

from __future__ import annotations

import argparse
import json
import math
import os
import random
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Dict, List, Optional, Tuple


# Model features only (no Polymarket prices here).
FEATURE_ORDER = [
    "bn_mid",
    "bn_spread_bps",
    "bn_obi_5",
    "bn_obi_10",
    "bn_bid_volume_5",
    "bn_ask_volume_5",
    "bn_vol_short_bps",
    "bn_vol_long_bps",
    "spot_vs_start_ret_bps",
    "spot_vs_threshold_ret_bps",
    "secs_to_anchor",
]


@dataclass
class PointDataset:
    x: List[List[float]]
    y: List[int]
    ts: List[str]  # RFC3339
    group: List[str]  # condition_id
    asset: List[str]  # btc|eth|sol|other
    yes_ask: List[float]  # policy-only
    no_ask: List[float]  # policy-only


@dataclass
class SequenceDataset:
    x: List[List[List[float]]]  # [N, L, F]
    y: List[int]
    ts: List[str]
    group: List[str]
    asset: List[str]
    yes_ask: List[float]
    no_ask: List[float]
    seq_len: int
    feature_dim: int


def _slice_sequence_dataset(ds: SequenceDataset, idxs: List[int]) -> SequenceDataset:
    return SequenceDataset(
        x=[ds.x[i] for i in idxs],
        y=[ds.y[i] for i in idxs],
        ts=[ds.ts[i] for i in idxs],
        group=[ds.group[i] for i in idxs],
        asset=[ds.asset[i] for i in idxs],
        yes_ask=[ds.yes_ask[i] for i in idxs],
        no_ask=[ds.no_ask[i] for i in idxs],
        seq_len=ds.seq_len,
        feature_dim=ds.feature_dim,
    )


def split_train_val_sequences(
    ds: SequenceDataset,
    val_ratio: float,
    min_val_samples: int,
) -> Tuple[SequenceDataset, SequenceDataset]:
    n = len(ds.y)
    if n < 20:
        raise SystemExit(f"dataset too small for train/val split: n={n}")
    if not (0.05 <= val_ratio <= 0.5):
        raise SystemExit("--val-ratio must be in [0.05, 0.5]")

    # Group-aware split: keep each condition_id in only one side.
    group_last_ts: Dict[str, str] = {}
    group_count: Dict[str, int] = {}
    for g, t in zip(ds.group, ds.ts):
        group_count[g] = group_count.get(g, 0) + 1
        if g not in group_last_ts or t > group_last_ts[g]:
            group_last_ts[g] = t

    groups = sorted(group_last_ts.keys(), key=lambda g: group_last_ts[g])
    if len(groups) < 2:
        raise SystemExit("dataset has <2 groups; cannot split train/val safely")

    target_val_rows = max(int(n * val_ratio), min_val_samples)
    val_groups: List[str] = []
    val_rows = 0
    for g in reversed(groups):
        if len(groups) - len(val_groups) <= 1:
            break
        val_groups.append(g)
        val_rows += group_count.get(g, 0)
        if val_rows >= target_val_rows:
            break

    val_set = set(val_groups)
    train_idx = [i for i in range(n) if ds.group[i] not in val_set]
    val_idx = [i for i in range(n) if ds.group[i] in val_set]
    if len(train_idx) < 10 or len(val_idx) < 1:
        raise SystemExit(
            f"group split too small for train/val: train={len(train_idx)} val={len(val_idx)}"
        )
    return _slice_sequence_dataset(ds, train_idx), _slice_sequence_dataset(ds, val_idx)


def chronological_split_sequences(
    ds: SequenceDataset,
    test_ratio: float,
    min_total_samples: int,
) -> Tuple[SequenceDataset, SequenceDataset]:
    n = len(ds.y)
    if min_total_samples <= 0:
        raise SystemExit("--min-total-samples must be > 0")
    if n < min_total_samples:
        raise SystemExit(f"dataset too small: n={n} (need >={min_total_samples})")
    if not (0.05 <= test_ratio <= 0.5):
        raise SystemExit("--test-ratio must be in [0.05, 0.5]")

    # Group-aware chronological split by condition_id.
    group_last_ts: Dict[str, str] = {}
    group_count: Dict[str, int] = {}
    for g, t in zip(ds.group, ds.ts):
        group_count[g] = group_count.get(g, 0) + 1
        if g not in group_last_ts or t > group_last_ts[g]:
            group_last_ts[g] = t

    groups = sorted(group_last_ts.keys(), key=lambda g: group_last_ts[g])
    if len(groups) < 2:
        raise SystemExit("dataset has <2 groups; cannot split train/test safely")

    target_test_rows = max(1, int(n * test_ratio))
    test_groups: List[str] = []
    test_rows = 0
    for g in reversed(groups):
        if len(groups) - len(test_groups) <= 1:
            break
        test_groups.append(g)
        test_rows += group_count.get(g, 0)
        if test_rows >= target_test_rows:
            break

    test_set = set(test_groups)
    train_idx = [i for i in range(n) if ds.group[i] not in test_set]
    test_idx = [i for i in range(n) if ds.group[i] in test_set]
    if len(train_idx) < 10 or len(test_idx) < 1:
        raise SystemExit(
            f"group split too small for train/test: train={len(train_idx)} test={len(test_idx)}"
        )

    return _slice_sequence_dataset(ds, train_idx), _slice_sequence_dataset(ds, test_idx)


def build_sequences(ds: PointDataset, seq_len: int, stride: int = 1) -> SequenceDataset:
    if seq_len <= 0:
        raise SystemExit("--seq-len must be > 0")
    if not ds.x:
        raise SystemExit("no rows in point dataset")

    idx = list(range(len(ds.y)))
    idx.sort(key=lambda i: ds.ts[i])

    history_by_group: Dict[str, List[List[float]]] = {}
    emit_counter_by_group: Dict[str, int] = {}
    x_seq: List[List[List[float]]] = []
    y: List[int] = []
    ts: List[str] = []
    group: List[str] = []
    asset: List[str] = []
    yes_ask: List[float] = []
    no_ask: List[float] = []

    for i in idx:
        g = ds.group[i]
        hist = history_by_group.setdefault(g, [])
        hist.append(ds.x[i])

        counter = emit_counter_by_group.get(g, 0)
        emit_counter_by_group[g] = counter + 1

        if counter % stride != 0:
            continue

        if len(hist) >= seq_len:
            seq = hist[-seq_len:]
        else:
            pad = [hist[0]] * (seq_len - len(hist))
            seq = pad + hist

        x_seq.append([row[:] for row in seq])
        y.append(ds.y[i])
        ts.append(ds.ts[i])
        group.append(g)
        asset.append(ds.asset[i])
        yes_ask.append(ds.yes_ask[i])
        no_ask.append(ds.no_ask[i])

    return SequenceDataset(
        x=x_seq,
        y=y,
        ts=ts,
        group=group,
        asset=asset,
        yes_ask=yes_ask,
        no_ask=no_ask,
        seq_len=seq_len,
        feature_dim=len(ds.x[0]),
    )


def mean_std(x: List[List[float]]) -> Tuple[List[float], List[float]]:
    n = len(x)
    d = len(x[0])
    mean = [0.0] * d
    var = [0.0] * d

    for row in x:
        for j, v in enumerate(row):
            mean[j] += v
    mean = [m / n for m in mean]

    for row in x:
        for j, v in enumerate(row):
            dv = v - mean[j]
            var[j] += dv * dv
    var = [v / max(1, n - 1) for v in var]
    std = [math.sqrt(v) for v in var]
    std = [s if s > 1e-12 else 1.0 for s in std]
    return mean, std


def acc_at_05(y_true: List[int], p: List[float]) -> float:
    c = 0
    for yt, pi in zip(y_true, p):
        pred = 1 if pi >= 0.5 else 0
        if pred == yt:
            c += 1
    return c / max(1, len(y_true))


def brier(y_true: List[int], p: List[float]) -> float:
    s = 0.0
    for yt, pi in zip(y_true, p):
        s += (pi - float(yt)) ** 2
    return s / max(1, len(y_true))


def log_loss(y_true: List[int], p: List[float]) -> float:
    eps = 1e-12
    s = 0.0
    for yt, pi in zip(y_true, p):
        pi = min(1.0 - eps, max(eps, pi))
        if yt == 1:
            s += -math.log(pi)
        else:
            s += -math.log(1.0 - pi)
    return s / max(1, len(y_true))


def timeframe_bucket_from_market_slug(market_slug: str) -> str:
    slug = (market_slug or "").lower()
    if "-5m-" in slug:
        return "5m"
    if "-15m-" in slug:
        return "15m"
    return "other"


def print_baseline_ev_tables(
    ds: PointDataset,
    condition_market_slug: Dict[str, str],
    fee_buffer: float,
    slippage_buffer: float,
) -> None:
    """
    Sanity-check table: if you always buy YES or always buy NO at the observed asks
    and hold to settlement, what's the realized ROI distribution?

    This is NOT the strategy backtest (no model). It's a baseline to catch EV math
    mistakes and to understand market pricing.
    """
    total_cost = fee_buffer + slippage_buffer

    stats: Dict[Tuple[str, str, str], Dict[str, float]] = {}
    # (timeframe, asset, side) -> counters

    def _bump(tf: str, asset: str, side: str, ask: float, win: bool) -> None:
        key = (tf, asset, side)
        s = stats.setdefault(
            key,
            {
                "samples": 0.0,
                "sum_ask": 0.0,
                "wins": 0.0,
                "sum_ev_net_per_share": 0.0,
                "sum_roi": 0.0,
            },
        )
        s["samples"] += 1.0
        s["sum_ask"] += float(ask)
        s["wins"] += 1.0 if win else 0.0

        # Per-share PnL in $ terms (YES/NO both settle to 1.0 on win, 0.0 on loss).
        s["sum_ev_net_per_share"] += (1.0 - ask) if win else (-ask)

        cost = ask + total_cost
        roi = ((1.0 / cost) - 1.0) if win else -1.0
        s["sum_roi"] += float(roi)

    for i in range(len(ds.y)):
        g = ds.group[i]
        tf = timeframe_bucket_from_market_slug(condition_market_slug.get(g, ""))
        asset = ds.asset[i]
        ya = float(ds.yes_ask[i])
        na = float(ds.no_ask[i])
        y = int(ds.y[i])

        _bump(tf, asset, "YES", ya, win=(y == 1))
        _bump(tf, asset, "NO", na, win=(y == 0))

    def _print_bucket(tf: str) -> None:
        rows = []
        for (tfi, asset, side), s in stats.items():
            if tfi != tf:
                continue
            n = int(s["samples"])
            if n <= 0:
                continue
            avg_ask = s["sum_ask"] / s["samples"]
            win_rate = s["wins"] / s["samples"]
            ev_net = s["sum_ev_net_per_share"] / s["samples"]
            roi_avg = s["sum_roi"] / s["samples"]
            rows.append(
                (asset, side, n, avg_ask, win_rate, ev_net, roi_avg),
            )
        if not rows:
            return
        rows.sort(key=lambda r: (r[0], r[1]))
        print(f"\n[baseline] timeframe={tf} (always-buy side @ ask, hold to settlement)")
        print(
            "| Asset | Side | Samples | Avg Ask | Settle Success | EV Net/Share | ROI Avg (stake=1) |"
        )
        print("|---|---:|---:|---:|---:|---:|---:|")
        for asset, side, n, avg_ask, win_rate, ev_net, roi_avg in rows:
            print(
                f"| {asset.upper()} | {side} | {n} | {avg_ask:.4f} | {win_rate:.4f} | {ev_net:.4f} | {roi_avg:.4f} |"
            )

    for tf in ("5m", "15m", "other"):
        _print_bucket(tf)


def _to_float(v: object) -> Optional[float]:
    if v is None:
        return None
    try:
        f = float(v)
    except Exception:
        return None
    if not math.isfinite(f):
        return None
    return f


def _to_rfc3339(ts: object) -> str:
    if isinstance(ts, datetime):
        if ts.tzinfo is None:
            ts = ts.replace(tzinfo=timezone.utc)
        return ts.isoformat()
    return str(ts)


def _parse_price_to_beat(raw: Optional[str]) -> Optional[float]:
    if raw is None:
        return None
    s = str(raw).strip()
    if not s:
        return None
    # Handle "$94,000" style strings.
    s = s.replace("$", "").replace(",", "").strip()
    try:
        v = float(s)
    except Exception:
        return None
    if not math.isfinite(v) or v <= 0.0:
        return None
    return v


def evaluate_ev_policy_one_trade_per_condition(
    y_true: List[int],
    p_pred: List[float],
    groups: List[str],
    yes_ask: List[float],
    no_ask: List[float],
    fee_buffer: float,
    slippage_buffer: float,
    edge_threshold: float,
) -> Dict[str, float]:
    """
    One-trade-per-condition policy evaluation on fixed stake=1.0.

    For each condition_id:
      - compute best side EV_ROI across all candidate timestamps
      - pick the single timestamp with max EV_ROI >= edge_threshold
      - realized ROI: win -> 1/entry_cost - 1, else -1
    """
    total_cost = fee_buffer + slippage_buffer

    best_by_group: Dict[str, Tuple[float, int, float]] = {}
    # group -> (best_ev_roi, side_yes(1)/no(0), entry_cost)

    skipped_bad_price = 0
    for yt, pp, g, ya, na in zip(y_true, p_pred, groups, yes_ask, no_ask):
        if (
            (not math.isfinite(ya))
            or (not math.isfinite(na))
            or ya <= 0.0
            or na <= 0.0
        ):
            skipped_bad_price += 1
            continue

        yes_cost = ya + total_cost
        no_cost = na + total_cost
        if yes_cost <= 0.0 or no_cost <= 0.0:
            skipped_bad_price += 1
            continue

        p_yes = min(1.0, max(0.0, float(pp)))
        p_no = 1.0 - p_yes
        ev_yes = (p_yes / yes_cost) - 1.0
        ev_no = (p_no / no_cost) - 1.0

        if ev_yes >= ev_no:
            best_ev = ev_yes
            side_yes = 1
            entry_cost = yes_cost
        else:
            best_ev = ev_no
            side_yes = 0
            entry_cost = no_cost

        if best_ev < edge_threshold:
            continue

        prev = best_by_group.get(g)
        if prev is None or best_ev > prev[0]:
            best_by_group[g] = (best_ev, side_yes, entry_cost)

    conditions = len(set(groups))
    trades = 0
    wins = 0
    sum_pred_ev = 0.0
    realized_rois: List[float] = []
    sum_entry_cost = 0.0

    # Resolve realized PnL using the true label for each group (constant per group).
    y_by_group: Dict[str, int] = {}
    for yt, g in zip(y_true, groups):
        # y should be constant; keep first observed.
        if g not in y_by_group:
            y_by_group[g] = int(yt)

    for g, (best_ev, side_yes, entry_cost) in best_by_group.items():
        yt = y_by_group.get(g, 0)
        trades += 1
        sum_pred_ev += best_ev
        sum_entry_cost += entry_cost

        if side_yes == 1:
            realized = ((1.0 / entry_cost) - 1.0) if yt == 1 else -1.0
        else:
            realized = ((1.0 / entry_cost) - 1.0) if yt == 0 else -1.0
        realized_rois.append(realized)
        if realized > 0.0:
            wins += 1

    avg_realized = (sum(realized_rois) / max(1, trades)) if realized_rois else 0.0
    avg_pred_ev = (sum_pred_ev / max(1, trades)) if trades > 0 else 0.0

    # Basic t-stat on per-trade ROI (not annualized).
    t_stat = 0.0
    roi_std = 0.0
    if trades >= 2:
        mean_r = avg_realized
        var = 0.0
        for r in realized_rois:
            var += (r - mean_r) ** 2
        var = var / max(1, trades - 1)
        roi_std = math.sqrt(var)
        if roi_std > 1e-12:
            t_stat = mean_r / (roi_std / math.sqrt(trades))

    return {
        "edge_threshold": edge_threshold,
        "fee_buffer": fee_buffer,
        "slippage_buffer": slippage_buffer,
        "cost_buffer_total": total_cost,
        "conditions": float(conditions),
        "samples": float(len(y_true)),
        "trades": float(trades),
        "trade_rate_per_condition": (trades / max(1, conditions)),
        "hit_rate": (wins / max(1, trades)),
        "predicted_ev_avg_per_trade": avg_pred_ev,
        "predicted_ev_avg_per_trade_pct": avg_pred_ev * 100.0,
        "realized_roi_avg_per_trade": avg_realized,
        "realized_roi_avg_per_trade_pct": avg_realized * 100.0,
        "realized_roi_sum": float(sum(realized_rois)),
        "realized_roi_sum_pct": float(sum(realized_rois)) * 100.0,
        "avg_entry_cost": (sum_entry_cost / max(1, trades)),
        "roi_std": roi_std,
        "t_stat": t_stat,
        "skipped_bad_price": float(skipped_bad_price),
    }


def select_best_edge_threshold(
    y_val: List[int],
    p_val: List[float],
    groups_val: List[str],
    yes_ask_val: List[float],
    no_ask_val: List[float],
    fee_buffer: float,
    slippage_buffer: float,
    threshold_min: float,
    threshold_max: float,
    threshold_step: float,
    min_val_trades: int,
    min_val_trade_rate: float,
    abstain_on_non_positive_val_pnl: bool,
) -> Tuple[float, Dict[str, float]]:
    abstain_thr = 2.0
    if threshold_step <= 0.0:
        raise SystemExit("--edge-threshold-step must be > 0")
    if threshold_max < threshold_min:
        raise SystemExit("--edge-threshold-max must be >= --edge-threshold-min")
    if min_val_trade_rate < 0.0 or min_val_trade_rate > 1.0:
        raise SystemExit("--min-val-trade-rate must be in [0,1]")

    thresholds: List[float] = []
    t = threshold_min
    while t <= threshold_max + 1e-12:
        thresholds.append(round(t, 10))
        t += threshold_step

    best_thr = threshold_min
    best_eval: Optional[Dict[str, float]] = None
    best_sum = -1e18
    best_avg = -1e18
    relaxed_eval: Optional[Dict[str, float]] = None
    relaxed_thr = threshold_min
    relaxed_sum = -1e18
    relaxed_avg = -1e18

    for thr in thresholds:
        ev = evaluate_ev_policy_one_trade_per_condition(
            y_true=y_val,
            p_pred=p_val,
            groups=groups_val,
            yes_ask=yes_ask_val,
            no_ask=no_ask_val,
            fee_buffer=fee_buffer,
            slippage_buffer=slippage_buffer,
            edge_threshold=thr,
        )
        trades = int(ev["trades"])
        trade_rate = float(ev["trade_rate_per_condition"])
        pnl_sum = float(ev["realized_roi_sum"])
        pnl_avg = float(ev["realized_roi_avg_per_trade"])

        if (
            trades >= min_val_trades
            and trade_rate >= min_val_trade_rate
            and ((pnl_sum > best_sum) or (pnl_sum == best_sum and pnl_avg > best_avg))
        ):
            best_sum = pnl_sum
            best_avg = pnl_avg
            best_eval = ev
            best_thr = thr

        if trades > 0 and pnl_sum > 0.0 and (
            (pnl_sum > relaxed_sum) or (pnl_sum == relaxed_sum and pnl_avg > relaxed_avg)
        ):
            relaxed_sum = pnl_sum
            relaxed_avg = pnl_avg
            relaxed_eval = ev
            relaxed_thr = thr

    if best_eval is None:
        if relaxed_eval is not None:
            relaxed_eval["relaxed_threshold_selection"] = 1.0
            relaxed_eval["relaxed_reason"] = "no_threshold_met_trade_rate_or_min_trades"
            return relaxed_thr, relaxed_eval

        best_eval = evaluate_ev_policy_one_trade_per_condition(
            y_true=y_val,
            p_pred=p_val,
            groups=groups_val,
            yes_ask=yes_ask_val,
            no_ask=no_ask_val,
            fee_buffer=fee_buffer,
            slippage_buffer=slippage_buffer,
            edge_threshold=abstain_thr,
        )
        best_eval["abstain_recommended"] = 1.0
        best_eval["abstain_reason"] = "insufficient_validation_trades"
        return abstain_thr, best_eval

    if abstain_on_non_positive_val_pnl:
        if (
            float(best_eval.get("realized_roi_sum", 0.0)) <= 0.0
            or float(best_eval.get("realized_roi_avg_per_trade", 0.0)) <= 0.0
        ):
            best_eval = evaluate_ev_policy_one_trade_per_condition(
                y_true=y_val,
                p_pred=p_val,
                groups=groups_val,
                yes_ask=yes_ask_val,
                no_ask=no_ask_val,
                fee_buffer=fee_buffer,
                slippage_buffer=slippage_buffer,
                edge_threshold=abstain_thr,
            )
            best_eval["abstain_recommended"] = 1.0
            best_eval["abstain_reason"] = "non_positive_validation_pnl"
            return abstain_thr, best_eval

    best_eval["abstain_gate_disabled"] = 1.0
    return best_thr, best_eval


def maybe_save_parquet(rows: List[Dict[str, object]], path: str) -> None:
    try:
        import pandas as pd  # type: ignore
    except Exception:
        print("[warn] pandas not installed; skipping parquet snapshot")
        return

    try:
        import pyarrow  # noqa: F401
        import pyarrow.parquet  # noqa: F401
    except Exception:
        print("[warn] pyarrow not installed; skipping parquet snapshot")
        return

    df = pd.DataFrame(rows)
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    df.to_parquet(path, index=False)
    print(f"Saved parquet snapshot: {path} (rows={len(df)})")


def fetch_from_db(
    db_url: str,
    lookback_hours: int,
    sample_seconds: int,
    entry_window_start_seconds: int,
    entry_window_end_seconds: int,
    vol_short_window_seconds: int,
    vol_long_window_seconds: int,
    market_timeframe: str,
    market_asset: str,
    limit: int,
    pm_book_source: str,
) -> Tuple[PointDataset, List[Dict[str, object]], Dict[str, str]]:
    try:
        import psycopg2  # type: ignore
    except Exception as e:
        raise SystemExit(
            "psycopg2 is required.\n"
            "Install: python3 -m pip install psycopg2-binary\n"
            f"Import error: {e}"
        )

    if lookback_hours <= 0:
        raise SystemExit("--lookback-hours must be > 0")
    if sample_seconds <= 0:
        raise SystemExit("--sample-seconds must be > 0")
    if entry_window_start_seconds <= 0:
        raise SystemExit("--entry-window-start-seconds must be > 0")
    if entry_window_end_seconds < 0:
        raise SystemExit("--entry-window-end-seconds must be >= 0")
    if entry_window_start_seconds <= entry_window_end_seconds:
        raise SystemExit("--entry-window-start-seconds must be > --entry-window-end-seconds")
    if vol_short_window_seconds <= 0:
        raise SystemExit("--vol-short-window-seconds must be > 0")
    if vol_long_window_seconds <= 0:
        raise SystemExit("--vol-long-window-seconds must be > 0")
    if vol_long_window_seconds < vol_short_window_seconds:
        raise SystemExit("--vol-long-window-seconds must be >= --vol-short-window-seconds")
    if market_timeframe not in ("all", "5m", "15m"):
        raise SystemExit("--market-timeframe must be one of: all, 5m, 15m")
    if market_asset not in ("all", "btc", "eth", "sol", "other"):
        raise SystemExit("--market-asset must be one of: all, btc, eth, sol, other")
    if limit <= 0:
        raise SystemExit("--limit must be > 0")
    if pm_book_source not in ("obh", "ws"):
        raise SystemExit("--pm-book-source must be one of: obh, ws")

    # Only interpolate a fixed, validated table name.
    if pm_book_source == "obh":
        pm_table = "clob_orderbook_history_ticks"
        pm_time_col = "book_ts"
        pm_domain_clause = ""  # table has no domain column
    else:
        pm_table = "clob_orderbook_snapshots"
        pm_time_col = "received_at"
        pm_domain_clause = "AND LOWER(COALESCE(domain, '')) = 'crypto'"

    sql = f"""
    WITH settled AS (
      SELECT
        condition_id,
        MAX(COALESCE(market_slug, raw_market->>'slug')) AS market_slug,
        MAX(token_id) FILTER (
          WHERE LOWER(TRIM(COALESCE(outcome, ''))) IN ('yes', 'up', 'higher', 'above', 'true')
        ) AS yes_token_id,
        MAX(token_id) FILTER (
          WHERE LOWER(TRIM(COALESCE(outcome, ''))) IN ('no', 'down', 'lower', 'below', 'false')
        ) AS no_token_id,
        MAX(settled_price) FILTER (
          WHERE LOWER(TRIM(COALESCE(outcome, ''))) IN ('yes', 'up', 'higher', 'above', 'true')
        )::double precision AS yes_settled_price,
        MAX(settled_price) FILTER (
          WHERE LOWER(TRIM(COALESCE(outcome, ''))) IN ('no', 'down', 'lower', 'below', 'false')
        )::double precision AS no_settled_price,
        MAX(resolved_at) AS resolved_at,
        MAX(COALESCE((raw_market->>'endDate')::timestamptz, (raw_market->>'end_date')::timestamptz)) AS end_date,
        MAX(COALESCE((raw_market->>'eventStartTime')::timestamptz, (raw_market->>'startDate')::timestamptz, (raw_market->>'start_date')::timestamptz)) AS start_date,
        MAX(COALESCE(raw_market->>'groupItemThreshold', raw_market->>'group_item_threshold')) AS group_item_threshold,
        MAX(COALESCE(raw_market->>'upperBound', raw_market->>'upper_bound')) AS upper_bound,
        MAX(COALESCE(raw_market->>'lowerBound', raw_market->>'lower_bound')) AS lower_bound
      FROM pm_token_settlements
      WHERE resolved = TRUE
        AND settled_price IS NOT NULL
        AND condition_id IS NOT NULL
        AND (%s::text = 'all' OR LOWER(COALESCE(market_slug, raw_market->>'slug', '')) LIKE %s)
        AND (
          %s::text = 'all'
          OR (%s::text = 'btc' AND LOWER(COALESCE(market_slug, raw_market->>'slug', '')) LIKE 'btc-%%')
          OR (%s::text = 'eth' AND LOWER(COALESCE(market_slug, raw_market->>'slug', '')) LIKE 'eth-%%')
          OR (%s::text = 'sol' AND LOWER(COALESCE(market_slug, raw_market->>'slug', '')) LIKE 'sol-%%')
          OR (
            %s::text = 'other'
            AND LOWER(COALESCE(market_slug, raw_market->>'slug', '')) NOT LIKE 'btc-%%'
            AND LOWER(COALESCE(market_slug, raw_market->>'slug', '')) NOT LIKE 'eth-%%'
            AND LOWER(COALESCE(market_slug, raw_market->>'slug', '')) NOT LIKE 'sol-%%'
          )
        )
        AND resolved_at >= NOW() - (%s::bigint * INTERVAL '1 hour')
      GROUP BY condition_id
      HAVING
        MAX(token_id) FILTER (
          WHERE LOWER(TRIM(COALESCE(outcome, ''))) IN ('yes', 'up', 'higher', 'above', 'true')
        ) IS NOT NULL
        AND MAX(token_id) FILTER (
          WHERE LOWER(TRIM(COALESCE(outcome, ''))) IN ('no', 'down', 'lower', 'below', 'false')
        ) IS NOT NULL
    ),
    enriched AS (
      SELECT
        st.condition_id,
        st.market_slug,
        st.yes_token_id,
        st.no_token_id,
        st.yes_settled_price,
        st.no_settled_price,
        COALESCE(st.end_date, st.resolved_at) AS anchor_ts,
        st.start_date AS market_start_ts,
        COALESCE(
          NULLIF(BTRIM(t.metadata->>'symbol'), ''),
          CASE
            WHEN LOWER(COALESCE(st.market_slug, '')) LIKE 'btc-%%' THEN 'BTCUSDT'
            WHEN LOWER(COALESCE(st.market_slug, '')) LIKE 'eth-%%' THEN 'ETHUSDT'
            WHEN LOWER(COALESCE(st.market_slug, '')) LIKE 'sol-%%' THEN 'SOLUSDT'
            ELSE NULL
          END
        ) AS binance_symbol,
        COALESCE(
          NULLIF(BTRIM(t.metadata->>'price_to_beat'), ''),
          NULLIF(BTRIM(st.group_item_threshold), ''),
          NULLIF(BTRIM(st.upper_bound), ''),
          NULLIF(BTRIM(st.lower_bound), '')
        ) AS price_to_beat_raw
      FROM settled st
      LEFT JOIN collector_token_targets t
        ON t.token_id = st.yes_token_id
      WHERE COALESCE(st.end_date, st.resolved_at) IS NOT NULL
    ),
    samples AS (
      SELECT
        e.*,
        gs.ts
      FROM enriched e
      JOIN LATERAL (
        SELECT generate_series(
          e.anchor_ts - (%s::bigint * INTERVAL '1 second'),
          e.anchor_ts - (%s::bigint * INTERVAL '1 second'),
          (%s::bigint * INTERVAL '1 second')
        ) AS ts
      ) gs ON TRUE
    )
    SELECT
      s.ts,
      s.condition_id,
      s.market_slug,
      s.binance_symbol,
      s.price_to_beat_raw,
      s.anchor_ts,
      s.market_start_ts,
      s.yes_token_id,
      s.no_token_id,
      yb.yes_ask,
      nb.no_ask,
      bn.mid_price,
      bn.spread_bps,
      bn.obi_5,
      bn.obi_10,
      bn.bid_volume_5,
      bn.ask_volume_5,
      bv.vol_short_bps,
      bv.vol_long_bps,
      bs.price AS spot_start,
      sn.price AS spot_now,
      GREATEST(EXTRACT(EPOCH FROM (s.anchor_ts - s.ts)), 0.0)::double precision AS secs_to_anchor,
      s.yes_settled_price,
      s.no_settled_price
    FROM samples s
    LEFT JOIN LATERAL (
      SELECT
        (asks->0->>'price')::double precision AS yes_ask
      FROM {pm_table}
      WHERE token_id = s.yes_token_id
        AND {pm_time_col} <= s.ts
        {pm_domain_clause}
      ORDER BY {pm_time_col} DESC
      LIMIT 1
    ) yb ON TRUE
    LEFT JOIN LATERAL (
      SELECT
        (asks->0->>'price')::double precision AS no_ask
      FROM {pm_table}
      WHERE token_id = s.no_token_id
        AND {pm_time_col} <= s.ts
        {pm_domain_clause}
      ORDER BY {pm_time_col} DESC
      LIMIT 1
    ) nb ON TRUE
    LEFT JOIN LATERAL (
      SELECT
        mid_price::double precision AS mid_price,
        spread_bps::double precision AS spread_bps,
        obi_5::double precision AS obi_5,
        obi_10::double precision AS obi_10,
        bid_volume_5::double precision AS bid_volume_5,
        ask_volume_5::double precision AS ask_volume_5
      FROM binance_lob_ticks b
      WHERE s.binance_symbol IS NOT NULL
        AND b.symbol = s.binance_symbol
        AND b.event_time <= s.ts
      ORDER BY b.event_time DESC
      LIMIT 1
    ) bn ON TRUE
    LEFT JOIN LATERAL (
      SELECT
        COALESCE(
          (
            STDDEV_SAMP(mid_price) FILTER (
              WHERE sample_ts > s.ts - (%s::bigint * INTERVAL '1 second')
            ) / NULLIF(
              AVG(mid_price) FILTER (
                WHERE sample_ts > s.ts - (%s::bigint * INTERVAL '1 second')
              ),
              0
            )
          ) * 10000.0,
          0.0
        )::double precision AS vol_short_bps,
        COALESCE(
          (STDDEV_SAMP(mid_price) / NULLIF(AVG(mid_price), 0)) * 10000.0,
          0.0
        )::double precision AS vol_long_bps
      FROM (
        SELECT
          b2.event_time AS sample_ts,
          b2.mid_price::double precision AS mid_price
        FROM binance_lob_ticks b2
        WHERE s.binance_symbol IS NOT NULL
          AND b2.symbol = s.binance_symbol
          AND b2.event_time <= s.ts
          AND b2.event_time > s.ts - (%s::bigint * INTERVAL '1 second')
      ) v
      WHERE mid_price IS NOT NULL
        AND mid_price > 0.0
    ) bv ON TRUE
    LEFT JOIN LATERAL (
      SELECT b.price::double precision AS price
      FROM binance_price_ticks b
      WHERE s.binance_symbol IS NOT NULL
        AND b.symbol = s.binance_symbol
        AND b.trade_time <= s.ts
      ORDER BY b.trade_time DESC
      LIMIT 1
    ) sn ON TRUE
    LEFT JOIN LATERAL (
      SELECT b.price::double precision AS price
      FROM binance_price_ticks b
      WHERE s.binance_symbol IS NOT NULL
        AND s.market_start_ts IS NOT NULL
        AND b.symbol = s.binance_symbol
        AND b.trade_time <= s.market_start_ts
      ORDER BY b.trade_time DESC
      LIMIT 1
    ) bs ON TRUE
    WHERE yb.yes_ask IS NOT NULL
      AND nb.no_ask IS NOT NULL
      AND bn.mid_price IS NOT NULL
    ORDER BY s.ts ASC
    LIMIT %s
    """

    params = [
        market_timeframe,
        ("%-{}-%".format(market_timeframe) if market_timeframe in ("5m", "15m") else "%"),
        market_asset,
        market_asset,
        market_asset,
        market_asset,
        market_asset,
        lookback_hours,
        entry_window_start_seconds,
        entry_window_end_seconds,
        sample_seconds,
        vol_short_window_seconds,
        vol_short_window_seconds,
        vol_long_window_seconds,
        limit,
    ]

    x: List[List[float]] = []
    y: List[int] = []
    ts: List[str] = []
    group: List[str] = []
    asset: List[str] = []
    yes_ask: List[float] = []
    no_ask: List[float] = []
    condition_market_slug: Dict[str, str] = {}
    exported_rows: List[Dict[str, object]] = []

    skipped_bad_label = 0
    skipped_nonfinite = 0
    skipped_missing_threshold = 0

    conn = psycopg2.connect(db_url)
    try:
        with conn.cursor() as cur:
            cur.execute(sql, params)
            for row in cur.fetchall():
                ts_raw = row[0]
                condition_id = str(row[1] or "").strip()
                market_slug = str(row[2] or "")
                binance_symbol = str(row[3] or "")
                price_to_beat_raw = row[4]
                # row[5]=anchor_ts, row[6]=market_start_ts
                yes_token_id = str(row[7] or "")
                no_token_id = str(row[8] or "")
                yes_a = _to_float(row[9])
                no_a = _to_float(row[10])

                bn_mid = _to_float(row[11])
                bn_spread_bps = _to_float(row[12]) or 0.0
                bn_obi_5 = _to_float(row[13]) or 0.0
                bn_obi_10 = _to_float(row[14]) or 0.0
                bn_bid_vol_5 = _to_float(row[15]) or 0.0
                bn_ask_vol_5 = _to_float(row[16]) or 0.0
                bn_vol_short_bps = _to_float(row[17]) or 0.0
                bn_vol_long_bps = _to_float(row[18]) or 0.0
                spot_start = _to_float(row[19])
                spot_now = _to_float(row[20])
                secs_to_anchor = _to_float(row[21]) or 0.0
                yes_settled = _to_float(row[22])
                no_settled = _to_float(row[23])

                if yes_settled is None:
                    skipped_bad_label += 1
                    continue
                y_yes_win = 1 if yes_settled > 0.5 else 0

                spot_now_f = spot_now if spot_now is not None else bn_mid
                if spot_now_f is None or spot_now_f <= 0.0:
                    skipped_nonfinite += 1
                    continue

                spot_start_f = spot_start if spot_start is not None else spot_now_f
                spot_vs_start_ret_bps = (
                    ((spot_now_f - spot_start_f) / spot_start_f) * 10000.0
                    if spot_start_f > 0.0
                    else 0.0
                )

                # For up/down markets the threshold IS the spot price at
                # market start.  collector_token_targets stores a series
                # number (22, 23 …) not a dollar price, and Gamma API
                # returns groupItemThreshold=0 for relative markets.
                # Fall back to spot_start which is the Binance price at
                # eventStartTime – exactly what determines settlement.
                thr = _parse_price_to_beat(price_to_beat_raw)
                if thr is None or thr < 1.0:
                    # price_to_beat is 0 or a small series number – use
                    # spot at market start as the effective threshold.
                    thr = spot_start_f if spot_start_f > 0.0 else None
                if thr is None:
                    skipped_missing_threshold += 1
                    continue

                spot_vs_thr_ret_bps = ((spot_now_f / thr) - 1.0) * 10000.0

                # Asset bucket from slug (fallback to "other").
                slug_lower = market_slug.lower()
                if slug_lower.startswith("btc-"):
                    asset_bucket = "btc"
                elif slug_lower.startswith("eth-"):
                    asset_bucket = "eth"
                elif slug_lower.startswith("sol-"):
                    asset_bucket = "sol"
                else:
                    asset_bucket = "other"

                feats = [
                    float(bn_mid) if bn_mid is not None else float(spot_now_f),
                    float(bn_spread_bps),
                    float(bn_obi_5),
                    float(bn_obi_10),
                    float(bn_bid_vol_5),
                    float(bn_ask_vol_5),
                    float(bn_vol_short_bps),
                    float(bn_vol_long_bps),
                    float(spot_vs_start_ret_bps),
                    float(spot_vs_thr_ret_bps),
                    float(secs_to_anchor),
                ]

                if any((v is None or (not math.isfinite(v))) for v in feats):
                    skipped_nonfinite += 1
                    continue
                if yes_a is None or no_a is None:
                    skipped_nonfinite += 1
                    continue

                ts_iso = _to_rfc3339(ts_raw)
                x.append(feats)
                y.append(y_yes_win)
                ts.append(ts_iso)
                group.append(condition_id)
                asset.append(asset_bucket)
                yes_ask.append(float(yes_a))
                no_ask.append(float(no_a))
                if condition_id and market_slug:
                    condition_market_slug[condition_id] = market_slug

                exported_rows.append(
                    {
                        "ts": ts_iso,
                        "condition_id": condition_id,
                        "market_slug": market_slug,
                        "asset": asset_bucket,
                        "binance_symbol": binance_symbol,
                        "price_to_beat": thr,
                        "spot_now": spot_now_f,
                        "spot_start": spot_start_f,
                        "spot_vs_start_ret_bps": spot_vs_start_ret_bps,
                        "spot_vs_threshold_ret_bps": spot_vs_thr_ret_bps,
                        "yes_token_id": yes_token_id,
                        "no_token_id": no_token_id,
                        "yes_ask": float(yes_a),
                        "no_ask": float(no_a),
                        "yes_settled_price": yes_settled,
                        "no_settled_price": no_settled,
                        "y_yes_win": y_yes_win,
                    }
                )
    finally:
        conn.close()

    if not x:
        raise SystemExit("no usable rows fetched (need Polymarket asks + Binance L2 + settlements)")

    print(
        "[fetch] usable_rows={} skipped_bad_label={} skipped_missing_threshold={} skipped_nonfinite={}".format(
            len(x),
            skipped_bad_label,
            skipped_missing_threshold,
            skipped_nonfinite,
        )
    )
    print(
        "[fetch] class_balance yes_win={} no_win={}".format(
            sum(y),
            len(y) - sum(y),
        )
    )

    return (
        PointDataset(x=x, y=y, ts=ts, group=group, asset=asset, yes_ask=yes_ask, no_ask=no_ask),
        exported_rows,
        condition_market_slug,
    )


def train_and_export_onnx_tcn(
    train_ds: SequenceDataset,
    test_ds: SequenceDataset,
    condition_market_slug: Optional[Dict[str, str]],
    channels: List[int],
    kernel_size: int,
    dropout: float,
    val_ratio: float,
    min_val_samples: int,
    fee_buffer: float,
    slippage_buffer: float,
    edge_threshold_min: float,
    edge_threshold_max: float,
    edge_threshold_step: float,
    min_val_trades: int,
    min_val_trade_rate: float,
    abstain_on_non_positive_val_pnl: bool,
    epochs: int,
    batch_size: int,
    lr: float,
    seed: int,
    onnx_path: str,
    opset: int,
) -> dict:
    try:
        import torch
        import torch.nn as nn
        import torch.nn.functional as torch_f
    except Exception as e:
        raise SystemExit(
            "PyTorch is required for training.\n"
            "Install: python3 -m pip install torch\n"
            f"Import error: {e}"
        )

    if not channels:
        raise SystemExit("--tcn-channels must not be empty")
    if kernel_size <= 1:
        raise SystemExit("--tcn-kernel-size must be > 1")
    if not (0.0 <= dropout < 1.0):
        raise SystemExit("--tcn-dropout must be in [0,1)")
    if fee_buffer < 0.0:
        raise SystemExit("--fee-buffer must be >= 0")
    if slippage_buffer < 0.0:
        raise SystemExit("--slippage-buffer must be >= 0")

    torch.manual_seed(seed)
    random.seed(seed)

    train_main_ds, val_ds = split_train_val_sequences(
        train_ds,
        val_ratio=val_ratio,
        min_val_samples=min_val_samples,
    )

    seq_len = train_main_ds.seq_len
    in_dim = train_main_ds.feature_dim

    flat_train = [row for seq in train_main_ds.x for row in seq]
    mean_vec, std_vec = mean_std(flat_train)

    class CausalConv1d(nn.Module):
        def __init__(self, in_ch: int, out_ch: int, k: int, dilation: int):
            super().__init__()
            self.left_pad = (k - 1) * dilation
            self.conv = nn.Conv1d(in_ch, out_ch, kernel_size=k, dilation=dilation)

        def forward(self, x):
            x = torch_f.pad(x, (self.left_pad, 0))
            return self.conv(x)

    class TcnResidualBlock(nn.Module):
        def __init__(self, in_ch: int, out_ch: int, k: int, dilation: int, p_drop: float):
            super().__init__()
            self.conv1 = CausalConv1d(in_ch, out_ch, k, dilation)
            self.conv2 = CausalConv1d(out_ch, out_ch, k, dilation)
            self.relu = nn.ReLU()
            self.drop = nn.Dropout(p_drop)
            self.skip = (
                nn.Conv1d(in_ch, out_ch, kernel_size=1)
                if in_ch != out_ch
                else nn.Identity()
            )

        def forward(self, x):
            y = self.conv1(x)
            y = self.relu(y)
            y = self.drop(y)
            y = self.conv2(y)
            y = self.relu(y)
            y = self.drop(y)
            return self.relu(y + self.skip(x))

    class BinanceThresholdTCN(nn.Module):
        def __init__(self, mean_f, std_f, tcn_channels, k, p_drop):
            super().__init__()
            self.register_buffer(
                "mean",
                torch.tensor(mean_f, dtype=torch.float32).view(1, 1, -1),
                persistent=True,
            )
            self.register_buffer(
                "std",
                torch.tensor(std_f, dtype=torch.float32).view(1, 1, -1),
                persistent=True,
            )

            chs = [in_dim] + list(tcn_channels)
            blocks = []
            for i in range(1, len(chs)):
                blocks.append(
                    TcnResidualBlock(
                        in_ch=chs[i - 1],
                        out_ch=chs[i],
                        k=k,
                        dilation=2 ** (i - 1),
                        p_drop=p_drop,
                    )
                )
            self.tcn = nn.Sequential(*blocks)
            self.head = nn.Linear(chs[-1], 1)

        def forward(self, x_seq):
            # x_seq: [B, L, F]
            x = (x_seq - self.mean) / self.std
            x = x.permute(0, 2, 1)  # [B, F, L]
            h = self.tcn(x)
            last = h[:, :, -1]
            logit = self.head(last).squeeze(-1)
            return torch.sigmoid(logit)

    model = BinanceThresholdTCN(mean_vec, std_vec, channels, kernel_size, dropout)
    opt = torch.optim.Adam(model.parameters(), lr=lr)
    loss_fn = nn.BCELoss()

    def to_batches(x_seqs: List[List[List[float]]], y_: List[int], bs: int):
        idxs = list(range(len(y_)))
        random.shuffle(idxs)
        for i in range(0, len(idxs), bs):
            chunk = idxs[i : i + bs]
            x_b = torch.tensor([x_seqs[j] for j in chunk], dtype=torch.float32)
            y_b = torch.tensor([y_[j] for j in chunk], dtype=torch.float32)
            yield x_b, y_b

    best_val = 1e18
    best_state = None
    for ep in range(epochs):
        model.train()
        for x_b, y_b in to_batches(train_main_ds.x, train_main_ds.y, batch_size):
            opt.zero_grad()
            p = model(x_b)
            loss = loss_fn(p, y_b)
            loss.backward()
            torch.nn.utils.clip_grad_norm_(model.parameters(), max_norm=1.0)
            opt.step()

        model.eval()
        with torch.no_grad():
            x_v = torch.tensor(val_ds.x, dtype=torch.float32)
            y_v = torch.tensor(val_ds.y, dtype=torch.float32)
            p_v = model(x_v)
            vloss = loss_fn(p_v, y_v).item()

        if vloss < best_val:
            best_val = vloss
            best_state = {k: v.clone() for k, v in model.state_dict().items()}

        if (ep + 1) % 5 == 0 or ep == 0:
            print(f"[train] epoch={ep+1}/{epochs} val_loss={vloss:.6f}")

    if best_state is not None:
        model.load_state_dict(best_state)

    def predict(ds: SequenceDataset) -> List[float]:
        model.eval()
        out: List[float] = []
        with torch.no_grad():
            x_all = torch.tensor(ds.x, dtype=torch.float32)
            p_all = model(x_all).cpu().numpy().tolist()
            out = [float(v) for v in p_all]
        return out

    p_val = predict(val_ds)
    p_test = predict(test_ds)

    selected_edge_threshold, val_policy = select_best_edge_threshold(
        y_val=val_ds.y,
        p_val=p_val,
        groups_val=val_ds.group,
        yes_ask_val=val_ds.yes_ask,
        no_ask_val=val_ds.no_ask,
        fee_buffer=fee_buffer,
        slippage_buffer=slippage_buffer,
        threshold_min=edge_threshold_min,
        threshold_max=edge_threshold_max,
        threshold_step=edge_threshold_step,
        min_val_trades=min_val_trades,
        min_val_trade_rate=min_val_trade_rate,
        abstain_on_non_positive_val_pnl=abstain_on_non_positive_val_pnl,
    )

    test_policy = evaluate_ev_policy_one_trade_per_condition(
        y_true=test_ds.y,
        p_pred=p_test,
        groups=test_ds.group,
        yes_ask=test_ds.yes_ask,
        no_ask=test_ds.no_ask,
        fee_buffer=fee_buffer,
        slippage_buffer=slippage_buffer,
        edge_threshold=selected_edge_threshold,
    )

    test_policy_edge0 = evaluate_ev_policy_one_trade_per_condition(
        y_true=test_ds.y,
        p_pred=p_test,
        groups=test_ds.group,
        yes_ask=test_ds.yes_ask,
        no_ask=test_ds.no_ask,
        fee_buffer=fee_buffer,
        slippage_buffer=slippage_buffer,
        edge_threshold=0.0,
    )

    test_policy_by_timeframe: Dict[str, Dict[str, float]] = {}
    if condition_market_slug:
        timeframe_idxs: Dict[str, List[int]] = {"5m": [], "15m": [], "other": []}
        for i, cond in enumerate(test_ds.group):
            bucket = timeframe_bucket_from_market_slug(condition_market_slug.get(cond, ""))
            timeframe_idxs[bucket].append(i)

        for bucket, idxs in timeframe_idxs.items():
            if not idxs:
                continue
            y_sub = [test_ds.y[i] for i in idxs]
            p_sub = [p_test[i] for i in idxs]
            g_sub = [test_ds.group[i] for i in idxs]
            ya_sub = [test_ds.yes_ask[i] for i in idxs]
            na_sub = [test_ds.no_ask[i] for i in idxs]
            cond_count = len({test_ds.group[i] for i in idxs})
            policy = evaluate_ev_policy_one_trade_per_condition(
                y_true=y_sub,
                p_pred=p_sub,
                groups=g_sub,
                yes_ask=ya_sub,
                no_ask=na_sub,
                fee_buffer=fee_buffer,
                slippage_buffer=slippage_buffer,
                edge_threshold=selected_edge_threshold,
            )
            policy["conditions"] = float(cond_count)
            test_policy_by_timeframe[bucket] = policy

    metrics = {
        "n_train": len(train_ds.y),
        "n_train_main": len(train_main_ds.y),
        "n_val": len(val_ds.y),
        "n_test": len(test_ds.y),
        "acc_at_0.5": acc_at_05(test_ds.y, p_test),
        "brier": brier(test_ds.y, p_test),
        "log_loss": log_loss(test_ds.y, p_test),
    }
    try:
        from sklearn.metrics import roc_auc_score
        metrics["auc"] = roc_auc_score(test_ds.y, p_test)
    except Exception:
        metrics["auc"] = float("nan")

    os.makedirs(os.path.dirname(onnx_path) or ".", exist_ok=True)
    dummy = torch.zeros((1, seq_len, in_dim), dtype=torch.float32)
    torch.onnx.export(
        model,
        dummy,
        onnx_path,
        input_names=["x_seq"],
        output_names=["p_yes_win"],
        opset_version=opset,
    )

    return {
        "type": "tcn_binary_classifier_binance_threshold",
        "feature_order": FEATURE_ORDER,
        "input_dim": in_dim,
        "sequence_length": seq_len,
        "tcn_channels": channels,
        "tcn_kernel_size": kernel_size,
        "tcn_dropout": dropout,
        "trained_at": datetime.now(timezone.utc).isoformat(),
        "metrics": metrics,
        "edge_policy": {
            "selected_edge_threshold": selected_edge_threshold,
            "selection_objective": "maximize realized_roi_sum on validation (one trade per condition)",
            "min_val_trades": min_val_trades,
            "min_val_trade_rate": min_val_trade_rate,
            "abstain_on_non_positive_val_pnl": 1.0 if abstain_on_non_positive_val_pnl else 0.0,
            "val_policy": val_policy,
            "test_policy": test_policy,
            "test_policy_edge0": test_policy_edge0,
            "test_policy_by_timeframe": test_policy_by_timeframe,
        },
        "cost_model": {
            "fee_buffer": fee_buffer,
            "slippage_buffer": slippage_buffer,
            "cost_buffer_total": fee_buffer + slippage_buffer,
        },
        "label": "y_yes_win (YES settles to 1.0)",
        "note": "Binance L2-only features; Polymarket asks used only for EV backtest.",
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--db-url",
        default=os.environ.get("DATABASE_URL", ""),
        help="Postgres URL (default: env DATABASE_URL)",
    )
    ap.add_argument("--lookback-hours", type=int, default=24 * 14)
    ap.add_argument("--sample-seconds", type=int, default=5)
    ap.add_argument("--market-timeframe", default="all", choices=["all", "5m", "15m"])
    ap.add_argument("--market-asset", default="all", choices=["all", "btc", "eth", "sol", "other"])
    ap.add_argument(
        "--pm-book-source",
        default="ws",
        choices=["obh", "ws"],
        help="Polymarket price source for asks (EV only): obh=clob_orderbook_history_ticks, ws=clob_orderbook_snapshots",
    )
    ap.add_argument("--entry-window-seconds", type=int, default=None)
    ap.add_argument("--entry-window-start-seconds-5m", type=int, default=120)
    ap.add_argument("--entry-window-end-seconds-5m", type=int, default=0)
    ap.add_argument("--entry-window-start-seconds-15m", type=int, default=240)
    ap.add_argument("--entry-window-end-seconds-15m", type=int, default=0)
    ap.add_argument("--entry-window-start-seconds-default", type=int, default=240)
    ap.add_argument("--entry-window-end-seconds-default", type=int, default=30)
    ap.add_argument("--vol-short-window-seconds", type=int, default=240)
    ap.add_argument("--vol-long-window-seconds", type=int, default=840)
    ap.add_argument("--seq-len", type=int, default=48)
    ap.add_argument("--limit", type=int, default=50000)
    ap.add_argument("--test-ratio", type=float, default=0.2)
    ap.add_argument("--min-total-samples", type=int, default=200)
    ap.add_argument("--tcn-channels", default="64,64,64")
    ap.add_argument("--tcn-kernel-size", type=int, default=3)
    ap.add_argument("--tcn-dropout", type=float, default=0.1)
    ap.add_argument("--val-ratio", type=float, default=0.15)
    ap.add_argument("--min-val-samples", type=int, default=256)
    ap.add_argument("--fee-buffer", type=float, default=0.005)
    ap.add_argument("--slippage-buffer", type=float, default=0.005)
    ap.add_argument("--edge-threshold-min", type=float, default=0.0)
    ap.add_argument("--edge-threshold-max", type=float, default=0.12)
    ap.add_argument("--edge-threshold-step", type=float, default=0.002)
    ap.add_argument("--min-val-trades", type=int, default=25)
    ap.add_argument("--min-val-trade-rate", type=float, default=0.05)
    ap.add_argument("--allow-negative-val-pnl", action="store_true")
    ap.add_argument("--epochs", type=int, default=25)
    ap.add_argument("--batch-size", type=int, default=512)
    ap.add_argument("--lr", type=float, default=1e-3)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--opset", type=int, default=17)
    ap.add_argument("--output", default="./models/crypto/binance_threshold_tcn_v1.onnx")
    ap.add_argument("--meta", default="./models/crypto/binance_threshold_tcn_v1.meta.json")
    ap.add_argument("--save-parquet", default=None)
    ap.add_argument("--fetch-only", action="store_true")
    ap.add_argument("--stride", type=int, default=1, help="emit every N-th sequence per group to reduce overlap (default: 1)")
    ap.add_argument("--export-scaler", default=None, help="export feature scaler (offsets/scales) as JSON for config-based normalization")

    args = ap.parse_args()

    if not args.db_url:
        raise SystemExit("--db-url (or env DATABASE_URL) is required")

    channels = [int(s) for s in args.tcn_channels.split(",") if s.strip()]
    if not channels:
        raise SystemExit("--tcn-channels must not be empty")

    if args.entry_window_seconds is not None:
        entry_window_start_seconds = int(args.entry_window_seconds)
        entry_window_end_seconds = 0
    elif args.market_timeframe == "5m":
        entry_window_start_seconds = args.entry_window_start_seconds_5m
        entry_window_end_seconds = args.entry_window_end_seconds_5m
    elif args.market_timeframe == "15m":
        entry_window_start_seconds = args.entry_window_start_seconds_15m
        entry_window_end_seconds = args.entry_window_end_seconds_15m
    else:
        entry_window_start_seconds = args.entry_window_start_seconds_default
        entry_window_end_seconds = args.entry_window_end_seconds_default

    print(
        "Fetching from DB (asset={}, timeframe={}, Binance L2 + settlement; PM asks for EV, entry window [{}s, {}s])...".format(
            args.market_asset,
            args.market_timeframe,
            entry_window_start_seconds,
            entry_window_end_seconds,
        )
    )
    point_ds, export_rows, condition_market_slug = fetch_from_db(
        db_url=args.db_url,
        lookback_hours=args.lookback_hours,
        sample_seconds=args.sample_seconds,
        entry_window_start_seconds=entry_window_start_seconds,
        entry_window_end_seconds=entry_window_end_seconds,
        vol_short_window_seconds=args.vol_short_window_seconds,
        vol_long_window_seconds=args.vol_long_window_seconds,
        market_timeframe=args.market_timeframe,
        market_asset=args.market_asset,
        limit=args.limit,
        pm_book_source=args.pm_book_source,
    )
    print(f"Point rows: {len(point_ds.y)}")
    print_baseline_ev_tables(
        point_ds,
        condition_market_slug,
        fee_buffer=args.fee_buffer,
        slippage_buffer=args.slippage_buffer,
    )

    seq_ds = build_sequences(point_ds, seq_len=args.seq_len, stride=args.stride)
    print(
        "Sequence rows: {} (seq_len={}, feature_dim={})".format(
            len(seq_ds.y), seq_ds.seq_len, seq_ds.feature_dim
        )
    )

    if args.save_parquet:
        maybe_save_parquet(export_rows, args.save_parquet)

    if args.fetch_only:
        print("Fetch-only mode complete.")
        return

    train_ds, test_ds = chronological_split_sequences(
        seq_ds,
        args.test_ratio,
        args.min_total_samples,
    )
    print(f"Split: train={len(train_ds.y)} test={len(test_ds.y)}")

    meta = train_and_export_onnx_tcn(
        train_ds=train_ds,
        test_ds=test_ds,
        condition_market_slug=condition_market_slug,
        channels=channels,
        kernel_size=args.tcn_kernel_size,
        dropout=args.tcn_dropout,
        val_ratio=args.val_ratio,
        min_val_samples=max(1, args.min_val_samples),
        fee_buffer=args.fee_buffer,
        slippage_buffer=args.slippage_buffer,
        edge_threshold_min=args.edge_threshold_min,
        edge_threshold_max=args.edge_threshold_max,
        edge_threshold_step=args.edge_threshold_step,
        min_val_trades=max(0, args.min_val_trades),
        min_val_trade_rate=args.min_val_trade_rate,
        abstain_on_non_positive_val_pnl=not args.allow_negative_val_pnl,
        epochs=args.epochs,
        batch_size=max(1, args.batch_size),
        lr=args.lr,
        seed=args.seed,
        onnx_path=args.output,
        opset=args.opset,
    )
    meta["dataset_config"] = {
        "lookback_hours": args.lookback_hours,
        "sample_seconds": args.sample_seconds,
        "entry_window_start_seconds": entry_window_start_seconds,
        "entry_window_end_seconds": entry_window_end_seconds,
        "vol_short_window_seconds": args.vol_short_window_seconds,
        "vol_long_window_seconds": args.vol_long_window_seconds,
        "market_timeframe": args.market_timeframe,
        "market_asset": args.market_asset,
        "pm_book_source": args.pm_book_source,
        "min_total_samples": args.min_total_samples,
        "min_val_trade_rate": args.min_val_trade_rate,
        "allow_negative_val_pnl": bool(args.allow_negative_val_pnl),
    }

    os.makedirs(os.path.dirname(args.meta) or ".", exist_ok=True)
    with open(args.meta, "w", encoding="utf-8") as f:
        json.dump(meta, f, indent=2)

    # Export scaler for config-based normalization (offset=mean, scale=1/std)
    if args.export_scaler:
        flat_train_rows = [row for seq in train_ds.x for row in seq]
        mean_vals, std_vals = mean_std(flat_train_rows)
        scaler = {
            "feature_names": FEATURE_ORDER,
            "feature_offsets": mean_vals,
            "feature_scales": [1.0 / s if s > 0 else 1.0 for s in std_vals],
        }
        os.makedirs(os.path.dirname(args.export_scaler) or ".", exist_ok=True)
        with open(args.export_scaler, "w") as f:
            json.dump(scaler, f, indent=2)
        print(f"  scaler: {args.export_scaler}")

    m = meta["metrics"]
    print("\nExported:")
    print(f"  onnx: {args.output}")
    print(f"  meta: {args.meta}")
    print(
        f"  metrics: acc@0.5={m['acc_at_0.5']*100:.2f}%  brier={m['brier']:.6f}  ll={m['log_loss']:.6f}  auc={m.get('auc', float('nan')):.4f}"
    )
    ep = meta.get("edge_policy", {})
    tpol = ep.get("test_policy", {})
    print(
        "  edge_policy: threshold={:.4f} trades={} hit={:.2f}% avg_roi={:.2f}% t_stat={:.2f}".format(
            float(ep.get("selected_edge_threshold", 0.0)),
            int(float(tpol.get("trades", 0.0))),
            100.0 * float(tpol.get("hit_rate", 0.0)),
            float(tpol.get("realized_roi_avg_per_trade_pct", 0.0)),
            float(tpol.get("t_stat", 0.0)),
        )
    )

    by_tf = ep.get("test_policy_by_timeframe", {}) or {}
    for tf, pol in by_tf.items():
        print(
            f"  {tf}: trades={int(float(pol.get('trades', 0.0)))} hit={100.0*float(pol.get('hit_rate', 0.0)):.2f}% avg_roi={float(pol.get('realized_roi_avg_per_trade_pct', 0.0)):.2f}%"
        )


if __name__ == "__main__":
    main()
