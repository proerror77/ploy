# Research Data Architecture Review - 2026-05-23

## Scope

This review covers the PM5D / AutoFactor research chain after the Research OS
trace migration and the first legacy workflow cutover:

1. Raw source surfaces.
2. Complete sampled research snapshots.
3. Alpha-search / factor attribution.
4. Candidate replay and runtime parity.
5. Durable Research OS trace and Research Manager planning.

It does not approve dry-run or live promotion. The current reviewed stage is
`factor_attribution` / `walk_forward` infrastructure, with candidate replay
reserved for `executable_replay` evidence.

## Current Shape

| Layer | Current state | Review |
| --- | --- | --- |
| Raw source surfaces | Tango PostgreSQL plus cold lake paths for CLOB/orderbook data | Exists, but not all research runners consume raw/full-resolution surfaces directly |
| Research snapshot | `research_snapshot_compile` emits immutable complete sampled artifacts with source-surface metadata and canonical `gate_category` values | Canonical factor-search input; not full-resolution execution replay evidence |
| Alpha-search | Hosted artifact walk-forward emits registry previews, typed runtime contracts, MCTS tree, search feedback, promotion/handoff artifacts | Usable for factor discovery and attribution |
| Candidate replay | `candidate_replay_tapes` exists and `persist_research_trace` can link replay artifacts to factor evaluations | Durable replay identity now exists; true runtime replay evidence must still be supplied per candidate |
| Durable trace | Hosted walk-forward defaults to trace persistence and writes Research OS rows when DB secrets and deployed binaries are available | Current `main` run `26336054813` persisted trace and uploaded `research-trace/persisted.env` |
| Research Manager | `research_trace_plan` can read durable DB trace and emit next-plan JSON | Current `main` run `26336127621` produced a `fix_data` plan; executor exists but automatic closed-loop execution is still unproven |
| Legacy research paths | Factor routers and Event ML rolling evidence are artifact-backed only | Former direct `ploy-ci-1` DB execution/export branches have been removed from the active research chain |
| Diagnostic backtest | `backtest.yml` now emits `evidence-stage` artifacts with `promotion_ready=false` | Useful diagnostic replay/backtest surface; not promotion evidence |

## Fixed In This Migration

1. **Backtest evidence can no longer imply promotion.**

   `backtest.yml` publishes an explicit `diagnostic` evidence-stage contract,
   labels issues with `evidence:diagnostic`, and forces
   `promotion_ready=false` even if an underlying artifact claims otherwise.

2. **Factor review is explicitly attribution-only.**

   `factor-review-v2-hosted-artifact.yml` publishes machine-readable
   `factor_attribution` stage artifacts and blocks `dry_run_candidate` /
   `live_candidate` as next stages.

3. **Candidate replay is a durable trace object.**

   Migration `044_candidate_replay_tapes.sql`, the Research OS registry types,
   and `persist_research_trace` now persist candidate replay identity,
   provenance, evidence stage, runtime score, strategy profile, metrics,
   blockers, and links to `factor_evaluations`.

4. **AutoFactor side effects require durable trace provenance.**

   Promotion side effects now require the persisted trace marker plus snapshot
   provenance, reject direct-DB/debug/self-hosted source markers, and validate
   that the ready handoff embeds a runtime candidate replay tape before handoff
   issue or config PR creation.

5. **Legacy factor direct-DB branches are removed.**

   `factor-review-v2.yml` and `factor-walk-forward-v2.yml` no longer build or
   run `ploy-research` on `ploy-ci-1`. They route snapshot-backed requests to
   the GitHub-hosted artifact workflows and fail closed when `snapshot_run_id`
   is missing.

6. **Legacy Event ML direct-DB export is removed.**

   `event-ml-rolling-evidence.yml` no longer builds the database-backed
   `factor_research` exporter or runs on `ploy-ci-1`. It requires an existing
   event-root dataset artifact via `source_dataset_run_id` and fails closed when
   that artifact provenance is missing.

7. **Shared AutoFactor accounting is now generated from one active catalog.**

   `config/autofactor_accounting_catalog.json` is consumed by Rust
   alpha-search/persistence code and Python promotion/replay builders. Runtime
   candidate replay now rejects target/horizon mismatches against the selected
   factor row before emitting replay evidence.

8. **Active sampled snapshot naming has been cut away from legacy `full`
   terminology.**

   Active dispatch/provenance fields now use `upload_sampled_snapshot` and
   `sampled_snapshot_embedded`. `research-snapshot.yml` keeps a backward
   compatible input alias for older dispatch payloads, but downstream artifacts
   no longer describe retained sampled products as unsampled source data.

## Current Verified Runs

| Run | Git ref / source | Result | Meaning |
| --- | --- | --- | --- |
| `26336054813` | `main` at `a5234cbfdf3cb93c22aa013888e418b83d308399` | Hosted walk-forward succeeded; `persisted=true`; stages `factor_attribution,walk_forward` | Snapshot -> walk-forward -> durable trace path is working from current main |
| `26336127621` | `main` Research Trace Plan | Succeeded; `schema_version=research_trace_plan.v1`; `theme=fix_data`; `candidate_count=0` | Research Manager can read trace and produce a typed next plan |

