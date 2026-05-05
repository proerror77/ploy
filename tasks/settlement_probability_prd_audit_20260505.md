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
- Latest reviewed head: PR #319 head after the conservative-matrix follow-up
- PR status: mergeable
- Ordinary PR CI: passed on run `25354110281`
- PR Auto Review: passed on run `25354110277`
- CodeRabbit: success
- Runner blocker issue: #320, closed after `ploy-ci-1` recovery
- Follow-up issue for remaining PRD promotion blockers: #321
- Final pushed head: PR #319 current head after final audit docs
- Final PR check status: all required PR checks green on run `25354770307`
  plus PR Auto Review `25354770288`; CodeRabbit success.
- Final runner status after cancelling the diagnostic snapshot:
  `ploy-ci-1` is `online` / `busy=false`.

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
| Data quality / coverage | `data-gap-audit.md` | Strict `pm5d-vol` run `25354264444` failed at `Audit required market data`; every required source had critical max gaps around `1700m` | Failing / blocks promotion |
| Deribit / vol inputs | `data_profile=pm5d-vol` or `include_deribit=true` snapshot | Strict `pm5d-vol` gate requires Deribit IV/Greeks but fails coverage before snapshot compile | Missing for full PRD vol lane |
| Portable replay evidence | Full research snapshot artifact | `upload_full_snapshot` option exists; strict run did not upload a full snapshot because data audit failed. Diagnostic non-strict run `25354314443` was cancelled during snapshot compile and produced no full snapshot artifact | Code present; strict data gate failed |
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

Fresh strict data-gate attempt:

- Run: `25354264444`
- Ref: `feat/settlement-probability-prd`
- Profile: `pm5d-vol`
- Gate: `critical`
- Window: `2026-04-21..2026-05-01`
- Symbols: `BTCUSDT,ETHUSDT,SOLUSDT`
- Stake: `15u`
- Result: failed at `Audit required market data`
- Required sources: `polymarket_quotes`, `polymarket_orderbooks`,
  `deribit_iv`, `deribit_atm_greeks`, `binance_price`,
  `binance_agg_trades`, `binance_lob`
- Gap evidence: PM quotes/orderbooks max gap `1705m`; Deribit and Binance
  sources max gap around `1700m`
- Root-cause follow-up on `tango-1-1` found this was a real continuous
  collection hole, not a missing "7 days" misunderstanding:
  - strict audit gap: PM quotes/orderbooks from about
    `2026-05-04 05:29/05:30 +0800` to
    `2026-05-05 09:54/09:55 +0800`;
  - Deribit/Binance sources from about `2026-05-04 05:30 +0800` to
    `2026-05-05 09:50 +0800`;
  - Binance price / aggTrade / LOB services had `NRestarts=6480`, and journals
    showed repeated `timed out during opening handshake` failures until around
    `2026-05-05 09:50 +0800`;
  - Deribit IV stayed active but logged repeated upstream SSL/read-timeout
    errors, and ATM Greeks cycles returned `ok=0/0` while no fresh IV
    instruments were available;
  - current one-hour audit after recovery is `ok` for PM quotes/orderbooks,
    Deribit IV/Greeks, and BTC/ETH/SOL Binance price/trades/LOB.

This is a valid negative PRD result, not a code failure. It means the current
database window cannot prove the settlement-probability system is ready for
replay or dry-run under the full PRD.

Diagnostic follow-up:

- Run: `25354314443`
- Difference: `data_gate=never`, `upload_full_snapshot=true`
- Status: completed with conclusion `cancelled`; it was cancelled while still
  in `Compile snapshot` after the strict decision-grade data gate had already
  failed
- Interpretation: useful only to inspect materialized rows and artifact shape;
  it cannot override strict data insufficiency.

Conservative execution surface:

- `factor_walk_forward_v2` now prints a second Full-Depth Execution Matrix with
  `visible_depth_haircut=0.5` and `max_levels=3`.
- The settlement probability report now also prints conservative settlement PnL
  and conservative profit-factor columns for baseline, calibration, and edge
  buckets using the same `50% visible depth / max 3 CLOB levels` assumption.
- Verification:
  `rustfmt --edition 2021 --check
  crates/ploy-research/examples/factor_walk_forward_v2.rs`
