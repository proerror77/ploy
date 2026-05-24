# Research Data Architecture Review - 2026-05-25

Evidence stage: `diagnostic` / architecture review. This review describes data
and research-chain readiness. It is not dry-run, live, or strategy promotion
evidence.

Reviewed ref: `origin/main` at
`82361657c1e8ec4feb99aa720a85fce25a5255f8`.

## Executive Verdict

The research chain is now separated enough to run an automatic loop:

```text
research-snapshot.yml
  -> factor-walk-forward-v2-hosted-artifact.yml
  -> persist Research OS trace
  -> research-trace-plan.yml
  -> research-manager-execute-plan.yml
  -> runtime-candidate-replay.yml / recorded-replay-parity.yml
  -> new hosted walk-forward or rejection
```

That means Ploy can automatically produce factor attribution, walk-forward
diagnostics, durable trace rows, Research Manager plans, bounded follow-up
workflow dispatches, runtime candidate replay, and rejection decisions.

It has not yet proven an automatically tradable strategy. The latest dry-run
candidate evidence is explicitly blocked: dry-run candidate gate run
`26369050956` found residual runtime evidence after config sync
(`82` buy orders, `80` closed trades, `2` open positions), and the report
summary at that time showed `266` trades, `264` closed trades, win rate
`39.8%`, and realized PnL `-714.35`. That is a clean-baseline blocker, not a
strategy approval.

## Current Data Shape

| Layer | Current truth surface | Review |
| --- | --- | --- |
| Raw collector data | Tango PostgreSQL tables, including `binance_lob_ticks`, quote ticks, trades, settlements, and hot `clob_orderbook_snapshots` | Raw data is the source material. Research workflows should compile artifacts from it instead of repeatedly joining raw tables during every search. |
| Full-depth CLOB archive | `/opt/ploy/data/lake/orderbook_snapshots/date=YYYY-MM-DD/hour=HH/` with Parquet/ZSTD, manifests, and `_SUCCESS` markers | This is the right execution-depth lake. It is full-fidelity by policy and separate from sampled research snapshots. |
| Research snapshots | Retained `research-snapshot.yml` artifacts with manifest, source surfaces, sampling, quality report, and data audit | Correct canonical input for factor search and walk-forward, but still sampled. A sampled snapshot is not full-depth execution proof. |
| Runtime replay tapes | `runtime-candidate-replay.yml` artifacts and `candidate_replay_tapes` Research OS rows | Correct pre-dry-run proof surface when `basis=runtime_market_update_replay`, official settlement is complete, and full-depth entry is confirmed. |
| Durable Research OS state | PostgreSQL tables `research_dataset_snapshots`, `factor_registry`, `factor_evaluations`, `experiment_trace`, and `candidate_replay_tapes` | Correct durable layer. This should remain the queryable system of record for research lineage and promotion decisions. |
| DuckDB | Local/CI Parquet query accelerator and export helper | Useful execution engine, not durable state. The problem is not "data is not in DuckDB"; the problem is whether artifacts preserve the right fidelity and are linked into trace. |

## What Is Now Correct

1. **Legacy PRD snapshot fallback is removed.**

   PR `#686` removed `--allow-legacy-snapshot-build` from
   `scripts/run_settlement_probability_prd_gate.py`. The settlement-probability
   PRD gate now only consumes retained research snapshots and fails before
   dispatch when `--snapshot-run-id` is missing.

2. **AutoFactor runtime input projection has a shared catalog.**

   PR `#685` added `config/autofactor_runtime_contract_catalog.json` and made
   Rust alpha-search plus Python promotion/replay helpers consume it. Unsupported
   inputs such as `external_pressure` and `iv_change_1m` are contract blockers,
   not silently inferred runtime fields.

3. **Research lineage has durable queryable state.**

   Migrations `042`, `043`, and `044` define `factor_registry`,
   `factor_evaluations`, append-only `experiment_trace`,
   `research_dataset_snapshots`, and `candidate_replay_tapes`. This is the
   correct place for `dsl_hash`, `ast_json`, runtime contract, run id, dataset
   window, blockers, replay identity, and promotion decision.

4. **Legacy router workflows are removed.**

   `factor-review-v2.yml` and `factor-walk-forward-v2.yml` have been removed.
   Factor review and walk-forward now dispatch directly through the hosted
   artifact workflows, which require `snapshot_run_id`.

5. **Full-depth CLOB is no longer confused with sampled snapshots.**

   `collect-full-depth-execution-surface.yml` produces
   `full_depth_execution_surface.v1` from the CLOB orderbook archive. Hosted
   walk-forward can consume that proof to satisfy execution-surface blockers
   for matching windows.

## Remaining Architecture Problems

### P0 - Dry-run evidence is contaminated until reset/clean baseline passes

The dry-run config was synced, but dry-run candidate gate run `26369050956`
failed in `clean-baseline` mode because old runtime evidence remained:

- `buy_orders=82`
- `closed_trades=80`
- `open_positions=2`
- `reason=residual_runtime_evidence`

Do not evaluate the new dry-run candidate quality until the runtime evidence
reset has completed, the strategy has restarted cleanly, and a fresh candidate
gate run proves the baseline has no residual orders/fills.