The current walk-forward evidence is deliberately blocked:

- `sweep-summary.json` reports `decision=blocked` and `qualified_count=0`.
- Candidate replay remains a diagnostic aggregate
  (`basis=factor_walk_forward_top_bucket_aggregate`) and is blocked by
  `no_runtime_mappable_candidate`.
- Snapshot contract blockers include
  `sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots`,
  so the run cannot make full-depth executable handoff claims.
- The snapshot backing this run contains `BTCUSDT`, `ETHUSDT`, and `SOLUSDT`;
  earlier six-symbol dispatch was invalid for that snapshot scope.

## Remaining Problems

1. **Research Manager is not proven closed-loop automation.**

   `research_trace_plan` produces a typed plan from DB trace, and
   `research-manager-execute-plan.yml` can dry-run bounded follow-up actions.
   The missing proof is an executed loop that reads the plan, dispatches the
   next research workflow, attaches evidence, and updates the next issue/run
   without manual artifact interpretation.

2. **Feature snapshots are still sampled research products, not full execution
   tapes.**

   Snapshot manifests carry source surfaces, cadence, `raw_full_fidelity`,
   `snapshot_sampled`, and `gate_category`. AutoFactor promotion and aggregate
   candidate replay now consume that manifest and attach blocking flags when a
   `required_for_execution` surface is sampled, so sampled rows cannot suppress
   full-depth fillability blockers. The remaining data-layer gap is the
   positive replacement: durable full-depth lake/runtime replay evidence must
   become the normal executable handoff source.

3. **Runtime input canonicalization is now gated for AutoFactor promotion/replay,
   but not yet a shared cross-language source of truth.**

   Factor registry contracts now carry typed runtime metadata, and Python
   promotion/replay consumers fail closed when registry previews are present
   but contracts are missing, blocked, or semantically mismatched. The remaining
   gap is deeper: Rust runtime scoring and Rust/Python contract catalogs still
   need one generated/shared source for feature names, horizons, LOB surfaces,
   blocker semantics, and strategy profile/family mapping.

4. **Multi-horizon label/accounting has a shared AutoFactor catalog, but the
   broader label engine still needs generated contracts.**

   AutoFactor promotion now checks that the candidate runtime replay tape's
   `source_factor.target/horizon` matches the factor row being promoted.
   Runtime/aggregate replay artifacts write the same target/horizon into their
   decision contract, and AutoFactor target/horizon/accounting metadata now
   comes from `config/autofactor_accounting_catalog.json`. The remaining
   architecture gap is consolidating 30s / 60s / 5m / 15m label definitions
   beyond AutoFactor into one generated catalog shared by Rust research, runtime
   replay builders, and persistence.

5. **DuckDB should remain a query accelerator, not durable state.**

   Durable research registry, promotion decisions, replay tape identity, and
   experiment trace belong in PostgreSQL plus immutable artifacts/lake files.
   DuckDB can be used for local Parquet scans, but not as the authoritative
   strategy discovery state layer.

## Required Next Work

| Priority | Work | Done when |
| --- | --- | --- |
| P0 | Execute the Research Manager `fix_data` plan in dry-run and then bounded execute mode | Executor artifact dispatches the next snapshot/data-audit or hosted walk-forward run and records issue/run linkage |
| P0 | Repair or replace sampled execution-surface evidence | Full-depth CLOB lake or runtime replay evidence replaces sampled `clob_orderbook_snapshots` for executable handoff |
| P0 | Explain why latest trace plan had empty factor registry/latest run arrays | Query path either links run `26336054813` rows or records why no runtime-mappable factor rows were persisted |
| P1 | Promote runtime input canonicalization to a generated shared contract | Rust runtime scoring, Rust alpha-search, and Python promotion/replay use one source of truth instead of mirrored catalogs |
| P1 | Complete full-depth executable evidence layer | Runtime replay or full-depth lake evidence replaces sampled snapshot rows for executable handoff |
| P1 | Generate shared non-AutoFactor label contracts | Runtime, research, replay, and trace persistence derive 30s / 60s / 5m / 15m label definitions from one source |

## Verdict

The architecture is now much cleaner than the mixed state: snapshot artifacts,
factor attribution, durable trace, candidate replay, and diagnostic backtest
have separate evidence stages and the old direct-DB factor execution branch has
been cut out of the active workflows.

The research chain has been restored through durable trace planning, but not
through strategy discovery or dry-run handoff. The proven sequence is:

```text
research-snapshot.yml
  -> factor-walk-forward-v2-hosted-artifact.yml
  -> persist Research OS trace
  -> research-trace-plan.yml
```

The missing sequence is:

```text
research-trace-plan.yml
  -> research-manager-execute-plan.yml
  -> next bounded snapshot/data-audit or hosted walk-forward run
  -> issue/evidence update
  -> revised candidate or explicit rejection
```

Only after that loop runs without manual artifact interpretation should the
system be described as automatic research/backtest/strategy discovery. Today it
is a separated, trace-backed research architecture whose current result is
`fix_data` with zero qualified candidates, not a tradable strategy.
