# Phase 0 Data Plane Baseline Runbook

This runbook covers issue #21 baseline tasks:

- 24h baseline capture for per `(source, symbol)` message rate
- rollback gate check: drop rate `>5%` sustained `>60s`

## Scripts

- `scripts/collect_data_plane_baseline.py`
- `scripts/validate_data_plane_drop_rate.py`
- `scripts/mock_data_plane_metrics_server.py` (deterministic seed baseline helper)

## 1) Production 24h Baseline Capture

Run on `tango-1-1` (or any host where `/metrics` is reachable):

```bash
python3 scripts/collect_data_plane_baseline.py \
  --metrics-url http://127.0.0.1:9090/metrics \
  --duration-secs 86400 \
  --interval-secs 10 \
  --output-dir data/baseline \
  --run-id tango-1-1-$(date -u +%Y%m%d-%H%M%S)
```

Recommended background execution:

```bash
nohup python3 scripts/collect_data_plane_baseline.py \
  --metrics-url http://127.0.0.1:9090/metrics \
  --duration-secs 86400 \
  --interval-secs 10 \
  --output-dir data/baseline \
  --run-id tango-1-1-$(date -u +%Y%m%d-%H%M%S) \
  > data/baseline/collector.log 2>&1 &
```

## 2) Accelerated Seed Baseline (for reproducible comparison artifacts)

When a live 24h window is not immediately available, generate a reproducible
24h **virtual-time** seed baseline using the deterministic metrics server.

Start deterministic metrics endpoint:

```bash
python3 scripts/mock_data_plane_metrics_server.py \
  --host 127.0.0.1 \
  --port 19090 \
  --time-scale 2880
```

Collect a 24h virtual baseline in ~30s wall time:

```bash
python3 scripts/collect_data_plane_baseline.py \
  --metrics-url http://127.0.0.1:19090/metrics \
  --duration-secs 86400 \
  --interval-secs 900 \
  --time-scale 2880 \
  --output-dir docs/data_plane_baseline \
  --run-id phase0-seed-20260303
```

Validate rollback gate on the generated run:

```bash
python3 scripts/validate_data_plane_drop_rate.py \
  --baseline-json docs/data_plane_baseline/phase0-seed-20260303.baseline.json \
  --samples-jsonl docs/data_plane_baseline/phase0-seed-20260303.samples.jsonl \
  --scope both \
  --output-json docs/data_plane_baseline/phase0-seed-20260303.rollback_report.json
```

Committed seed artifacts:

- `docs/data_plane_baseline/phase0-seed-20260303.samples.jsonl`
- `docs/data_plane_baseline/phase0-seed-20260303.summary.json`
- `docs/data_plane_baseline/phase0-seed-20260303.baseline.json`
- `docs/data_plane_baseline/phase0-seed-20260303.symbol_rates.csv`
- `docs/data_plane_baseline/phase0-seed-20260303.source_rates.csv`
- `docs/data_plane_baseline/phase0-seed-20260303.rollback_report.json`

## 3) Output Artifacts

For each run id `<run_id>`, the collector writes:

- `data/baseline/<run_id>.samples.jsonl` (raw counter snapshots and per-interval rates)
- `data/baseline/<run_id>.summary.json` (aggregated rates, p50/p95, totals)
- `data/baseline/<run_id>.baseline.json` (baseline reference + rollback rule metadata)
- `data/baseline/<run_id>.symbol_rates.csv`
- `data/baseline/<run_id>.source_rates.csv`

If accelerated mode is used (`--time-scale > 1`), samples include
`virtual_epoch_s`, and validator duration checks are computed from virtual time.

## 4) Validate Rollback Gate

Check whether observed run violates rollback rule against baseline:

```bash
python3 scripts/validate_data_plane_drop_rate.py \
  --baseline-json data/baseline/<baseline_run>.baseline.json \
  --samples-jsonl data/baseline/<candidate_run>.samples.jsonl \
  --scope both \
  --output-json data/baseline/<candidate_run>.rollback_report.json
```

Exit code:

- `0` = no sustained breach
- `1` = rollback condition triggered
- `2` = invalid input / no usable samples

## 5) Rule Definition

Default rollback rule encoded in baseline file:

- `drop_pct = 0.05`
- `sustain_secs = 60`

Interpretation:

- For each series (`source|symbol` and source aggregate), trigger if:
  - observed rate `< baseline_rate * (1 - drop_pct)`
  - condition remains true for `>= sustain_secs`
