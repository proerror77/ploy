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
- Latest reviewed head: `fcadcf730466ec0b9ca339ac211026e25aa17f8e`
- PR status: mergeable
- Ordinary PR CI: passed on run `25353323418`
- Workflow lint: passed on run `25353323418`
- Runner blocker issue: #320

## Prompt-To-Artifact Checklist

| PRD requirement | Required artifact / command | Current evidence | Status |
| --- | --- | --- | --- |
| Main lane is Settlement Probability Trading | `tasks/todo.md`, PR #319 code path | Plan records settlement probability as the main lane and repricing as secondary diagnostic | Complete |
| Use full Polymarket CLOB execution, not top book | `FullDepthExecutionMatrix`, full-depth labels in `crates/ploy-research/src/factors_v2.rs` | Snapshot-backed reports `25353780686` / `25353780673` include full-depth entry-fillable settlement rows | Complete |
| Candidate rows are side + stake + executable labels | `factor_walk_forward_v2` report inputs | Reports evaluate side-aligned candidate rows at `15u` stake with full-depth settlement labels | Complete |
| Probability baselines | Settlement probability report | Reports include `q_naive_50_50`, `q_market_midpoint`, distance Phi, LOB/vol baselines, existing fair/model probabilities | Complete |
| Calibration report | `=== Settlement Probability Report ===` artifacts | Present in runs `25353780686` and `25353780673` | Complete |
| Edge bucket report | Factor walk-forward artifact `report.txt` | Present in runs `25353780686` and `25353780673` | Complete |
| Baseline comparison | Settlement probability report baseline section | Present in runs `25353780686` and `25353780673` | Complete |
| Anti-overfit diagnostics | label shift, prediction shift, symbol holdout, baseline ablation sections | Present in runs `25353780686` and `25353780673` | Complete |
| Walk-forward OOS | `factor-walk-forward-v2.yml` | Snapshot-backed runs completed successfully after correcting inclusive end-date input | Complete |
| Official settlement fidelity | Snapshot manifest `require_official_settlement=true` | Provenance for snapshots `25254380121` and `25255158983` confirms official settlement required | Partial evidence |
| Data quality / coverage | `data-gap-audit.md` | Both successful runs still report `data_audit_status=critical` and max gaps around 280-410m | Failing / blocks promotion |
| Deribit / vol inputs | `data_profile=pm5d-vol` or `include_deribit=true` snapshot | Existing snapshots use `pm5d-execution`, `include_deribit=false` | Missing for full PRD vol lane |
| Portable replay evidence | Full research snapshot artifact | New `upload_full_snapshot` option added to `research-snapshot.yml`; not yet exercised because runner is offline | Code present; runtime blocked |
| Replay parity | `ReplayParityReport` / recorded replay artifacts | Not part of current completed evidence | Missing |
| Dry-run readiness | Dry-run handoff packet and kill switch evidence | PRD gates have not passed | Not ready |

## Remote Validation Evidence

The original queued runs failed before data execution because they referenced an
unreachable commit and then, after retriggering, used an end-date input that
expanded beyond the immutable snapshot window. The decision-grade reruns used
`end_date=2026-05-01`, which the CLI expands to the snapshot window
`2026-04-21 00:00:00 UTC -> 2026-05-02 00:00:00 UTC`.

Successful snapshot-backed runs:

- BTC/ETH/SOL: run `25353780686`, artifact
  `factor-walk-forward-v2-25353780686`
- XRP/DOGE/BNB: run `25353780673`, artifact
  `factor-walk-forward-v2-25353780673`

Execution evidence:

- BTC/ETH/SOL: full-depth entry fill rate `49.23%`, exit fill rate `40.70%`,
  full-depth settlement PnL rows `112256`.
- XRP/DOGE/BNB: full-depth entry fill rate `45.22%`, exit fill rate `36.27%`,
  full-depth settlement PnL rows `97939`.

Settlement probability evidence:

- BTC/ETH/SOL `q_market_midpoint`: ECE `0.004096`, top-edge full-depth
  settlement PnL `+4.9364`, profit factor `0.9072`, symbol holdouts all pass
  (`BTC +7.5521`, `ETH +4.8017`, `SOL +1.6634`).
- BTC/ETH/SOL `q_existing_fair_prob`: ECE `0.003891`, top-edge full-depth
  settlement PnL `+4.7994`, profit factor `0.9071`, symbol holdouts all pass
  (`BTC +7.5528`, `ETH +4.3742`, `SOL +1.6950`).
- XRP/DOGE/BNB `q_existing_fair_prob`: ECE `0.013897`, top-edge full-depth
  settlement PnL `+0.5916`, but DOGE holdout fails (`DOGE -0.2531`).
- XRP/DOGE/BNB `q_market_midpoint`: ECE `0.014006`, top-edge full-depth
  settlement PnL `+0.5490`, but DOGE holdout fails (`DOGE -0.1951`).

Interpretation:

- BTC/ETH/SOL has a real settlement-probability candidate worth the next
  replay-quality research pass.
- XRP/DOGE/BNB is not an all-symbol candidate; at most XRP/BNB deserve a
  narrowed follow-up, while DOGE should be excluded or separately modeled.
- Distance-only Phi baselines are anti-signaled in these reports and must not
  be promoted as the settlement model.

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

Therefore current evidence is enough to identify the next candidate lane, but
not enough for dry-run promotion. BTC/ETH/SOL `q_market_midpoint` /
`q_existing_fair_prob` top-edge settlement probability is the strongest
candidate. Promotion remains blocked by critical data gaps, missing Deribit vol
inputs, missing conservative haircut report, and missing replay parity.

## Recovery Checklist

1. Collect or compile a fresh snapshot with `data_gate=critical` and
   `upload_full_snapshot=true`, so the next evidence is portable and not only
   runner-local.
2. Run a `pm5d-vol` / Deribit-included snapshot for BTC/ETH/SOL to satisfy the
   PRD volatility lane.
3. Add or run conservative-depth labels: visible-depth haircut, max sweep
   levels, quote-age, latency, and adverse-selection buffers.
4. Narrow the candidate to BTC/ETH/SOL settlement probability top-edge buckets.
5. Exclude DOGE from any all-six settlement promotion until its holdout turns
   positive under the same full-depth labels.
6. Run recorded replay parity with the same scorer before any dry-run handoff.
