#!/usr/bin/env python3
"""
Train a small MLP (neural network) to predict Polymarket BTC 5m UP probability
from Binance LOB-derived features that are already persisted in Postgres and
exported via:

  ploy strategy export-crypto-lob-dataset --format csv --output ./data/crypto_lob_dataset.csv

This script outputs a JSON model that can be loaded by the Rust runtime
(`DenseNetwork`) for 24/7 inference on EC2:

  - input_mean / input_std (z-score normalization)
  - layers: weights[out][in], bias[out], activation per layer

Recommended split: chronological (no look-ahead leakage).

NOTE: This is a legacy path (JSON weights).
Preferred production path is ONNX:
  - Train from DB (no dataset file): scripts/train_crypto_lob_mlp_onnx_from_db.py
  - Runtime inference (Rust): model_type=onnx (compiled with --features onnx)
"""

from __future__ import annotations

import argparse
import csv
import json
import math
import os
import random
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import List, Tuple


FEATURES = [
    "obi5",
    "obi10",
    "spread_bps",
    "bid_volume_5",
    "ask_volume_5",
    "momentum_1s",
    "momentum_5s",
]


def _parse_float(v: str) -> float | None:
    if v is None:
        return None
    s = v.strip()
    if not s:
        return None
    try:
        x = float(s)
    except Exception:
        return None
    if not math.isfinite(x):
        return None
    return x


def _parse_int(v: str) -> int | None:
    if v is None:
        return None
    s = v.strip()
    if not s:
        return None
    try:
        return int(s)
    except Exception:
        return None


@dataclass
class Dataset:
    x: List[List[float]]
    y: List[int]
    ts: List[str]


def load_csv(path: str) -> Dataset:
    x: List[List[float]] = []
    y: List[int] = []
    ts: List[str] = []

    with open(path, "r", newline="") as f:
        r = csv.DictReader(f)
        missing_cols = [c for c in (["executed_at", "y_up"] + FEATURES) if c not in r.fieldnames]
        if missing_cols:
            raise SystemExit(f"missing columns in CSV: {missing_cols}")

        for row in r:
            # label
            yi = _parse_int(row.get("y_up", ""))
            if yi not in (0, 1):
                continue

            feats: List[float] = []
            ok = True
            for k in FEATURES:
                v = _parse_float(row.get(k, ""))
                if v is None:
                    ok = False
                    break
                feats.append(v)
            if not ok:
                continue

            x.append(feats)
            y.append(int(yi))
            ts.append(row.get("executed_at", ""))

    if not x:
        raise SystemExit("no usable rows loaded from CSV")

    return Dataset(x=x, y=y, ts=ts)


def chronological_split(ds: Dataset, test_ratio: float) -> Tuple[Dataset, Dataset]:
    n = len(ds.y)
    if n < 50:
        raise SystemExit(f"dataset too small: n={n}")
    if not (0.05 <= test_ratio <= 0.5):
        raise SystemExit("--test-ratio must be in [0.05, 0.5]")

    idx = list(range(n))
    # Sort by timestamp string (RFC3339) which is lexicographically sortable.
    idx.sort(key=lambda i: ds.ts[i])

    cut = int((1.0 - test_ratio) * n)
    cut = max(1, min(cut, n - 1))

    def _take(idxs: List[int]) -> Dataset:
        return Dataset(
            x=[ds.x[i] for i in idxs],
            y=[ds.y[i] for i in idxs],
            ts=[ds.ts[i] for i in idxs],
        )

    return _take(idx[:cut]), _take(idx[cut:])


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

    # Avoid zero std (constant feature)
    std = [s if s > 1e-12 else 1.0 for s in std]

    return mean, std


def zscore(x: List[List[float]], mean: List[float], std: List[float]) -> List[List[float]]:
    out: List[List[float]] = []
    for row in x:
        out.append([(row[j] - mean[j]) / std[j] for j in range(len(row))])
    return out


def sigmoid(x: float) -> float:
    if x >= 0.0:
        z = math.exp(-x)
        return 1.0 / (1.0 + z)
    z = math.exp(x)
    return z / (1.0 + z)


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


def accuracy_at_05(y_true: List[int], p: List[float]) -> float:
    c = 0
    for yt, pi in zip(y_true, p):
        pred = 1 if pi >= 0.5 else 0
        if pred == yt:
            c += 1
    return c / max(1, len(y_true))


