# Phase 0 Data Plane Baseline Runbook

This runbook covers issue #21 baseline tasks that are executed on `tango-1-1`:

- 24h baseline capture for per `(source, symbol)` message rate
- rollback gate check: drop rate `>5%` sustained `>60s`

## Scripts

- `scripts/collect_data_plane_baseline.py`
- `scripts/validate_data_plane_drop_rate.py`

## 1) Start 24h Baseline Capture

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

## 2) Output Artifacts

For each run id `<run_id>`, the collector writes:

- `data/baseline/<run_id>.samples.jsonl` (raw counter snapshots and per-interval rates)
- `data/baseline/<run_id>.summary.json` (aggregated rates, p50/p95, totals)
- `data/baseline/<run_id>.baseline.json` (baseline reference + rollback rule metadata)
- `data/baseline/<run_id>.symbol_rates.csv`
- `data/baseline/<run_id>.source_rates.csv`

## 3) Validate Rollback Gate

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

## 4) Rule Definition

Default rollback rule encoded in baseline file:

- `drop_pct = 0.05`
- `sustain_secs = 60`

Interpretation:

- For each series (`source|symbol` and source aggregate), trigger if:
  - observed rate `< baseline_rate * (1 - drop_pct)`
  - condition remains true for `>= sustain_secs`
