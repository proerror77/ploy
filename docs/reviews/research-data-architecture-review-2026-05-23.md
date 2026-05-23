# Research Data Architecture Review - 2026-05-23

## Scope

This review covers the PM5D / AutoFactor research chain after the Research OS
trace migration:

1. Raw source surfaces.
2. Sampled research snapshots.
3. Alpha-search / factor attribution.
4. Candidate replay and runtime parity.
5. Durable Research OS trace and Research Manager planning.

It does not approve dry-run or live promotion. The current reviewed stage is
`factor_attribution` plus research infrastructure.

## Current Shape

| Layer | Current state | Review |
| --- | --- | --- |
| Raw source surfaces | Tango PostgreSQL plus cold lake paths for CLOB/orderbook data | Exists, but not all research runners consume raw/full-resolution surfaces directly |
| Research snapshot | `research_snapshot_compile` emits immutable sampled artifacts with source-surface metadata and canonical `gate_category` values | Canonical factor-search input; not full-resolution replay evidence |
| Alpha-search | Hosted artifact walk-forward emits registry previews, MCTS tree, search feedback, promotion/handoff artifacts | Usable for factor discovery and attribution |
| Durable trace | `persist_research_trace` writes snapshot/search artifacts into Research OS tables | Queryable foundation now exists, gated behind explicit workflow option |
| Research Manager | `research_trace_plan` can read durable DB trace and emit next-plan JSON | Planning surface exists; not yet wired to issue creation or scheduled research |
| Legacy paths | `factor-review-v2.yml` / `factor-walk-forward-v2.yml` still exist as compatibility routers | Direct `ploy-ci-1` DB branches are debug-only and blocked by default |

## Main Problems

1. **Candidate replay is still not a first-class durable data product.**

   `factor_evaluations.candidate_replay_id` now reserves the boundary, but
   there is not yet a `candidate_replay_tapes` table or writer. A sampled
   research snapshot can support factor attribution, but executable claims still
   need a concrete replay tape with exact `MarketUpdate` sequence, scorer input,
   executable price, fillability, and event-level accounting.

2. **Trace persistence is available but not yet exercised on real production
   evidence.**

   Hosted walk-forward can persist when
   `options_json.persist_research_trace=true` and a protected DB secret exists.
   Until migration 043 is deployed and one real run persists rows, Research
   Manager planning from DB remains locally verified code rather than observed
   production behavior.

3. **Raw full-depth CLOB and sampled snapshot semantics are now explicit, but
   still easy to misuse in downstream reports.**

   The manifest records `gate_category`, `raw_full_fidelity`, and
   `snapshot_sampled`, and the runbook says sampled snapshot is not full data.
   `research_trace_plan` derives promotion blockers from required execution
   surfaces that are missing, unmaterialized, or only sampled. Any future report
   that uses full-depth execution language must cite candidate replay or
   full-depth lake evidence, not only snapshot rows.

4. **DuckDB is not the canonical stable data layer.**

   The stable source of truth remains PostgreSQL plus retained artifacts/lake
   files. DuckDB can still be useful as a local query engine over Parquet, but
   it should not become the durable registry or promotion state. Research OS
   state belongs in PostgreSQL tables; heavy analytical scans should use
   immutable Parquet/ZSTD snapshots or lake files.

5. **Research Manager is not yet closed-loop automation.**

   `research_trace_plan` produces a typed plan from DB trace. It does not yet
   create GitHub issues, dispatch next hosted runs, or write typed priors back
   into the alpha-search chain.

6. **Legacy diagnostic backtest remains separate from Research OS trace.**

   `backtest.yml` is explicitly non-promotion evidence and still uses legacy
   quote-tick Parquet semantics. It should not feed promotion unless replay,
   runtime scorer parity, and full-depth fillability evidence are attached.

## Required Next Work

| Priority | Work | Done when |
| --- | --- | --- |
| P0 | Deploy migration 043 and install `persist-research-trace` / `research-trace-plan` on Tango | `deploy-tango-1-1.yml` succeeds from `main`, remote binaries exist, migration is applied |
| P0 | Run one hosted walk-forward with `persist_research_trace=true` | DB contains rows for the run in all four Research OS tables |
| P0 | Run `research-trace-plan` against the DB trace | Plan JSON references latest trace rows and returns `continue_search`, `revise_prior`, `fix_data`, or `fix_runtime` |
| P1 | Add `candidate_replay_tapes` durable schema and writer | Replay tape identity is separate from sampled snapshot identity and links to `factor_evaluations.candidate_replay_id` |
| P1 | Wire Research Manager plan to issue/dispatch automation | A durable trace can trigger the next bounded hosted research run without manual artifact reading |
| P1 | Move remaining diagnostic-only reports behind explicit evidence-stage labels | No artifact can imply dry-run/live readiness without parity gates |

## Review Verdict

The architecture is now moving in the right direction: raw data, sampled
research snapshots, alpha-search artifacts, durable trace, and planning are
separated. The system is not yet a fully automatic research/backtest/strategy
discovery loop because the durable trace has not been production-exercised and
candidate replay is still not a first-class persisted layer.

The next concrete milestone is one end-to-end run:

```text
research-snapshot.yml
  -> factor-walk-forward-v2-hosted-artifact.yml with persist_research_trace=true
  -> research-trace-plan
  -> next alpha-search issue/run
```

Only after that loop is observed should we claim the research chain has been
restored.
