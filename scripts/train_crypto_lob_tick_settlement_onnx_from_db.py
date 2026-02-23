#!/usr/bin/env python3
"""
Train a TCN to predict final YES settlement outcome from market-state snapshots:
  - Polymarket CLOB orderbook snapshots (YES + NO top-of-book prices)
  - Recent trade-tick context (counts/volume/vwap/last)
  - Official settlement labels from pm_token_settlements

Label:
  y_yes_win = 1 if YES settles to 1.0, else 0.

This script is intentionally market-data based (LOB/tick + settlement), not
execution-intent based.

Install deps on the training machine:
  python3 -m pip install torch psycopg2-binary

Optional (save a Parquet snapshot for reproducibility):
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


FEATURE_ORDER = [
    "yes_best_bid",
    "yes_best_ask",
    "yes_mid",
    "no_best_bid",
    "no_best_ask",
    "no_mid",
    "yes_spread_bps",
    "no_spread_bps",
    "yes_no_mid_gap",
    "yes_last_trade",
    "no_last_trade",
    "yes_trade_count",
    "no_trade_count",
    "yes_trade_volume",
    "no_trade_volume",
    "yes_trade_vwap",
    "no_trade_vwap",
]


@dataclass
class Dataset:
    x: List[List[float]]
    y: List[int]
    ts: List[str]  # RFC3339
    group: List[str]  # condition_id


@dataclass
class SequenceDataset:
    x: List[List[List[float]]]  # [N, L, F]
    y: List[int]
    ts: List[str]
    seq_len: int
    feature_dim: int


def _slice_sequence_dataset(ds: SequenceDataset, idxs: List[int]) -> SequenceDataset:
    return SequenceDataset(
        x=[ds.x[i] for i in idxs],
        y=[ds.y[i] for i in idxs],
        ts=[ds.ts[i] for i in idxs],
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

    idx = list(range(n))
    idx.sort(key=lambda i: ds.ts[i])

    val_n = max(int(n * val_ratio), min_val_samples)
    # Keep at least 10 samples in training side.
    val_n = min(val_n, n - 10)
    val_n = max(1, val_n)

    train_idx = idx[:-val_n]
    val_idx = idx[-val_n:]
    return _slice_sequence_dataset(ds, train_idx), _slice_sequence_dataset(ds, val_idx)


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


def evaluate_ev_policy(
    y_true: List[int],
    p_pred: List[float],
    x_seq: List[List[List[float]]],
    fee_buffer: float,
    slippage_buffer: float,
    edge_threshold: float,
) -> Dict[str, float]:
    """
    Evaluate a simple side-selection policy:
      ev_yes = p_yes - yes_ask - costs
      ev_no  = (1 - p_yes) - no_ask - costs
    take side with higher EV if EV >= edge_threshold.
    """
    total_cost = fee_buffer + slippage_buffer
    trades = 0
    wins = 0
    skipped_bad_price = 0
    sum_pred_ev = 0.0
    total_pnl = 0.0

    for yt, pp, seq in zip(y_true, p_pred, x_seq):
        last = seq[-1]
        # FEATURE_ORDER indices
        yes_ask = float(last[1])
        no_ask = float(last[4])

        if (
            (not math.isfinite(yes_ask))
            or (not math.isfinite(no_ask))
            or yes_ask <= 0.0
            or no_ask <= 0.0
            or yes_ask >= 1.0
            or no_ask >= 1.0
        ):
            skipped_bad_price += 1
            continue

        ev_yes = pp - yes_ask - total_cost
        ev_no = (1.0 - pp) - no_ask - total_cost

        if ev_yes >= ev_no:
            side = "YES"
            best_ev = ev_yes
        else:
            side = "NO"
            best_ev = ev_no

        if best_ev < edge_threshold:
            continue

        trades += 1
        sum_pred_ev += best_ev

        if side == "YES":
            pnl = (1.0 - yes_ask - total_cost) if yt == 1 else (-yes_ask - total_cost)
        else:
            pnl = (1.0 - no_ask - total_cost) if yt == 0 else (-no_ask - total_cost)

        total_pnl += pnl
        if pnl > 0.0:
            wins += 1

    return {
        "edge_threshold": edge_threshold,
        "fee_buffer": fee_buffer,
        "slippage_buffer": slippage_buffer,
        "cost_buffer_total": total_cost,
        "samples": float(len(y_true)),
        "trades": float(trades),
        "trade_rate": (trades / max(1, len(y_true))),
        "hit_rate": (wins / max(1, trades)),
        "predicted_ev_sum": sum_pred_ev,
        "predicted_ev_avg_per_trade": (sum_pred_ev / max(1, trades)),
        "realized_pnl_sum": total_pnl,
        "realized_pnl_avg_per_trade": (total_pnl / max(1, trades)),
        "skipped_bad_price": float(skipped_bad_price),
    }


def select_best_edge_threshold(
    y_val: List[int],
    p_val: List[float],
    x_val: List[List[List[float]]],
    fee_buffer: float,
    slippage_buffer: float,
    threshold_min: float,
    threshold_max: float,
    threshold_step: float,
    min_val_trades: int,
) -> Tuple[float, Dict[str, float]]:
    if threshold_step <= 0.0:
        raise SystemExit("--edge-threshold-step must be > 0")
    if threshold_max < threshold_min:
        raise SystemExit("--edge-threshold-max must be >= --edge-threshold-min")

    thresholds: List[float] = []
    t = threshold_min
    while t <= threshold_max + 1e-12:
        thresholds.append(round(t, 10))
        t += threshold_step

    best_thr = threshold_min
    best_eval: Optional[Dict[str, float]] = None
    best_sum = -1e18
    best_avg = -1e18

    for thr in thresholds:
        ev = evaluate_ev_policy(
            y_true=y_val,
            p_pred=p_val,
            x_seq=x_val,
            fee_buffer=fee_buffer,
            slippage_buffer=slippage_buffer,
            edge_threshold=thr,
        )
        trades = int(ev["trades"])
        pnl_sum = float(ev["realized_pnl_sum"])
        pnl_avg = float(ev["realized_pnl_avg_per_trade"])

        if trades >= min_val_trades and (
            (pnl_sum > best_sum) or (pnl_sum == best_sum and pnl_avg > best_avg)
        ):
            best_sum = pnl_sum
            best_avg = pnl_avg
            best_eval = ev
            best_thr = thr

    if best_eval is None:
        # No threshold produced enough validation trades; abstain.
        abstain_thr = threshold_max + threshold_step
        best_eval = evaluate_ev_policy(
            y_true=y_val,
            p_pred=p_val,
            x_seq=x_val,
            fee_buffer=fee_buffer,
            slippage_buffer=slippage_buffer,
            edge_threshold=abstain_thr,
        )
        best_eval["abstain_recommended"] = 1.0
        best_eval["abstain_reason"] = "insufficient_validation_trades"
        best_thr = abstain_thr
        return best_thr, best_eval

    # Safety gate: only deploy when validation realized PnL is clearly positive.
    if (
        float(best_eval.get("realized_pnl_sum", 0.0)) <= 0.0
        or float(best_eval.get("realized_pnl_avg_per_trade", 0.0)) <= 0.0
    ):
        abstain_thr = threshold_max + threshold_step
        best_eval = evaluate_ev_policy(
            y_true=y_val,
            p_pred=p_val,
            x_seq=x_val,
            fee_buffer=fee_buffer,
            slippage_buffer=slippage_buffer,
            edge_threshold=abstain_thr,
        )
        best_eval["abstain_recommended"] = 1.0
        best_eval["abstain_reason"] = "non_positive_validation_pnl"
        best_thr = abstain_thr

    return best_thr, best_eval


def chronological_split_sequences(
    ds: SequenceDataset,
    test_ratio: float,
) -> Tuple[SequenceDataset, SequenceDataset]:
    n = len(ds.y)
    if n < 100:
        raise SystemExit(f"dataset too small: n={n} (need >=100)")
    if not (0.05 <= test_ratio <= 0.5):
        raise SystemExit("--test-ratio must be in [0.05, 0.5]")

    idx = list(range(n))
    idx.sort(key=lambda i: ds.ts[i])

    cut = int((1.0 - test_ratio) * n)
    cut = max(1, min(cut, n - 1))

    def take(idxs: List[int]) -> SequenceDataset:
        return SequenceDataset(
            x=[ds.x[i] for i in idxs],
            y=[ds.y[i] for i in idxs],
            ts=[ds.ts[i] for i in idxs],
            seq_len=ds.seq_len,
            feature_dim=ds.feature_dim,
        )

    return take(idx[:cut]), take(idx[cut:])


def build_sequences(ds: Dataset, seq_len: int) -> SequenceDataset:
    if seq_len <= 0:
        raise SystemExit("--seq-len must be > 0")
    if not ds.x:
        raise SystemExit("no rows in point dataset")

    idx = list(range(len(ds.y)))
    idx.sort(key=lambda i: ds.ts[i])

    history_by_group: Dict[str, List[List[float]]] = {}
    x_seq: List[List[List[float]]] = []
    y: List[int] = []
    ts: List[str] = []

    for i in idx:
        g = ds.group[i]
        hist = history_by_group.setdefault(g, [])
        hist.append(ds.x[i])

        if len(hist) >= seq_len:
            seq = hist[-seq_len:]
        else:
            pad = [hist[0]] * (seq_len - len(hist))
            seq = pad + hist

        x_seq.append([row[:] for row in seq])
        y.append(ds.y[i])
        ts.append(ds.ts[i])

    return SequenceDataset(
        x=x_seq,
        y=y,
        ts=ts,
        seq_len=seq_len,
        feature_dim=len(ds.x[0]),
    )


def _mid_price(best_bid: Optional[float], best_ask: Optional[float]) -> Optional[float]:
    if best_bid is not None and best_ask is not None and best_bid > 0.0 and best_ask > 0.0:
        return 0.5 * (best_bid + best_ask)
    if best_bid is not None and best_bid > 0.0:
        return best_bid
    if best_ask is not None and best_ask > 0.0:
        return best_ask
    return None


def _spread_bps(best_bid: Optional[float], best_ask: Optional[float], mid: Optional[float]) -> float:
    if (
        best_bid is None
        or best_ask is None
        or mid is None
        or best_bid <= 0.0
        or best_ask <= 0.0
        or mid <= 0.0
        or best_ask < best_bid
    ):
        return 0.0
    return ((best_ask - best_bid) / mid) * 10_000.0


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


def fetch_from_db(
    db_url: str,
    lookback_hours: int,
    sample_seconds: int,
    pair_window_seconds: int,
    trade_lookback_seconds: int,
    limit: int,
) -> Tuple[Dataset, List[Dict[str, object]]]:
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
    if pair_window_seconds <= 0:
        raise SystemExit("--pair-window-seconds must be > 0")
    if trade_lookback_seconds <= 0:
        raise SystemExit("--trade-lookback-seconds must be > 0")
    if limit <= 0:
        raise SystemExit("--limit must be > 0")

    sql = """
    WITH settled AS (
      SELECT
        condition_id,
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
        )::double precision AS no_settled_price
      FROM pm_token_settlements
      WHERE resolved = TRUE
        AND settled_price IS NOT NULL
        AND condition_id IS NOT NULL
      GROUP BY condition_id
      HAVING
        MAX(token_id) FILTER (
          WHERE LOWER(TRIM(COALESCE(outcome, ''))) IN ('yes', 'up', 'higher', 'above', 'true')
        ) IS NOT NULL
        AND MAX(token_id) FILTER (
          WHERE LOWER(TRIM(COALESCE(outcome, ''))) IN ('no', 'down', 'lower', 'below', 'false')
        ) IS NOT NULL
    ),
    yes_raw AS (
      SELECT
        st.condition_id,
        st.yes_token_id,
        st.no_token_id,
        st.yes_settled_price,
        st.no_settled_price,
        s.received_at AS ts,
        (s.bids->0->>'price')::double precision AS yes_best_bid,
        (s.asks->0->>'price')::double precision AS yes_best_ask,
        ROW_NUMBER() OVER (
          PARTITION BY st.condition_id, FLOOR(EXTRACT(EPOCH FROM s.received_at) / %s)
          ORDER BY s.received_at ASC
        ) AS rn
      FROM settled st
      JOIN clob_orderbook_snapshots s
        ON s.token_id = st.yes_token_id
      WHERE LOWER(COALESCE(s.domain, '')) = 'crypto'
        AND s.received_at >= NOW() - (%s::bigint * INTERVAL '1 hour')
    ),
    yes_snap AS (
      SELECT *
      FROM yes_raw
      WHERE rn = 1
      ORDER BY ts ASC
      LIMIT %s
    ),
    paired AS (
      SELECT
        y.condition_id,
        y.yes_token_id,
        y.no_token_id,
        y.ts,
        y.yes_best_bid,
        y.yes_best_ask,
        y.yes_settled_price,
        y.no_settled_price,
        (n.bids->0->>'price')::double precision AS no_best_bid,
        (n.asks->0->>'price')::double precision AS no_best_ask
      FROM yes_snap y
      JOIN LATERAL (
        SELECT s2.*
        FROM clob_orderbook_snapshots s2
        WHERE s2.token_id = y.no_token_id
          AND s2.received_at BETWEEN y.ts - (%s::bigint * INTERVAL '1 second')
                                AND y.ts + (%s::bigint * INTERVAL '1 second')
        ORDER BY ABS(EXTRACT(EPOCH FROM (s2.received_at - y.ts))) ASC
        LIMIT 1
      ) n ON TRUE
    )
    SELECT
      p.ts,
      p.condition_id,
      p.yes_token_id,
      p.no_token_id,
      p.yes_best_bid,
      p.yes_best_ask,
      p.no_best_bid,
      p.no_best_ask,
      yt.last_price AS yes_last_trade,
      yt.cnt AS yes_trade_count,
      yt.vol AS yes_trade_volume,
      yt.vwap AS yes_trade_vwap,
      nt.last_price AS no_last_trade,
      nt.cnt AS no_trade_count,
      nt.vol AS no_trade_volume,
      nt.vwap AS no_trade_vwap,
      p.yes_settled_price,
      p.no_settled_price
    FROM paired p
    LEFT JOIN LATERAL (
      SELECT
        COUNT(*)::double precision AS cnt,
        COALESCE(SUM(size), 0)::double precision AS vol,
        COALESCE(SUM(price * size) / NULLIF(SUM(size), 0), NULL)::double precision AS vwap,
        (ARRAY_AGG(price ORDER BY trade_ts DESC))[1]::double precision AS last_price
      FROM clob_trade_ticks t
      WHERE t.token_id = p.yes_token_id
        AND t.trade_ts <= p.ts
        AND t.trade_ts > p.ts - (%s::bigint * INTERVAL '1 second')
    ) yt ON TRUE
    LEFT JOIN LATERAL (
      SELECT
        COUNT(*)::double precision AS cnt,
        COALESCE(SUM(size), 0)::double precision AS vol,
        COALESCE(SUM(price * size) / NULLIF(SUM(size), 0), NULL)::double precision AS vwap,
        (ARRAY_AGG(price ORDER BY trade_ts DESC))[1]::double precision AS last_price
      FROM clob_trade_ticks t
      WHERE t.token_id = p.no_token_id
        AND t.trade_ts <= p.ts
        AND t.trade_ts > p.ts - (%s::bigint * INTERVAL '1 second')
    ) nt ON TRUE
    ORDER BY p.ts ASC
    """

    params = [
        sample_seconds,
        lookback_hours,
        limit,
        pair_window_seconds,
        pair_window_seconds,
        trade_lookback_seconds,
        trade_lookback_seconds,
    ]

    x: List[List[float]] = []
    y: List[int] = []
    ts: List[str] = []
    group: List[str] = []
    exported_rows: List[Dict[str, object]] = []

    skipped_bad_book = 0
    skipped_bad_label = 0
    skipped_nonfinite = 0

    conn = psycopg2.connect(db_url)
    try:
        with conn.cursor() as cur:
            cur.execute(sql, params)
            for row in cur.fetchall():
                ts_raw = row[0]
                condition_id = row[1]
                yes_token_id = row[2]
                no_token_id = row[3]
                yes_best_bid = _to_float(row[4])
                yes_best_ask = _to_float(row[5])
                no_best_bid = _to_float(row[6])
                no_best_ask = _to_float(row[7])
                yes_last_trade = _to_float(row[8])
                yes_trade_count = _to_float(row[9]) or 0.0
                yes_trade_volume = _to_float(row[10]) or 0.0
                yes_trade_vwap = _to_float(row[11])
                no_last_trade = _to_float(row[12])
                no_trade_count = _to_float(row[13]) or 0.0
                no_trade_volume = _to_float(row[14]) or 0.0
                no_trade_vwap = _to_float(row[15])
                yes_settled = _to_float(row[16])
                no_settled = _to_float(row[17])

                yes_mid = _mid_price(yes_best_bid, yes_best_ask)
                no_mid = _mid_price(no_best_bid, no_best_ask)
                if yes_mid is None or no_mid is None:
                    skipped_bad_book += 1
                    continue

                if yes_settled is None:
                    skipped_bad_label += 1
                    continue
                y_yes_win = 1 if yes_settled > 0.5 else 0

                yes_spread_bps = _spread_bps(yes_best_bid, yes_best_ask, yes_mid)
                no_spread_bps = _spread_bps(no_best_bid, no_best_ask, no_mid)
                yes_no_mid_gap = (yes_mid + no_mid) - 1.0

                # If recent trade window is empty, backfill trade-derived features with mid.
                yes_last = yes_last_trade if yes_last_trade is not None else yes_mid
                no_last = no_last_trade if no_last_trade is not None else no_mid
                yes_vwap = yes_trade_vwap if yes_trade_vwap is not None else yes_mid
                no_vwap = no_trade_vwap if no_trade_vwap is not None else no_mid

                feats = [
                    yes_best_bid if yes_best_bid is not None else yes_mid,
                    yes_best_ask if yes_best_ask is not None else yes_mid,
                    yes_mid,
                    no_best_bid if no_best_bid is not None else no_mid,
                    no_best_ask if no_best_ask is not None else no_mid,
                    no_mid,
                    yes_spread_bps,
                    no_spread_bps,
                    yes_no_mid_gap,
                    yes_last,
                    no_last,
                    yes_trade_count,
                    no_trade_count,
                    yes_trade_volume,
                    no_trade_volume,
                    yes_vwap,
                    no_vwap,
                ]

                if any((v is None or (not math.isfinite(v))) for v in feats):
                    skipped_nonfinite += 1
                    continue

                ts_iso = _to_rfc3339(ts_raw)
                x.append([float(v) for v in feats])
                y.append(y_yes_win)
                ts.append(ts_iso)
                group.append(str(condition_id))

                exported_rows.append(
                    {
                        "ts": ts_iso,
                        "condition_id": condition_id,
                        "yes_token_id": yes_token_id,
                        "no_token_id": no_token_id,
                        "yes_best_bid": float(feats[0]),
                        "yes_best_ask": float(feats[1]),
                        "no_best_bid": float(feats[3]),
                        "no_best_ask": float(feats[4]),
                        "yes_settled_price": yes_settled,
                        "no_settled_price": no_settled,
                        "yes_settlement_success": y_yes_win,
                        "no_settlement_success": 1 - y_yes_win,
                    }
                )
    finally:
        conn.close()

    if not x:
        raise SystemExit("no usable rows fetched")

    print(
        "[fetch] usable_rows={} skipped_bad_book={} skipped_bad_label={} skipped_nonfinite={}".format(
            len(x),
            skipped_bad_book,
            skipped_bad_label,
            skipped_nonfinite,
        )
    )
    print(
        "[fetch] class_balance yes_win={} no_win={}".format(
            sum(y),
            len(y) - sum(y),
        )
    )

    return Dataset(x=x, y=y, ts=ts, group=group), exported_rows


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


def train_and_export_onnx_tcn(
    train_ds: SequenceDataset,
    test_ds: SequenceDataset,
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
            self.skip = nn.Conv1d(in_ch, out_ch, kernel_size=1) if in_ch != out_ch else nn.Identity()

        def forward(self, x):
            y = self.conv1(x)
            y = self.relu(y)
            y = self.drop(y)
            y = self.conv2(y)
            y = self.relu(y)
            y = self.drop(y)
            return self.relu(y + self.skip(x))

    class LobTickSettleTCN(nn.Module):
        def __init__(self, mean_f, std_f, tcn_channels, k, p_drop):
            super().__init__()
            self.register_buffer(
                "mean",
                torch.tensor(mean_f, dtype=torch.float32).view(1, 1, -1),
            )
            self.register_buffer(
                "std",
                torch.tensor(std_f, dtype=torch.float32).view(1, 1, -1),
            )

            blocks = []
            prev_ch = in_dim
            dilation = 1
            for ch in tcn_channels:
                blocks.append(TcnResidualBlock(prev_ch, ch, k, dilation, p_drop))
                prev_ch = ch
                dilation *= 2
            self.tcn = nn.Sequential(*blocks)
            self.head = nn.Linear(prev_ch, 1)

        def forward(self, x):
            # x: [B, L, F]
            x = (x - self.mean) / self.std
            x = x.transpose(1, 2)  # [B, F, L]
            h = self.tcn(x)
            last = h[:, :, -1]
            logits = self.head(last)
            return torch.sigmoid(logits)

    model = LobTickSettleTCN(mean_vec, std_vec, channels, kernel_size, dropout)
    opt = torch.optim.Adam(model.parameters(), lr=lr)
    loss_fn = nn.BCELoss()

    xtr = torch.tensor(train_main_ds.x, dtype=torch.float32)
    ytr = torch.tensor(train_main_ds.y, dtype=torch.float32).view(-1, 1)
    xval = torch.tensor(val_ds.x, dtype=torch.float32)
    xte = torch.tensor(test_ds.x, dtype=torch.float32)

    n = xtr.shape[0]
    idxs = list(range(n))

    for epoch in range(1, epochs + 1):
        model.train()
        random.shuffle(idxs)

        total_loss = 0.0
        for start in range(0, n, batch_size):
            bidx = idxs[start : start + batch_size]
            xb = xtr[bidx]
            yb = ytr[bidx]

            opt.zero_grad()
            p = model(xb)
            loss = loss_fn(p, yb)
            loss.backward()
            opt.step()

            total_loss += float(loss.detach().cpu().item()) * len(bidx)

        if epoch == 1 or epoch == epochs or epoch % max(1, epochs // 5) == 0:
            model.eval()
            with torch.no_grad():
                p_val = model(xval).cpu().numpy().reshape(-1).tolist()
            print(
                f"epoch {epoch:>3}/{epochs}  "
                f"train_loss={total_loss/max(1,n):.6f}  "
                f"val_acc@0.5={acc_at_05(val_ds.y, p_val)*100:.2f}%"
            )

    model.eval()
    with torch.no_grad():
        p_val = model(xval).cpu().numpy().reshape(-1).tolist()
        p_test = model(xte).cpu().numpy().reshape(-1).tolist()

    selected_edge_threshold, val_policy = select_best_edge_threshold(
        y_val=val_ds.y,
        p_val=p_val,
        x_val=val_ds.x,
        fee_buffer=fee_buffer,
        slippage_buffer=slippage_buffer,
        threshold_min=edge_threshold_min,
        threshold_max=edge_threshold_max,
        threshold_step=edge_threshold_step,
        min_val_trades=min_val_trades,
    )

    test_policy = evaluate_ev_policy(
        y_true=test_ds.y,
        p_pred=p_test,
        x_seq=test_ds.x,
        fee_buffer=fee_buffer,
        slippage_buffer=slippage_buffer,
        edge_threshold=selected_edge_threshold,
    )
    test_policy_edge0 = evaluate_ev_policy(
        y_true=test_ds.y,
        p_pred=p_test,
        x_seq=test_ds.x,
        fee_buffer=fee_buffer,
        slippage_buffer=slippage_buffer,
        edge_threshold=0.0,
    )

    metrics = {
        "n_train": len(train_ds.y),
        "n_train_main": len(train_main_ds.y),
        "n_val": len(val_ds.y),
        "n_test": len(test_ds.y),
        "acc_at_0.5": acc_at_05(test_ds.y, p_test),
        "brier": brier(test_ds.y, p_test),
        "log_loss": log_loss(test_ds.y, p_test),
    }

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
        "type": "tcn_binary_classifier_lob_tick_settlement",
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
            "selection_objective": "maximize realized_pnl_sum on validation (min trades gate)",
            "min_val_trades": min_val_trades,
            "val_policy": val_policy,
            "test_policy": test_policy,
            "test_policy_edge0": test_policy_edge0,
        },
        "cost_model": {
            "fee_buffer": fee_buffer,
            "slippage_buffer": slippage_buffer,
            "cost_buffer_total": fee_buffer + slippage_buffer,
        },
        "label": "y_yes_win (YES settles to 1.0)",
        "note": "TCN with embedded z-score normalization in ONNX graph.",
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--db-url",
        default=os.environ.get("DATABASE_URL", "postgres://localhost/ploy"),
        help="Postgres URL (or env DATABASE_URL)",
    )
    ap.add_argument("--lookback-hours", type=int, default=336)
    ap.add_argument("--sample-seconds", type=int, default=30)
    ap.add_argument("--pair-window-seconds", type=int, default=2)
    ap.add_argument("--trade-lookback-seconds", type=int, default=60)
    ap.add_argument("--seq-len", type=int, default=32)
    ap.add_argument("--limit", type=int, default=50000)
    ap.add_argument("--test-ratio", type=float, default=0.2)
    ap.add_argument("--tcn-channels", default="64,64,64")
    ap.add_argument("--tcn-kernel-size", type=int, default=3)
    ap.add_argument("--tcn-dropout", type=float, default=0.1)
    ap.add_argument("--val-ratio", type=float, default=0.15)
    ap.add_argument("--min-val-samples", type=int, default=256)
    ap.add_argument("--fee-buffer", type=float, default=0.005)
    ap.add_argument("--slippage-buffer", type=float, default=0.005)
    ap.add_argument("--edge-threshold-min", type=float, default=0.0)
    ap.add_argument("--edge-threshold-max", type=float, default=0.08)
    ap.add_argument("--edge-threshold-step", type=float, default=0.002)
    ap.add_argument("--min-val-trades", type=int, default=100)
    ap.add_argument("--epochs", type=int, default=25)
    ap.add_argument("--batch-size", type=int, default=1024)
    ap.add_argument("--lr", type=float, default=1e-3)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--opset", type=int, default=17)
    ap.add_argument("--output", default="./models/crypto/lob_tick_settle_tcn_v1.onnx")
    ap.add_argument("--meta", default="./models/crypto/lob_tick_settle_tcn_v1.meta.json")
    ap.add_argument(
        "--save-parquet",
        default=None,
        help="optional parquet path for fetched rows (includes yes/no prices + settlement success)",
    )
    ap.add_argument(
        "--fetch-only",
        action="store_true",
        help="only fetch/validate dataset and optionally save parquet; skip training",
    )

    args = ap.parse_args()

    channels = [int(s) for s in args.tcn_channels.split(",") if s.strip()]
    if not channels:
        raise SystemExit("--tcn-channels must not be empty")

    print("Fetching from DB (LOB/tick + settlement)...")
    point_ds, export_rows = fetch_from_db(
        db_url=args.db_url,
        lookback_hours=args.lookback_hours,
        sample_seconds=args.sample_seconds,
        pair_window_seconds=args.pair_window_seconds,
        trade_lookback_seconds=args.trade_lookback_seconds,
        limit=args.limit,
    )
    print(f"Point rows: {len(point_ds.y)}")

    seq_ds = build_sequences(point_ds, seq_len=args.seq_len)
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

    train_ds, test_ds = chronological_split_sequences(seq_ds, args.test_ratio)
    print(f"Split: train={len(train_ds.y)} test={len(test_ds.y)}")

    meta = train_and_export_onnx_tcn(
        train_ds=train_ds,
        test_ds=test_ds,
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
        epochs=args.epochs,
        batch_size=max(1, args.batch_size),
        lr=args.lr,
        seed=args.seed,
        onnx_path=args.output,
        opset=args.opset,
    )

    os.makedirs(os.path.dirname(args.meta) or ".", exist_ok=True)
    with open(args.meta, "w", encoding="utf-8") as f:
        json.dump(meta, f, indent=2)

    m = meta["metrics"]
    print("\nExported:")
    print(f"  onnx: {args.output}")
    print(f"  meta: {args.meta}")
    print(
        f"  metrics: acc@0.5={m['acc_at_0.5']*100:.2f}%  brier={m['brier']:.6f}  ll={m['log_loss']:.6f}"
    )
    ep = meta.get("edge_policy", {})
    tpol = ep.get("test_policy", {})
    vpol = ep.get("val_policy", {})
    print(
        "  edge_policy: threshold={:.4f} trades={} hit={:.2f}% avg_pnl={:.5f} pnl_sum={:.3f}".format(
            float(ep.get("selected_edge_threshold", 0.0)),
            int(float(tpol.get("trades", 0.0))),
            100.0 * float(tpol.get("hit_rate", 0.0)),
            float(tpol.get("realized_pnl_avg_per_trade", 0.0)),
            float(tpol.get("realized_pnl_sum", 0.0)),
        )
    )
    if int(float(vpol.get("abstain_recommended", 0.0))) == 1:
        print(
            "  edge_policy_gate: ABSTAIN ({})".format(
                str(vpol.get("abstain_reason", "unspecified"))
            )
        )


if __name__ == "__main__":
    main()