### P0 - Full-depth execution proof is still artifact-first, not fully trace-first

The full-depth execution surface exists and hosted walk-forward can consume it,
but it is still primarily passed around as a workflow artifact/run id. It should
be a first-class durable Research OS object, keyed by surface, window, archive
root, manifest hashes, row count, and completeness status, then linked from
factor evaluations and candidate replay tapes.

Done means Research Manager can query full-depth coverage from durable trace
without recovering hidden artifact ids from prior workflow runs.

### P0 - Official settlement completeness remains a hard blocker

Settlement-probability candidates must not pass promotion unless every traded
event has official settlement coverage. Settlement repair workflows can produce
evidence, but missing settlement rows must route to data repair, not economics
approval.

### P0 - Tick-level LOB and sampled factor snapshots need an explicit contract

The user concern about "LOB should be WebSocket/tick-level, not 30 seconds" is
valid at the raw-data layer: the collector path stores `binance_lob_ticks` and
the tick-preserving optimize runbook explicitly forbids adding a bucket/stride
layer to raw LOB replay. However, factor walk-forward snapshots intentionally
compile sampled observations by default.

The missing contract is not "put everything in DuckDB." The missing contract is
two named products:

- **sampled factor snapshot**: compact factor-attribution tape, with sampling
  cadence recorded and no full-depth execution claim.
- **tick/full-depth execution tape**: ordered MarketUpdate or Parquet tape used
  for candidate replay, fillability, slippage, and runtime parity.

Until those are distinct in every artifact and trace row, agents can still ask
the wrong question: "what is the return of this sampled factor row?" instead of
"can this event-level runtime decision be filled at the configured stake?"

### P1 - Label contracts are only partially centralized

`config/autofactor_accounting_catalog.json` centralizes AutoFactor target,
horizon, lane, strategy profile, and execution requirements. That is good for
AutoFactor, but the broader 30s / 60s / 5m / 15m label engine is not yet a
generated shared contract across research, runtime replay builders, and trace
persistence.

### P1 - Runtime input catalog is shared for AutoFactor, not all feature lanes

`config/autofactor_runtime_contract_catalog.json` fixed the immediate
AutoFactor promotion problem. Event ML, LOB ML, optimize/backtest lanes, and
future non-AutoFactor runtime scorers still need generated feature schemas with
the same fail-closed semantics.

### P1 - Research Manager needs stronger negative-economics actions

The loop can reject and revise, but persistent negative executable ROI should
produce stronger family-level avoidance and explicit new search constraints.
Otherwise the closed loop can keep rediscovering variants of the same
economically weak thesis.

### P2 - Some older architecture docs are now superseded

`docs/architecture/research-backtest-rearchitecture.md` still describes the
April proposed design with `ploy-ci-1` as the research runner. Current active
workflow docs supersede that for factor search: retained snapshots are built
from Tango data and consumed by GitHub-hosted artifact workflows, while durable
trace persistence runs through Tango only when private DB access requires it.

## DuckDB Answer

DuckDB should not become the authoritative durable layer. It is the right tool
for fast Parquet scans, export verification, and bounded replay loading. The
durable layer should stay:

```text
PostgreSQL Research OS tables
  + immutable GitHub artifacts
  + cold Parquet/ZSTD lake files
```

The concrete fix is to ensure every DuckDB/Parquet product has a manifest hash,
window, source-surface contract, sampling/fidelity declaration, and durable
trace pointer. Moving state into DuckDB without those contracts would make the
architecture less auditable.

## Next Work

| Priority | Work | Done when |
| --- | --- | --- |
| P0 | Reset contaminated dry-run evidence and rerun clean-baseline gate | Candidate gate reports no residual orders/fills for the target deployment before candidate-quality evaluation |
| P0 | Promote full-depth execution surface into Research OS trace | Research Manager can query full-depth proof by window/surface without manual artifact ids |
| P0 | Keep official settlement completeness fail-closed | Every replay-fed handoff proves official settlement for all traded events |
| P0 | Split sampled factor snapshots from tick/full-depth execution tapes in manifests and trace | Agents cannot use sampled factor rows as full-depth execution evidence |
| P1 | Generate shared multi-horizon label contracts | Research, replay, runtime, and trace persistence use one target/horizon/accounting schema |
| P1 | Expand shared runtime feature schemas beyond AutoFactor | Event ML, LOB ML, optimize/backtest, and future runtime scorers all fail closed on unsupported inputs |
| P1 | Strengthen negative-ROI Research Manager priors | Repeated negative runtime replay closes or strongly penalizes weak factor families |
| P2 | Mark stale architecture docs superseded | Readers land on this current review or the current runbooks instead of the April proposed design |

## Can We Start Researching Strategies?

Yes, but only in the strict loop:

1. Reset and prove clean dry-run baseline before judging candidate-quality.
2. Use retained research snapshots for factor discovery.
3. Require typed runtime contracts before candidate replay.
4. Require runtime MarketUpdate replay, full-depth entry, official settlement,
   and positive executable ROI before dry-run handoff.
5. Treat sampled snapshot metrics, fillability diagnostics, and backtest-only
   PnL as research evidence, not tradable strategy evidence.

The architecture is good enough to continue automated discovery. It is not good
enough to relax promotion gates.
