# Settlement Probability PRD Completion Audit

Date: 2026-05-05

Objective: complete the Polymarket Crypto 5m / 15m settlement-probability PRD
as a dry-run-first strategy system. The system is not live-approved.

## Current Verdict

Status: **not complete**

The runtime/dry-run evidence path is now working, including settlement-specific
recorded replay parity. The retained 168h research promotion gate is still
blocked by strict market-data continuity, tracked in issue #339.

Do not mark the PRD complete until the retained-window gate passes with the
same or newer settlement-specific replay parity artifact.

## Deliverable Checklist

| PRD Requirement | Evidence | Status |
| --- | --- | --- |
| Settlement probability is the main lane, not repricing momentum | `tasks/todo.md` PRD plan; issue #332 BTC/ETH handoff candidate | Complete |
| Dedicated BTC/ETH settlement dry-run config exists | `config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml`; PR #334 | Complete |
| Deployment manifest exists for paper/dry-run only | `config/deployments/pm5d.threelayer.settlement-probability-btc-eth.dryrun.json`; PR #335 | Complete |
| Tango deploy workflow installs the settlement config | `.github/workflows/deploy-tango-1-1.yml`; deploy run `25370757209` and later `25373476802` | Complete |
| Old repricing dry-run is paused while settlement dry-run runs | Remote `ployctl deployments inspect`: settlement desired/observed `Running`, repricing `Paused` | Complete |
| Dry-run report exposes the strategy label | `scripts/report_dryrun_summary.py`; dry-run label `TL Settlement Probability BTC/ETH` | Complete |
| Runtime order evidence includes q/edge audit context | PR #336; deployed SHA `d87261d0cc153babc68ae799a424b294d6a0e154`; post-deploy orders show `signal_p_hat`, `signal_edge`, `signal_entry_price` | Complete |
| Runtime explicitly does not claim full-depth parity | Runtime order context records `runtime_price_basis=top_book_quote` and `full_depth_runtime_parity=false` | Complete |
| Settlement dry-run records its own MarketUpdate stream | PR #337; deployed SHA `b8c927669cf5bb673dde4a9f05d9ccc15b4f8fda`; `/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson` actively updates | Complete |
| Recorded replay parity uses the settlement-specific recording | Recorded replay parity run `25374110073`, artifact `recorded-replay-parity-25374110073` | Complete |
| Runtime strict replay parity passes for a fresh settlement window | Run `25374110073`: orders `2/2/2`, fills `2/2/2`, `strict_parity_ready=true`, `blocking_risk_flags=[]`, decision `continue` | Complete |
| Short-window PRD smoke consumes the new replay parity artifact | Gate run `25374918500`, snapshot `25374930653`, walk-forward `25375300501`; `recorded_replay_parity=true` | Complete as smoke only |
| Short-window smoke shows current data health | Snapshot `25374930653` passed `data_quality=true` with `audit_lookback_hours=1` | Complete as smoke only |
| Retained 168h strict PRD gate passes | Gate run `25374406705` failed before snapshot compilation; audit max gaps `1695-1705m` | Blocked by #339 |
| Non-empty retained walk-forward OOS passes | Retained gate did not reach walk-forward because strict data audit failed | Blocked by #339 |
| PRD promotion gate is decision-grade | Latest retained gate failed at strict data audit; latest short smoke has `walk_forward_oos=false` due no non-empty OOS windows | Blocked |
| Live approval / live trading | No live approval requested or granted | Not in scope |

## Evidence Details

### Runtime And Deployment

- PR #336: `https://github.com/proerror77/ploy/pull/336`
  - Merge SHA: `d87261d0cc153babc68ae799a424b294d6a0e154`
  - Adds probability-edge audit context to dry-run runtime orders.
- PR #337: `https://github.com/proerror77/ploy/pull/337`
  - Merge SHA: `b8c927669cf5bb673dde4a9f05d9ccc15b4f8fda`
  - Adds settlement-specific MarketUpdate recording.
- PR #338: `https://github.com/proerror77/ploy/pull/338`
  - Merge SHA: `cd52ea18dc4df7e19a4d5f88ad6987fd2efe4fb2`
  - Records parity evidence in `tasks/todo.md`.

Remote verification after deployment:

```text
pm5d.threelayer.settlement-probability-btc-eth.dryrun desired=Running observed=Running
pm5d.threelayer.repricing-momentum.dryrun desired=Paused observed=Paused
ployd.service ActiveState=active SubState=running Restart=always OOMPolicy=kill
MemoryHigh=1342177280 MemoryMax=1610612736
no cargo/rustc process on tango-1-1
```

### Replay Parity

Recorded replay parity run:

- Run: `25374110073`
- Window: `2026-05-05T19:33:30+08:00 -> 2026-05-05T19:35:30+08:00`
- Deployment: `pm5d.threelayer.settlement-probability-btc-eth.dryrun`
- Recording:
  `/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson`
