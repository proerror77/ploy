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
| Durable trace | Hosted walk-forward defaults to trace persistence and writes Research OS rows when DB secrets and deployed binaries are available | Queryable foundation exists; current post-PR #615 deploy/run verification is pending |
| Research Manager | `research_trace_plan` can read durable DB trace and emit next-plan JSON | Planning surface exists; not yet wired to automatic issue creation or workflow dispatch |
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
   provenance and reject direct-DB/debug/self-hosted source markers before
   handoff issue or config PR creation.

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

## Remaining Problems

1. **The post-PR #615 deploy has not completed yet.**

   The required Tango deploy run is still waiting on the protected environment.
   Until it succeeds, migration `044_candidate_replay_tapes.sql` and the
   deployed `persist-research-trace` / `research-trace-plan` binaries are not
   verified for the current `main` SHA.

2. **Trace-backed research has not been rerun from current `main`.**

   Earlier trace persistence was observed, but after the latest evidence-stage
   and replay provenance changes, the chain still needs a fresh hosted
   walk-forward run from `main` and a fresh Research Manager plan over the
   resulting DB trace.

3. **Research Manager is not closed-loop automation.**

   `research_trace_plan` produces a typed plan from DB trace, but it does not
   yet create GitHub issues, dispatch the next hosted run, or write typed priors
   back into the alpha-search chain.

4. **Feature snapshots are still sampled research products, not full execution
   tapes.**

   Snapshot manifests carry source surfaces, cadence, `raw_full_fidelity`,
   `snapshot_sampled`, and `gate_category`. AutoFactor promotion and aggregate
   candidate replay now consume that manifest and attach blocking flags when a
   `required_for_execution` surface is sampled, so sampled rows cannot suppress
   full-depth fillability blockers. The remaining data-layer gap is the
   positive replacement: durable full-depth lake/runtime replay evidence must
   become the normal executable handoff source.

5. **Runtime input canonicalization is now gated for AutoFactor promotion/replay,
   but not yet a shared cross-language source of truth.**

   Factor registry contracts now carry typed runtime metadata, and Python
   promotion/replay consumers fail closed when registry previews are present
   but contracts are missing, blocked, or semantically mismatched. The remaining
   gap is deeper: Rust runtime scoring and Rust/Python contract catalogs still
   need one generated/shared source for feature names, horizons, LOB surfaces,
   blocker semantics, and strategy profile/family mapping.

6. **Multi-horizon label/accounting is not a single hard gate yet.**

   Reports compute event-level fields such as unique event count and max event
   decisions, but the final persisted approval layer still needs to enforce the
   one-event-one-decision accounting contract across 30s / 60s / 5m / 15m
   horizons.

7. **DuckDB should remain a query accelerator, not durable state.**

   Durable research registry, promotion decisions, replay tape identity, and
   experiment trace belong in PostgreSQL plus immutable artifacts/lake files.
   DuckDB can be used for local Parquet scans, but not as the authoritative
   strategy discovery state layer.

## Required Next Work

| Priority | Work | Done when |
| --- | --- | --- |
| P0 | Approve/complete `deploy-tango-1-1.yml` run `26330884546` from `main` | Workflow succeeds for SHA `c2e4fde4c74b44d7e03a49d99142cbb702560122` |
| P0 | Verify current Tango research deployment | Migration `044_candidate_replay_tapes.sql` is applied and `/opt/ploy/bin/persist-research-trace` / `/opt/ploy/bin/research-trace-plan` are executable |
| P0 | Run one hosted walk-forward from current `main` with default trace persistence | DB contains current-run rows across Research OS trace tables |
| P0 | Run `research-trace-plan.yml` against the fresh trace | Plan JSON references latest trace rows and returns `continue_search`, `revise_prior`, `fix_data`, `fix_runtime`, or `fix_workflow` |
| P1 | Add Research Manager action executor | Plan output can open/link issues and dispatch bounded hosted research reruns without manual artifact reading |
| P1 | Promote runtime input canonicalization to a shared generated contract | Rust runtime scoring, Rust alpha-search, and Python promotion/replay use one source of truth instead of mirrored catalogs |
| P1 | Complete full-depth executable evidence layer | Runtime replay or full-depth lake evidence replaces sampled snapshot rows for executable handoff |
| P1 | Enforce multi-horizon accounting at persisted approval layer | Multi-decision or horizon-mixed artifacts are blocked before handoff |

## Verdict

The architecture is now much cleaner than the mixed state: snapshot artifacts,
factor attribution, durable trace, candidate replay, and diagnostic backtest
have separate evidence stages and the old direct-DB factor execution branch has
been cut out of the active workflows.

The research chain is not fully restored until the current deploy completes and
one fresh `main` run proves this sequence:

```text
research-snapshot.yml
  -> factor-walk-forward-v2-hosted-artifact.yml
  -> persist Research OS trace
  -> research-trace-plan.yml
  -> next bounded research issue/run
```

Only after that observed loop should the system be described as automatic
research/backtest/strategy discovery. Today it is a mostly separated research
architecture with the final deploy, rerun, and closed-loop automation still
pending.