- Verification:
  `CARGO_TARGET_DIR=/tmp/ploy-settlement-conservative-matrix
  /opt/homebrew/bin/timeout 240 rtk cargo check -p ploy-research --example
  factor_walk_forward_v2 --features db --no-default-features`
- Remaining conservative gap: this surfaces conservative capacity/fillability
  and conservative settlement edge buckets, but replay parity still needs a
  decision-grade snapshot.

## Short-Window Workflow Smoke

The user explicitly allowed missing history to keep filling while using current
or prior data for a short test. This smoke is not promotion evidence; it only
checks that the PRD workflow can run over a portable full snapshot after the
collectors recovered.

Snapshot smoke:

- Run: `25356726430`
- Ref: `feat/settlement-probability-prd`
- Profile: `pm5d-vol`
- Gate: `critical`
- Audit lookback: `1h`
- Window input: `2026-05-05..2026-05-05`, parsed as
  `2026-05-05 00:00:00 UTC -> 2026-05-06 00:00:00 UTC`
- Symbols: `BTCUSDT,ETHUSDT,SOLUSDT`
- Stake: `15u`
- Artifact: `research-snapshot-25356726430`
- Result: success
- Rows: observations `1225`, Deribit snapshots `485`, PM book snapshots `2876`
- Data audit: `ok` for PM quotes/orderbooks, Deribit IV/Greeks, and
  BTC/ETH/SOL Binance price/trades/LOB in the one-hour audit window
- Official settlement required: `true`
- Deribit included: `true`

Downstream walk-forward smoke:

- First run: `25356886955`
- Result: failed at snapshot read before reporting
- Root cause: portable `observations.json` serialized non-finite
  `flip_age_secs` as JSON `null`, while the loader expected a concrete `f64`
- Fix: commit `5677f294` makes `flip_age_secs` deserialize nullable f64 values
  as `NaN` and extends the snapshot-null regression test
- Verification: local snapshot replay against `research-snapshot-25356726430`
  completed and printed the settlement probability report
- Re-run: `25357077864`
- Result: success
- Artifact: `factor-walk-forward-v2-25357077864`
- Rows: source observations `1225`, side rows `2450`, executable settlement
  PnL rows `1004`, Deribit-enriched rows `1618`
- Execution: full-depth entry fill `40.98%`, exit fill `39.35%`
- Baseline highlights:
  - `q_existing_fair_prob`: ECE `0.101327`, avg full-depth settlement PnL
    `+1.2079`, avg conservative settlement PnL `+0.6244`, top-edge full-depth
    settlement PnL `+9.3541`, top-edge conservative PnL `+8.0545`
  - `q_market_midpoint`: ECE `0.102095`, avg full-depth settlement PnL
    `+1.2079`, avg conservative settlement PnL `+0.6244`, top-edge full-depth
    settlement PnL `+5.3665`, top-edge conservative PnL `+3.3891`
  - `q_distance_lob_vol_phi`: ECE `0.059314`, but average full-depth
    settlement PnL `-0.9257`, so lower calibration error did not translate
    into positive executable EV in this smoke
- Holdout caveat: SOL holdout is negative for `q_existing_fair_prob`
  (`-0.3116`) and `q_market_midpoint` (`-0.2303`)

Interpretation:

- The short-window workflow chain now works: strict current data audit,
  portable full snapshot artifact, full-depth execution matrix, Deribit-included
  settlement probability report, conservative edge columns, and issue-comment
  evidence.
- This does not unblock dry-run/live. The sample is tiny and not a retained
  clean 168h window, and the symbol holdout is not stable.

## Recovery Checklist

1. Collect or compile a fresh snapshot with `data_gate=critical` and
   `upload_full_snapshot=true`, so the next evidence is portable and not only
   runner-local.
2. Run a `pm5d-vol` / Deribit-included snapshot for BTC/ETH/SOL to satisfy the
   PRD volatility lane.
3. Run the conservative settlement edge buckets against the next strict
   snapshot and add any remaining quote-age, latency, and adverse-selection
   buffers needed for live handoff.
4. Narrow the candidate to BTC/ETH/SOL settlement probability top-edge buckets.
5. Exclude DOGE from any all-six settlement promotion until its holdout turns
   positive under the same full-depth labels.
6. Run recorded replay parity with the same scorer before any dry-run handoff.
