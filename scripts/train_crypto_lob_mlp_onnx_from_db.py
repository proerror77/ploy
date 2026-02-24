#!/usr/bin/env python3
"""
DEPRECATED: prefer `scripts/train_crypto_lob_tcn_onnx_from_db.py`.

Train a small MLP (DL) to predict y_up (Polymarket official settlement) from
Binance LOB-derived features, and export the model to ONNX.

This script reads directly from Postgres (no dataset file required).
Default source is `sync_records + pm_token_settlements`, with optional
`--horizon 5m|15m` to train per-window models.

Legacy source (`agent_order_executions`) is still available via:
  --source order_executions

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
from typing import List, Optional, Tuple


FEATURE_ORDER = [
    "obi5",
    "obi10",
    "spread_bps",
    "bid_volume_5",
    "ask_volume_5",
    "momentum_1s",
    "momentum_5s",
]


@dataclass
class Dataset:
    x: List[List[float]]
    y: List[int]
    ts: List[str]  # RFC3339


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


def zscore(x: List[List[float]], mean: List[float], std: List[float]) -> List[List[float]]:
    out: List[List[float]] = []
    for row in x:
        out.append([(row[j] - mean[j]) / std[j] for j in range(len(row))])
    return out


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


def chronological_split(ds: Dataset, test_ratio: float) -> Tuple[Dataset, Dataset]:
    n = len(ds.y)
    if n < 100:
        raise SystemExit(f"dataset too small: n={n} (need >=100)")
    if not (0.05 <= test_ratio <= 0.5):
        raise SystemExit("--test-ratio must be in [0.05, 0.5]")

    idx = list(range(n))
    idx.sort(key=lambda i: ds.ts[i])

    cut = int((1.0 - test_ratio) * n)
    cut = max(1, min(cut, n - 1))

    def take(idxs: List[int]) -> Dataset:
        return Dataset(
            x=[ds.x[i] for i in idxs],
            y=[ds.y[i] for i in idxs],
            ts=[ds.ts[i] for i in idxs],
        )

    return take(idx[:cut]), take(idx[cut:])


def fetch_from_order_executions(
    db_url: str,
    lookback_hours: int,
    account_id: Optional[str],
    agent_id: Optional[str],
    live_only: bool,
    limit: int,
) -> Dataset:
    try:
        import psycopg2  # type: ignore
    except Exception as e:
        raise SystemExit(
            "psycopg2 is required.\n"
            "Install: python3 -m pip install psycopg2-binary\n"
            f"Import error: {e}"
        )

    if limit <= 0:
        raise SystemExit("--limit must be > 0")

    where = [
        "e.executed_at >= NOW() - (%s::bigint * INTERVAL '1 hour')",
        "e.filled_shares > 0",
        "LOWER(e.domain) = 'crypto'",
        "s.resolved = TRUE",
        "s.settled_price IS NOT NULL",
        # Prefer entry intents
        "((e.metadata ? 'signal_type' AND RIGHT(e.metadata->>'signal_type', 6) = '_entry') OR (NOT (e.metadata ? 'signal_type') AND e.is_buy = TRUE))",
        # Require feature keys so casts do not throw
        "e.metadata ? 'lob_obi_5'",
        "e.metadata ? 'lob_obi_10'",
        "e.metadata ? 'lob_spread_bps'",
        "e.metadata ? 'lob_bid_volume_5'",
        "e.metadata ? 'lob_ask_volume_5'",
        "e.metadata ? 'signal_momentum_1s'",
        "e.metadata ? 'signal_momentum_5s'",
    ]
    params: List[object] = [lookback_hours]

    if account_id:
        where.append("e.account_id = %s")
        params.append(account_id)
    if agent_id:
        where.append("e.agent_id = %s")
        params.append(agent_id)
    if live_only:
        where.append("e.dry_run = FALSE")

    sql = f"""
    SELECT
      e.executed_at,
      (e.metadata->>'lob_obi_5')::double precision as obi5,
      (e.metadata->>'lob_obi_10')::double precision as obi10,
      (e.metadata->>'lob_spread_bps')::double precision as spread_bps,
      (e.metadata->>'lob_bid_volume_5')::double precision as bid_volume_5,
      (e.metadata->>'lob_ask_volume_5')::double precision as ask_volume_5,
      (e.metadata->>'signal_momentum_1s')::double precision as momentum_1s,
      (e.metadata->>'signal_momentum_5s')::double precision as momentum_5s,
      CASE
        WHEN e.market_side = 'UP' THEN CASE WHEN s.settled_price > 0.5 THEN 1 ELSE 0 END
        WHEN e.market_side = 'DOWN' THEN CASE WHEN s.settled_price > 0.5 THEN 0 ELSE 1 END
        ELSE NULL
      END as y_up
    FROM agent_order_executions e
    JOIN pm_token_settlements s
      ON s.token_id = e.token_id
    WHERE {" AND ".join(where)}
    ORDER BY e.executed_at ASC
    LIMIT {int(limit)}
    """

    x: List[List[float]] = []
    y: List[int] = []
    ts: List[str] = []

    conn = psycopg2.connect(db_url)
    try:
        with conn.cursor() as cur:
            cur.execute(sql, params)
            for row in cur.fetchall():
                executed_at = row[0]
                feats = list(row[1:8])
                yi = row[8]
                if yi not in (0, 1):
                    continue
                if any(v is None or (isinstance(v, float) and not math.isfinite(v)) for v in feats):
                    continue
                x.append([float(v) for v in feats])
                y.append(int(yi))
                if hasattr(executed_at, "isoformat"):
                    ts.append(executed_at.replace(tzinfo=timezone.utc).isoformat())
                else:
                    ts.append(str(executed_at))
    finally:
        conn.close()

    if not x:
        raise SystemExit("no usable rows fetched")
    return Dataset(x=x, y=y, ts=ts)


def fetch_from_sync_records(
    db_url: str,
    lookback_hours: int,
    symbol: Optional[str],
    horizon: Optional[str],
    limit: int,
) -> Dataset:
    try:
        import psycopg2  # type: ignore
    except Exception as e:
        raise SystemExit(
            "psycopg2 is required.\n"
            "Install: python3 -m pip install psycopg2-binary\n"
            f"Import error: {e}"
        )

    if limit <= 0:
        raise SystemExit("--limit must be > 0")

    horizon_norm: Optional[str] = None
    if horizon is not None:
        h = horizon.strip().lower()
        if h not in ("5m", "15m"):
            raise SystemExit("--horizon must be one of: 5m, 15m")
        horizon_norm = h

    where = [
        "sr.timestamp >= NOW() - (%s::bigint * INTERVAL '1 hour')",
        "sr.pm_market_slug IS NOT NULL",
        "LOWER(sr.pm_market_slug) LIKE '%up%'",
        "LOWER(sr.pm_market_slug) LIKE '%down%'",
        """(
            (sr.symbol = 'BTCUSDT' AND (LOWER(sr.pm_market_slug) LIKE '%btc%' OR LOWER(sr.pm_market_slug) LIKE '%bitcoin%')) OR
            (sr.symbol = 'ETHUSDT' AND (LOWER(sr.pm_market_slug) LIKE '%eth%' OR LOWER(sr.pm_market_slug) LIKE '%ethereum%')) OR
            (sr.symbol = 'SOLUSDT' AND (LOWER(sr.pm_market_slug) LIKE '%sol%' OR LOWER(sr.pm_market_slug) LIKE '%solana%')) OR
            (sr.symbol = 'XRPUSDT' AND (LOWER(sr.pm_market_slug) LIKE '%xrp%' OR LOWER(sr.pm_market_slug) LIKE '%ripple%'))
        )""",
        "ml.y_up IS NOT NULL",
        "ml.horizon IS NOT NULL",
        "sr.timestamp <= COALESCE(ml.resolved_at, NOW())",
        "sr.bn_obi_5 IS NOT NULL",
        "sr.bn_obi_10 IS NOT NULL",
        "sr.bn_spread_bps IS NOT NULL",
        "sr.bn_bid_volume IS NOT NULL",
        "sr.bn_ask_volume IS NOT NULL",
        "sr.bn_price_change_1s IS NOT NULL",
        "sr.bn_price_change_5s IS NOT NULL",
    ]
    params: List[object] = [lookback_hours]

    if symbol:
        where.append("sr.symbol = %s")
        params.append(symbol.strip().upper())
    if horizon_norm:
        where.append("ml.horizon = %s")
        params.append(horizon_norm)

    sql = f"""
    WITH market_labels AS (
      SELECT
        market_slug,
        MAX(resolved_at) AS resolved_at,
        CASE
          WHEN SUM(CASE WHEN LOWER(outcome) LIKE '%up%' THEN 1 ELSE 0 END) > 0 THEN
            MAX(CASE WHEN LOWER(outcome) LIKE '%up%' THEN CASE WHEN settled_price > 0.5 THEN 1 ELSE 0 END END)
          WHEN SUM(CASE WHEN LOWER(outcome) IN ('yes', 'true') THEN 1 ELSE 0 END) > 0 THEN
            MAX(CASE WHEN LOWER(outcome) IN ('yes', 'true') THEN CASE WHEN settled_price > 0.5 THEN 1 ELSE 0 END END)
          WHEN SUM(CASE WHEN LOWER(outcome) LIKE '%down%' THEN 1 ELSE 0 END) > 0 THEN
            MAX(CASE WHEN LOWER(outcome) LIKE '%down%' THEN CASE WHEN settled_price > 0.5 THEN 0 ELSE 1 END END)
          WHEN SUM(CASE WHEN LOWER(outcome) IN ('no', 'false') THEN 1 ELSE 0 END) > 0 THEN
            MAX(CASE WHEN LOWER(outcome) IN ('no', 'false') THEN CASE WHEN settled_price > 0.5 THEN 0 ELSE 1 END END)
          ELSE NULL
        END AS y_up,
        CASE
          WHEN market_slug ILIKE '%15m%' OR market_slug ILIKE '%15-minute%' OR market_slug ILIKE '%15_minute%' THEN '15m'
          WHEN market_slug ILIKE '%5m%' OR market_slug ILIKE '%5-minute%' OR market_slug ILIKE '%5_minute%' THEN '5m'
          ELSE NULL
        END AS horizon
      FROM pm_token_settlements
      WHERE resolved = TRUE
        AND settled_price IS NOT NULL
        AND market_slug IS NOT NULL
      GROUP BY market_slug
    )
    SELECT
      sr.timestamp,
      sr.bn_obi_5::double precision AS obi5,
      sr.bn_obi_10::double precision AS obi10,
      sr.bn_spread_bps::double precision AS spread_bps,
      sr.bn_bid_volume::double precision AS bid_volume_5,
      sr.bn_ask_volume::double precision AS ask_volume_5,
      sr.bn_price_change_1s::double precision AS momentum_1s,
      sr.bn_price_change_5s::double precision AS momentum_5s,
      ml.y_up
    FROM sync_records sr
    JOIN market_labels ml
      ON ml.market_slug = sr.pm_market_slug
    WHERE {" AND ".join(where)}
    ORDER BY sr.timestamp ASC
    LIMIT {int(limit)}
    """

    x: List[List[float]] = []
    y: List[int] = []
    ts: List[str] = []

    conn = psycopg2.connect(db_url)
    try:
        with conn.cursor() as cur:
            cur.execute(sql, params)
            for row in cur.fetchall():
                timestamp = row[0]
                feats = list(row[1:8])
                yi = row[8]
                if yi not in (0, 1):
                    continue
                if any(v is None or (isinstance(v, float) and not math.isfinite(v)) for v in feats):
                    continue
                x.append([float(v) for v in feats])
                y.append(int(yi))
                if hasattr(timestamp, "isoformat"):
                    ts.append(timestamp.replace(tzinfo=timezone.utc).isoformat())
                else:
                    ts.append(str(timestamp))
    finally:
        conn.close()

    if not x:
        raise SystemExit("no usable rows fetched from sync_records")
    return Dataset(x=x, y=y, ts=ts)


def maybe_save_parquet(ds: Dataset, path: str) -> None:
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

    rows = []
    for feats, yi, ts in zip(ds.x, ds.y, ds.ts):
        r = {"executed_at": ts, "y_up": yi}
        for k, v in zip(FEATURE_ORDER, feats):
            r[k] = v
        rows.append(r)

    df = pd.DataFrame(rows)
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    df.to_parquet(path, index=False)
    print(f"Saved parquet snapshot: {path} (rows={len(df)})")


def train_and_export_onnx(
    train_ds: Dataset,
    test_ds: Dataset,
    hidden: List[int],
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
    except Exception as e:
        raise SystemExit(
            "PyTorch is required for training.\n"
            "Install: python3 -m pip install torch\n"
            f"Import error: {e}"
        )

    torch.manual_seed(seed)
    random.seed(seed)

    in_dim = len(train_ds.x[0])
    mean, std = mean_std(train_ds.x)
    x_train = zscore(train_ds.x, mean, std)
    x_test = zscore(test_ds.x, mean, std)

    class LobMLP(nn.Module):
        def __init__(self, mean_vec, std_vec, hidden_dims):
            super().__init__()
            # Embed normalization in the ONNX graph.
            self.register_buffer("mean", torch.tensor(mean_vec, dtype=torch.float32))
            self.register_buffer("std", torch.tensor(std_vec, dtype=torch.float32))
            layers = []
            prev = in_dim
            for h in hidden_dims:
                layers.append(nn.Linear(prev, h))
                layers.append(nn.ReLU())
                prev = h
            layers.append(nn.Linear(prev, 1))  # logits
            self.net = nn.Sequential(*layers)

        def forward(self, x):
            x = (x - self.mean) / self.std
            logits = self.net(x)
            return torch.sigmoid(logits)

    model = LobMLP(mean, std, hidden)
    opt = torch.optim.Adam(model.parameters(), lr=lr)
    loss_fn = nn.BCELoss()

    xtr = torch.tensor(x_train, dtype=torch.float32)
    ytr = torch.tensor(train_ds.y, dtype=torch.float32).view(-1, 1)
    xte = torch.tensor(x_test, dtype=torch.float32)

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
            torch.nn.utils.clip_grad_norm_(model.parameters(), max_norm=1.0)
            opt.step()

            total_loss += float(loss.detach().cpu().item()) * len(bidx)

        if epoch == 1 or epoch == epochs or epoch % max(1, epochs // 5) == 0:
            model.eval()
            with torch.no_grad():
                p_test = model(xte).cpu().numpy().reshape(-1).tolist()
            print(
                f"epoch {epoch:>3}/{epochs}  "
                f"train_loss={total_loss/max(1,n):.6f}  "
                f"test_acc@0.5={acc_at_05(test_ds.y, p_test)*100:.2f}%"
            )

    model.eval()
    with torch.no_grad():
        p_test = model(xte).cpu().numpy().reshape(-1).tolist()

    metrics = {
        "n_train": len(train_ds.y),
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
    dummy = torch.zeros((1, in_dim), dtype=torch.float32)
    torch.onnx.export(
        model,
        dummy,
        onnx_path,
        input_names=["x"],
        output_names=["p_up"],
        opset_version=opset,
    )

    return {
        "type": "mlp_binary_classifier",
        "feature_order": FEATURE_ORDER,
        "input_dim": in_dim,
        "hidden": hidden,
        "trained_at": datetime.now(timezone.utc).isoformat(),
        "metrics": metrics,
        "note": "Model includes z-score normalization inside ONNX graph.",
    }


def main() -> None:
    print(
        "[deprecated] train_crypto_lob_mlp_onnx_from_db.py is legacy; "
        "use train_crypto_lob_tcn_onnx_from_db.py for current production models."
    )
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--db-url",
        default=os.environ.get("DATABASE_URL", "postgres://localhost/ploy"),
        help="Postgres URL (or env DATABASE_URL)",
    )
    ap.add_argument("--lookback-hours", type=int, default=168)
    ap.add_argument(
        "--source",
        choices=["sync_records", "order_executions"],
        default="sync_records",
        help="training data source",
    )
    ap.add_argument(
        "--symbol",
        default=None,
        help="optional symbol filter for sync_records source (e.g., BTCUSDT)",
    )
    ap.add_argument(
        "--horizon",
        choices=["5m", "15m"],
        default=None,
        help="optional horizon filter for sync_records source",
    )
    ap.add_argument("--account-id", default=None)
    ap.add_argument("--agent-id", default=None)
    ap.add_argument("--live-only", action="store_true")
    ap.add_argument("--limit", type=int, default=50000)
    ap.add_argument("--test-ratio", type=float, default=0.2)
    ap.add_argument("--hidden", default="32,16")
    ap.add_argument("--epochs", type=int, default=25)
    ap.add_argument("--batch-size", type=int, default=1024)
    ap.add_argument("--lr", type=float, default=1e-3)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--opset", type=int, default=17)
    ap.add_argument("--output", default="./models/crypto/lob_mlp_v1.onnx")
    ap.add_argument("--meta", default="./models/crypto/lob_mlp_v1.meta.json")
    ap.add_argument("--save-parquet", default=None)
    ap.add_argument("--export-scaler", default=None, help="export feature scaler (offsets/scales) as JSON for config-based normalization")

    args = ap.parse_args()

    hidden = [int(s) for s in args.hidden.split(",") if s.strip()]
    if not hidden:
        raise SystemExit("--hidden must not be empty")

    print(f"Fetching from DB (source={args.source})...")
    if args.source == "sync_records":
        ds = fetch_from_sync_records(
            db_url=args.db_url,
            lookback_hours=args.lookback_hours,
            symbol=args.symbol,
            horizon=args.horizon,
            limit=args.limit,
        )
    else:
        ds = fetch_from_order_executions(
            db_url=args.db_url,
            lookback_hours=args.lookback_hours,
            account_id=args.account_id,
            agent_id=args.agent_id,
            live_only=bool(args.live_only),
            limit=args.limit,
        )
    print(f"Rows: {len(ds.y)}")

    if args.save_parquet:
        maybe_save_parquet(ds, args.save_parquet)

    train_ds, test_ds = chronological_split(ds, args.test_ratio)
    print(f"Split: train={len(train_ds.y)} test={len(test_ds.y)}")

    meta = train_and_export_onnx(
        train_ds=train_ds,
        test_ds=test_ds,
        hidden=hidden,
        epochs=args.epochs,
        batch_size=max(1, args.batch_size),
        lr=args.lr,
        seed=args.seed,
        onnx_path=args.output,
        opset=args.opset,
    )

    os.makedirs(os.path.dirname(args.meta) or ".", exist_ok=True)
    with open(args.meta, "w") as f:
        json.dump(meta, f, indent=2)

    # Export scaler for config-based normalization (offset=mean, scale=1/std)
    if args.export_scaler:
        mean_vals, std_vals = mean_std(train_ds.x)
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

    print("\nEnable on EC2 (example):")
    print("  PLOY_CRYPTO_LOB_ML__ENABLED=true")
    print("  PLOY_CRYPTO_LOB_ML__MODEL_TYPE=onnx")
    print(f"  PLOY_CRYPTO_LOB_ML__MODEL_PATH={args.output}")
    print("  PLOY_CRYPTO_LOB_ML__MODEL_VERSION=lob_mlp_v1")
    print("  PLOY_CRYPTO_LOB_ML__WINDOW_FALLBACK_WEIGHT=0.10")


if __name__ == "__main__":
    main()
