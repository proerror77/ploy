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
| Candidate replay | `candidate_replay_tapes` exists and `persist_research_trace` can link replay artifacts to factor evaluations | Durable replay identity exists, and hosted walk-forward can now fan out runtime replay requests from unblocked runtime contracts |
| Durable trace | Hosted walk-forward defaults to trace persistence and writes Research OS rows when DB secrets and deployed binaries are available | Current `main` run `26344749058` persisted trace and uploaded `research-trace/persisted.env` |
| Research Manager | `research_trace_plan` can read durable DB trace and emit next-plan JSON; `research-manager-execute-plan.yml` and hosted walk-forward can dispatch bounded follow-up evidence | Main runs have proven executor dispatch, recorded replay parity, and closed-loop runtime replay fan-out |
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

9. **Closed-loop runtime replay fan-out is automated.**

   Hosted walk-forward now emits a deduped `runtime_replay_requests` batch while
   preserving the legacy single `runtime_replay_request` field. Dispatch is
   gated on successful durable trace persistence and closed-loop
   `action=fix_runtime`. Contract-blocked candidates, including runtime input
   semantic mismatches such as `external_pressure`, are excluded before
   dispatch.

## Current Verified Runs

| Run | Git ref / source | Result | Meaning |
| --- | --- | --- | --- |
| `26336054813` | `main` at `a5234cbfdf3cb93c22aa013888e418b83d308399` | Hosted walk-forward succeeded; `persisted=true`; stages `factor_attribution,walk_forward` | Snapshot -> walk-forward -> durable trace path is working from current main |
| `26336127621` | `main` Research Trace Plan | Succeeded; `schema_version=research_trace_plan.v1`; `theme=fix_data`; `candidate_count=0` | Research Manager can read trace and produce a typed next plan |
| `26340352094` | `main` deploy to `tango-1-1` | Succeeded; SSH path passed and Cloud Assistant fallback skipped | Runtime repair/deploy path is healthy from CI-built artifacts |
| `26340671822` | `main` Research Manager executor | Succeeded; dispatched runtime candidate replay `26340673836` and recorded replay parity `26340674276` | Closed-loop evidence dispatch is proven |
| `26340674276` | recorded replay parity | Succeeded; `strict_parity_ready=true` with no shared-row mismatches | Dry-run/runtime parity proof is available for the checked recording slice |
| `26340673836` | runtime candidate replay | Succeeded but blocked; `updates_processed=43721`, `intents_submitted=0`, `orders=0`, `fills=0` | Current AutoFactor runtime candidate is not tradable and should be revised/rejected |
| `26344523079` | `main` at `a74f030b7e8c288144b5fd5a35a75da9b589bc78` | Hosted walk-forward succeeded; `persisted=true`; dispatched five runtime replay requests from `runtime_replay_requests` | Snapshot -> walk-forward -> durable trace -> runtime replay fan-out is working |
| `26344588318` / `26344588684` / `26344589064` / `26344589439` / `26344589743` | runtime candidate replay batch | All succeeded with `basis=runtime_market_update_replay`; trade counts were `29`, `29`, `8`, `16`, and `15`; fill rate was `1.0` for all | Runtime replay automation works, but every candidate remains below the 50-trade promotion gate |
| `26344749058` | `main` hosted walk-forward with replay `26344588318` | Succeeded; consumed true runtime replay evidence; closed-loop action became `fix_data` with zero replay requests | Promotion now sees runtime replay evidence and blocks on data/trade-count/settlement gates instead of runtime provenance |

The current walk-forward/runtime replay evidence is deliberately blocked:

- `sweep-summary.json` still reports `decision=blocked`.
- Candidate replay is no longer only a diagnostic aggregate for the selected
  replay-fed run: it uses `basis=runtime_market_update_replay`, but remains
  blocked by `trade_count_too_small:29<50`,
  `official_settlement_missing:25<29`, and
  `candidate_strategy_replay_missing_contract:official_settlement`.
- Snapshot contract blockers include
  `sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots`,
  so the run cannot make full-depth executable handoff claims.
- The snapshot backing this run contains `BTCUSDT`, `ETHUSDT`, and `SOLUSDT`;
  earlier six-symbol dispatch was invalid for that snapshot scope.

## Remaining Problems

1. **Runtime replay dispatch is proven, but strategy discovery is not.**

   `research_trace_plan` produces a typed plan from DB trace, and
   `research-manager-execute-plan.yml` has executed bounded follow-up actions.
   Hosted walk-forward now turns positive runtime-mapped rows into bounded
   `runtime-candidate-replay.yml` requests without manual artifact
   interpretation. The current result is still not a dry-run handoff: the best
   fresh runtime replay has 29 trades, complete fills, positive ROI, and
   incomplete official settlement coverage, so promotion correctly blocks. The
   remaining discovery gap is denser candidate formation over more event tape,
   not lowering thresholds.

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
| P0 | Increase runtime replay event density without lowering promotion gates | Runtime replay candidates naturally reach the 50-trade minimum on distinct events, or the candidate family is rejected/revised |
| P0 | Repair or replace sampled execution-surface evidence | Full-depth CLOB lake or runtime replay evidence replaces sampled `clob_orderbook_snapshots` for executable handoff |
| P0 | Complete official settlement coverage for runtime replay candidates | Runtime replay artifacts include official settlement for all traded events before any handoff can become ready |
| P0 | Keep Research Manager candidate replay contract-driven | Executor replays only explicit or trace-derived unblocked runtime contracts and fails closed without one |
| P1 | Promote runtime input canonicalization to a generated shared contract | Rust runtime scoring, Rust alpha-search, and Python promotion/replay use one source of truth instead of mirrored catalogs |
| P1 | Complete full-depth executable evidence layer | Runtime replay or full-depth lake evidence replaces sampled snapshot rows for executable handoff |
| P1 | Generate shared non-AutoFactor label contracts | Runtime, research, replay, and trace persistence derive 30s / 60s / 5m / 15m label definitions from one source |

## Verdict

The architecture is now much cleaner than the mixed state: snapshot artifacts,
factor attribution, durable trace, candidate replay, and diagnostic backtest
have separate evidence stages and the old direct-DB factor execution branch has
been cut out of the active workflows.

The research chain has been restored through durable trace planning, but not
through profitable strategy discovery or dry-run handoff. The proven sequence is:

```text
research-snapshot.yml
  -> factor-walk-forward-v2-hosted-artifact.yml
  -> persist Research OS trace
  -> closed-loop runtime replay request batch
  -> research-trace-plan.yml
  -> research-manager-execute-plan.yml
  -> runtime-candidate-replay.yml / recorded-replay-parity.yml
```

The missing sequence is now:

```text
blocked runtime candidate
  -> closed-loop prior revision / new runtime-mappable candidate
  -> runtime_market_update_replay with at least 50 distinct event trades
  -> handoff only if full-depth execution, official settlement, ROI, fillability, and parity gates pass
```

The system can now automatically research, persist trace, dispatch replay/parity
evidence, and reject a weak candidate. It should not be described as having
found an automatically tradable strategy yet.
