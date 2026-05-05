# Settlement Probability PRD Completion Audit

Date: 2026-05-05

## Objective

Complete the Polymarket Crypto 5m / 15m settlement-probability strategy PRD,
run the required tests, and decide whether the data and strategy evidence are
sufficient for replay / dry-run promotion.

The strategy gate is:

```text
q_side - full_depth_entry_sweep_avg_price
  > fee + slippage + latency + model_error + safety_margin
```

Top-book evidence is not decision-grade. The promotion path requires
snapshot-backed full-depth execution labels and probability-model validation.

## Current Branch And PR Evidence

- Branch: `feat/settlement-probability-prd`
- PR: #319
- Latest reviewed head: `2eceb4657efcf4328f5fe9625234009fbcce7d49`
- PR status: mergeable
- Ordinary PR CI: passed on run `25353206867`
- Workflow lint: passed on run `25353206867`
- Runner blocker issue: #320

## Prompt-To-Artifact Checklist

| PRD requirement | Required artifact / command | Current evidence | Status |
| --- | --- | --- | --- |
| Main lane is Settlement Probability Trading | `tasks/todo.md`, PR #319 code path | Plan records settlement probability as the main lane and repricing as secondary diagnostic | Complete |
| Use full Polymarket CLOB execution, not top book | `FullDepthExecutionMatrix`, full-depth labels in `crates/ploy-research/src/factors_v2.rs` | Existing PR work uses full-depth settlement labels for report rows | Code present; remote validation blocked |
| Candidate rows are side + stake + executable labels | `factor_walk_forward_v2` report inputs | Report is wired to settlement probability rows and full-depth labels | Code present; remote validation blocked |
| Probability baselines | Settlement probability report | PR includes `q_naive_50_50`, `q_market_midpoint`, distance Phi, LOB/vol baselines, existing fair/model probabilities | Code present; remote validation blocked |
| Calibration report | `=== Settlement Probability Report ===` artifacts | Requires snapshot-backed report output from runs `25352664353` and `25352664495` | Missing |
| Edge bucket report | Factor walk-forward artifact `report.txt` | Requires snapshot-backed report output from runs `25352664353` and `25352664495` | Missing |
| Baseline comparison | Settlement probability report baseline section | Requires snapshot-backed report output from runs `25352664353` and `25352664495` | Missing |
| Anti-overfit diagnostics | label shift, prediction shift, symbol holdout, baseline ablation sections | Code added; needs snapshot-backed output | Missing |
| Walk-forward OOS | `factor-walk-forward-v2.yml` | Runs are queued/pending because `ploy-ci-1` is offline | Blocked |
| Official settlement fidelity | Snapshot manifest `require_official_settlement=true` | Provenance for snapshots `25254380121` and `25255158983` confirms official settlement required | Partial evidence |
| Data quality / coverage | `data-gap-audit.md` | Existing snapshot provenance reports `data_audit_status=critical` and max gaps around 280-410m | Failing / requires decision |
| Deribit / vol inputs | `data_profile=pm5d-vol` or `include_deribit=true` snapshot | Existing snapshots use `pm5d-execution`, `include_deribit=false` | Missing for full PRD vol lane |
| Portable replay evidence | Full research snapshot artifact | New `upload_full_snapshot` option added to `research-snapshot.yml`; not yet exercised because runner is offline | Code present; runtime blocked |
| Replay parity | `ReplayParityReport` / recorded replay artifacts | Not part of current completed evidence | Missing |
| Dry-run readiness | Dry-run handoff packet and kill switch evidence | PRD gates have not passed | Not ready |

## Blocking Evidence

The required snapshot-backed validation runs cannot start:

- BTC/ETH/SOL: run `25352664353`, queued
- XRP/DOGE/BNB: run `25352664495`, pending

GitHub runner state:

```text
runner: ploy-ci-1
status: offline
busy: false
labels: self-hosted, Linux, X64, ploy-ci-1
```

Aliyun ECS state:

```text
instance_id: i-6we7z44sfbfbnosbeymz
region: ap-northeast-1
status: Stopped
charge_type: PostPaid
operation_locks: financial, financial-recycling
start_error: InstanceExpired
```

## Snapshot Artifact Audit

Existing snapshot run artifacts are not enough to complete the PRD gate without
the runner-local registry:

- `25254380121`: `research-snapshot-provenance-25254380121`, about 13KB
- `25255158983`: `research-snapshot-provenance-25255158983`, about 13KB
- Both say `full_snapshot_embedded=false`
- Both say `registry=runner-local`
- Downloaded contents do not include `observations.json`,
  `pm_book_snapshots.json`, `deribit_snapshots.json`, or parquet observations

The provenance is useful for row counts and audit status, but it is not a
replayable dataset.

## Data Sufficiency Finding

Data sufficiency is not established.

Available provenance shows substantial row counts:

- BTC/ETH/SOL: observations `114008`, PM books `171794`
- XRP/DOGE/BNB: observations `108296`, PM books `141692`

But the same provenance reports:

- `data_audit_status=critical`
- max gaps around 280-410 minutes across required sources
- Deribit excluded (`include_deribit=false`)
- no portable full snapshot artifact

Therefore current evidence is insufficient for PRD promotion. The next
decision-grade step is to restore `ploy-ci-1`, rerun or resume the
snapshot-backed factor walk-forward reports, and inspect the actual calibration,
edge-bucket, baseline, anti-overfit, and holdout sections.

## Recovery Checklist

1. Clear Aliyun `financial` / `financial-recycling` locks for
   `i-6we7z44sfbfbnosbeymz`.
2. Start `ploy-ci-1`.
3. Confirm the GitHub runner is online.
4. Let or retrigger:
   - `25352664353` for BTC/ETH/SOL, snapshot `25254380121`
   - `25352664495` for XRP/DOGE/BNB, snapshot `25255158983`
5. For a fresh snapshot, set `upload_full_snapshot=true` when decision-grade
   portable evidence is needed.
6. Parse the report sections:
   - Settlement Probability Report
   - Calibration Buckets
   - Edge Buckets
   - Baseline Comparison
   - Anti-Overfit Diagnostics
   - Symbol Holdout Diagnostics
   - Baseline Ablations
7. Decide: reject, revise, collect more data, or promote to replay.