- Result:
  - `strict_parity_ready=true`
  - orders `2/2/2`
  - fills `2/2/2`
  - `blocking_risk_flags=[]`
  - decision `continue`

Advisory flags remain:

- `replay_has_no_event_level_rows`
- `events_present_in_dryrun_missing_from_replay`

These are not blocking for current runtime order/fill strict parity, but they
remain a quality gap before any future stronger event-level parity claim.

### Short-Window Smoke

Short-window PRD gate:

- Parent gate run: `25374918500`
- Snapshot run: `25374930653`
- Walk-forward run: `25375300501`
- Inputs:
  - `symbols=BTCUSDT,ETHUSDT`
  - `stake_usd=15`
  - `audit_lookback_hours=1`
  - `replay_parity_run_id=25374110073`
  - `replay_parity_artifact_name=recorded-replay-parity-25374110073`

Promotion gate excerpt:

```text
ready_for_dry_run_handoff=false
data_quality=true
deribit_vol_surface=true
full_depth_entry_capacity=true
conservative_entry_capacity=true
probability_calibration=true
full_depth_settlement_edge=true
conservative_settlement_edge=true
anti_overfit_diagnostics=true
symbol_holdout=true
recorded_replay_parity=true
walk_forward_oos=false
```

Interpretation: workflow shape and parity linkage are correct. This is not
promotion evidence because the short window has no non-empty OOS windows.

### Retained-Window Blocker

Retained PRD gate:

- Gate run: `25374406705`
- Snapshot run: `25374411431`
- Inputs:
  - `symbols=BTCUSDT,ETHUSDT`
  - `stake_usd=15`
  - `audit_lookback_hours=168`
  - `replay_parity_run_id=25374110073`
  - `replay_parity_artifact_name=recorded-replay-parity-25374110073`

Failure point: `Audit required market data`, before snapshot compilation.

Required `pm5d-vol` sources:

- `polymarket_quotes`
- `polymarket_orderbooks`
- `deribit_iv`
- `deribit_atm_greeks`
- `binance_price`
- `binance_agg_trades`
- `binance_lob`

Critical gaps:

```text
polymarket_quotes: max gap 1705m >= 15m
polymarket_orderbooks: max gap 1705m >= 15m
deribit_iv: max gap 1695-1700m >= 15m
deribit_atm_greeks: max gap 1700m >= 15m
binance_price/BTCUSDT: max gap 1700m >= 15m
binance_agg_trades/BTCUSDT: max gap 1695m >= 15m
binance_lob/BTCUSDT: max gap 1695m >= 15m
binance_price/ETHUSDT: max gap 1700m >= 15m
binance_agg_trades/ETHUSDT: max gap 1695m >= 15m
binance_lob/ETHUSDT: max gap 1695m >= 15m
```

Direct Tango audit after the failed gate:

- `lookback_hours=1`: `overall_status=ok`
- `lookback_hours=168`: `overall_status=critical`

Interpretation: current ingestion is healthy. The retained gate is blocked by a
historical hole inside the 168h lookback, not by current live capture.

## Open Blockers

### Blocker 1: Retained Data Continuity

Issue: `https://github.com/proerror77/ploy/issues/339`

The retained strict PRD gate cannot pass until every required `pm5d-vol` source
has continuous coverage over the audit window.

Current practical path:

1. Wait until the 2026-05-04/2026-05-05 collection outage rolls out of the
   168h lookback, roughly after `2026-05-12 09:55 +08:00`.
2. Or implement and verify lossless backfill for every required source.

The runbook exposes historical PM orderbook and Deribit IV backfill paths, but
there is no current repo evidence of a lossless historical Binance LOB backfill
for the missing WebSocket partial-depth interval. Do not bypass the strict gate
by substituting candles or synthetic LOB rows.

### Blocker 2: Full-Depth Runtime Parity

Runtime evidence intentionally records:

```text
runtime_price_basis=top_book_quote
full_depth_runtime_parity=false
```

This is acceptable for dry-run audit visibility. It is not acceptable as a live
or full-depth runtime-readiness claim.

## Next Valid Actions

1. Keep the settlement dry-run collecting paper evidence.
2. Keep monitoring quick data audit; current 1h audit is healthy.
3. Re-run the retained PRD gate after the 168h data hole rolls out, or after a
   validated full-source backfill.
4. Only if the retained gate passes, promote the next engineering slice:
   runtime full-depth/conservative execution approximation or true CLOB sweep
   parity.

## Completion Criteria Not Yet Met

The PRD is complete only when:

1. Retained strict `pm5d-vol` data audit passes.
2. Retained snapshot-backed walk-forward has non-empty OOS windows.
3. Promotion gate reports `ready_for_dry_run_handoff=true` using the
   settlement-specific replay parity artifact or a newer equivalent artifact.
4. Remaining live/full-depth runtime parity limitations are either implemented
   or explicitly kept as dry-run-only blockers.