def train_with_torch(
    x_train: List[List[float]],
    y_train: List[int],
    x_test: List[List[float]],
    y_test: List[int],
    hidden: List[int],
    epochs: int,
    batch_size: int,
    lr: float,
    seed: int,
) -> Tuple[dict, dict]:
    try:
        import torch
        import torch.nn as nn
    except Exception as e:
        raise SystemExit(
            "PyTorch is required for training.\n"
            "Install: python -m pip install torch\n"
            f"Import error: {e}"
        )

    torch.manual_seed(seed)
    random.seed(seed)

    in_dim = len(x_train[0])
    layers: List[nn.Module] = []
    prev = in_dim
    for h in hidden:
        layers.append(nn.Linear(prev, h))
        layers.append(nn.ReLU())
        prev = h
    layers.append(nn.Linear(prev, 1))  # logits
    model = nn.Sequential(*layers)

    opt = torch.optim.Adam(model.parameters(), lr=lr)
    loss_fn = nn.BCEWithLogitsLoss()

    # tensors
    xtr = torch.tensor(x_train, dtype=torch.float32)
    ytr = torch.tensor(y_train, dtype=torch.float32).view(-1, 1)
    xte = torch.tensor(x_test, dtype=torch.float32)
    yte = torch.tensor(y_test, dtype=torch.float32).view(-1, 1)

    n = xtr.shape[0]
    idxs = list(range(n))

    for epoch in range(1, epochs + 1):
        model.train()
        random.shuffle(idxs)

        total_loss = 0.0
        for start in range(0, n, batch_size):
            batch_idx = idxs[start : start + batch_size]
            xb = xtr[batch_idx]
            yb = ytr[batch_idx]

            opt.zero_grad()
            logits = model(xb)
            loss = loss_fn(logits, yb)
            loss.backward()
            opt.step()

            total_loss += float(loss.detach().cpu().item()) * len(batch_idx)

        if epoch == 1 or epoch == epochs or epoch % max(1, epochs // 5) == 0:
            model.eval()
            with torch.no_grad():
                p = torch.sigmoid(model(xte)).cpu().numpy().reshape(-1).tolist()
            acc = accuracy_at_05(y_test, p)
            print(
                f"epoch {epoch:>3}/{epochs}  "
                f"train_loss={total_loss/max(1,n):.6f}  "
                f"test_acc@0.5={acc*100:.2f}%"
            )

    # Final eval
    model.eval()
    with torch.no_grad():
        p_test = torch.sigmoid(model(xte)).cpu().numpy().reshape(-1).tolist()

    metrics = {
        "n_train": len(y_train),
        "n_test": len(y_test),
        "acc_at_0.5": accuracy_at_05(y_test, p_test),
        "brier": brier(y_test, p_test),
        "log_loss": log_loss(y_test, p_test),
    }

    # Export weights/bias in DenseNetwork schema.
    exported_layers = []
    i = 0
    # walk sequential: Linear, ReLU, Linear, ...
    while i < len(model):
        m = model[i]
        if isinstance(m, nn.Linear):
            w = m.weight.detach().cpu().numpy().tolist()  # [out][in]
            b = m.bias.detach().cpu().numpy().tolist()  # [out]
            act = "linear"
            # If next module is ReLU, encode activation on this layer.
            if i + 1 < len(model) and isinstance(model[i + 1], nn.ReLU):
                act = "relu"
                i += 1
            exported_layers.append({"weights": w, "bias": b, "activation": act})
        i += 1

    # Ensure last layer outputs probability.
    if not exported_layers:
        raise SystemExit("export failed: no layers found")
    exported_layers[-1]["activation"] = "sigmoid"

    export = {
        "input_dim": in_dim,
        "input_mean": None,  # filled by caller
        "input_std": None,  # filled by caller
        "layers": exported_layers,
        "metadata": {
            "type": "mlp_binary_classifier",
            "feature_order": FEATURES,
            "hidden": hidden,
            "trained_at": datetime.now(timezone.utc).isoformat(),
            "metrics": metrics,
        },
    }

    return export, metrics


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input", default="./data/crypto_lob_dataset.csv", help="Input CSV dataset")
    ap.add_argument("--output", default="./models/crypto/lob_mlp_v1.json", help="Output model JSON")
    ap.add_argument("--test-ratio", type=float, default=0.2, help="Chronological test ratio")
    ap.add_argument("--hidden", default="32,16", help="Hidden sizes e.g. 32,16")
    ap.add_argument("--epochs", type=int, default=25)
    ap.add_argument("--batch-size", type=int, default=1024)
    ap.add_argument("--lr", type=float, default=1e-3)
    ap.add_argument("--seed", type=int, default=42)

    args = ap.parse_args()

    ds = load_csv(args.input)
    train_ds, test_ds = chronological_split(ds, args.test_ratio)

    mean, std = mean_std(train_ds.x)
    x_train = zscore(train_ds.x, mean, std)
    x_test = zscore(test_ds.x, mean, std)

    hidden = [int(s) for s in args.hidden.split(",") if s.strip()]
    if not hidden:
        raise SystemExit("--hidden must not be empty")

    export, metrics = train_with_torch(
        x_train=x_train,
        y_train=train_ds.y,
        x_test=x_test,
        y_test=test_ds.y,
        hidden=hidden,
        epochs=args.epochs,
        batch_size=max(1, args.batch_size),
        lr=args.lr,
        seed=args.seed,
    )

    export["input_mean"] = mean
    export["input_std"] = std

    out_path = args.output
    os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
    with open(out_path, "w") as f:
        json.dump(export, f, indent=2, sort_keys=False)

    print("\nExported model:")
    print(f"  path: {out_path}")
    print(f"  metrics: acc@0.5={metrics['acc_at_0.5']*100:.2f}%  brier={metrics['brier']:.6f}  ll={metrics['log_loss']:.6f}")
    print("\nEnable on EC2 (example):")
    print("  PLOY_CRYPTO_LOB_ML__ENABLED=true")
    print("  PLOY_CRYPTO_LOB_ML__MODEL_TYPE=mlp")
    print(f"  PLOY_CRYPTO_LOB_ML__MODEL_PATH={out_path}")
    print("  PLOY_CRYPTO_LOB_ML__MODEL_VERSION=lob_mlp_v1")


if __name__ == "__main__":
    main()
