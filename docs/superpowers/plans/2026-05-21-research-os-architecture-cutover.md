# Research OS Architecture Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split Ploy's new FactorEvolve Research OS from the legacy AutoFactor promotion/config path, then restore the CI research loop as a research-only evidence chain.

**Architecture:** The new default chain is `snapshot artifact -> factor walk-forward -> alpha-search artifacts -> Research OS trace -> closed-loop plan/runtime-replay request`. Legacy dry-run handoff/config mutation remains available only through explicit promotion mode and dedicated promotion workflows. Runtime replay is a bridge between planes, not part of discovery.

**Tech Stack:** GitHub Actions, Python orchestration scripts, Rust `ploy-research`, Research OS JSON/SQL contracts, existing AutoFactor DSL artifacts.

---

## Task 8: Automate Daily Evidence Orchestration

**Files:**
- Create: `scripts/resolve_github_artifact.py`
- Modify: `.github/workflows/factor-evolve-daily-research.yml`
- Modify: `.github/workflows/factor-walk-forward-v2-hosted-artifact.yml`
- Modify: `docs/ALPHA_FACTOR_SEARCH_CICD.md`
- Modify: `docs/runbooks/event-ml-automl-workflow.md`
- Test: `tests/test_resolve_github_artifact.py`
- Test: `tests/workflow_security.rs`

- [x] Add a retained artifact resolver for latest full `research-snapshot-*`
      and latest `autofactor-research-trace.json` artifacts.
- [x] Add a scheduled daily research entrypoint that can run without manually
      supplied snapshot or trace run ids.
- [x] Keep daily search in `research_chain_mode=research_only` and preserve
      explicit legacy promotion/config boundaries.
- [x] Let hosted walk-forward dispatch `runtime-candidate-replay.yml` only when
      `closed-loop-decision.json` contains a runtime replay request and
      `options_json.auto_dispatch_runtime_replay=true`.
- [x] Record runtime replay dispatch intent in the uploaded alpha-search-chain
      artifact before dispatching.

## Task 9: Close Runtime Replay Feedback Into Research OS Trace

**Files:**
- Modify: `.github/workflows/runtime-candidate-replay.yml`
- Modify: `.github/workflows/factor-walk-forward-v2-hosted-artifact.yml`
- Modify: `docs/ALPHA_FACTOR_SEARCH_CICD.md`
- Modify: `docs/runbooks/event-ml-automl-workflow.md`
- Test: `tests/workflow_security.rs`

- [x] Keep `runtime-candidate-replay.yml` within the GitHub 10-input limit by
      moving advanced replay/promotion feedback fields into `options_json`.
- [x] Pass the hosted source factor-walk-forward run id/artifact into runtime
      replay dispatches.
- [x] After runtime replay artifact upload, dispatch
      `autofactor-strategy-promotion.yml` in evaluator-only mode when source
      context is present.
- [x] Keep handoff issue creation and config PR creation disabled in the
      automatic feedback path.

## Boundary Map

- New Research OS plane:
  - `.github/workflows/factor-evolve-daily-research.yml`
  - `.github/workflows/factor-walk-forward-v2-hosted-artifact.yml` with `research_chain_mode=research_only`
  - `scripts/run_factor_walk_forward_sweep.py --research-chain-mode research_only`
  - `crates/ploy-research/src/alpha_search.rs`
  - `crates/ploy-research/src/research_os/`
  - `autofactor-research-trace.json`
- Runtime replay bridge:
  - `.github/workflows/runtime-candidate-replay.yml`
  - `scripts/build_runtime_candidate_strategy_replay.py`
  - `candidate-strategy-replay.json` with `basis=runtime_market_update_replay`
- Legacy promotion/config plane:
  - `.github/workflows/autofactor-strategy-promotion.yml`
  - `scripts/evaluate_autofactor_strategy_promotion.py`
  - `scripts/apply_autofactor_handoff_to_config.py`
  - `autofactor-strategy-handoff.json`

## Task 1: Cut Hosted Research Default Away From Legacy Promotion

**Files:**
- Modify: `.github/workflows/factor-walk-forward-v2-hosted-artifact.yml`
- Modify: `.github/workflows/factor-evolve-daily-research.yml`
- Modify: `.github/workflows/factor-walk-forward-v2.yml`
- Modify: `scripts/run_factor_walk_forward_sweep.py`
- Test: `tests/test_factor_walk_forward_sweep.py`
- Test: `tests/workflow_security.rs`

- [x] Add `research_chain_mode` with values `research_only` and `legacy_promotion`.
- [x] Make FactorEvolve Daily pass `research_chain_mode=research_only`.
- [x] In `research_only`, run factor walk-forward and alpha-search artifacts only; do not build aggregate candidate replay and do not run promotion evaluator.
- [x] Keep `legacy_promotion` available for old handoff/config PR behavior.
- [x] Verify with:

```bash
python3 -m unittest tests.test_factor_walk_forward_sweep
CARGO_TARGET_DIR=/tmp/ploy-research-os-cutover rtk cargo test --locked --test workflow_security hosted_factor_walk_forward_has_candidate_replay_feedback_input
```

## Task 2: Make Research OS Trace The Data Contract

**Files:**
- Modify: `scripts/evaluate_autofactor_strategy_promotion.py`
- Modify: `scripts/run_factor_walk_forward_sweep.py`
- Modify: `.github/workflows/autofactor-strategy-promotion.yml`
- Test: `tests/test_autofactor_strategy_promotion.py`

- [x] Emit `autofactor-research-trace.json` with `factor_registry_upserts`, `factor_evaluations`, and `experiment_trace`.
- [x] Preserve `dsl_hash`, `ast_json`, `runtime_contract`, source run, promotion run, dataset window, and promotion decision.
- [x] Put old rows without DSL provenance into `skipped_registry_rows`.
- [x] Verify with:

```bash
python3 -m unittest tests.test_autofactor_strategy_promotion.AutoFactorStrategyPromotionTests.test_emits_research_os_trace_with_runtime_contract_provenance
```

## Task 3: Move Runtime Input Canonicalization Into A Shared Contract

**Files:**
- Create: `crates/ploy-market-contracts/src/runtime_inputs.rs`
- Create: `crates/ploy-research/src/research_os/runtime_inputs.rs`
- Modify: `crates/ploy-market-contracts/src/lib.rs`
- Modify: `crates/ploy-research/src/research_os/mod.rs`
- Modify: `crates/ploy-research/src/alpha_search.rs`
- Modify: `crates/ploy-strategy-bundles/src/strategies/three_layer_model.rs`
- Modify: `crates/ploy-strategy-bundles/src/strategies/three_layer.rs`
- Test: Rust unit tests in `runtime_inputs.rs`, `alpha_search.rs`, and `three_layer*.rs`

- [x] Define `RuntimeInputContract { name, semantic_family, source_surface, runtime_supported, research_supported, blockers }`.
- [x] Register current supported inputs: `model_full_depth_settlement_edge`, `conservative_settlement_edge`, `near_strike_score`, `entry_capacity_score`, `entry_price_quality`, `side_spread`.
- [x] Register blocked research-only inputs: `external_pressure`, `iv_change_1m`, `poly_lag_pressure`.
- [x] Make alpha-search runtime contracts read this registry instead of hard-coded string blockers.
- [x] Make runtime formula scorer reject unsupported inputs with the same blocker ids.
- [x] Verify with:

```bash
CARGO_TARGET_DIR=/tmp/ploy-runtime-input-contract rtk cargo test --locked -p ploy-market-contracts runtime_inputs --lib
CARGO_TARGET_DIR=/tmp/ploy-runtime-input-contract rtk cargo test --locked -p ploy-research alpha_search --lib
CARGO_TARGET_DIR=/tmp/ploy-runtime-input-contract rtk cargo test --locked -p ploy-strategy-bundles autofactor_formula --lib
CARGO_TARGET_DIR=/tmp/ploy-runtime-input-contract rtk cargo test --locked -p ploy-strategy-bundles settlement_autofactor --lib
```

## Task 4: Split Label/Horizon Semantics From Walk-Forward Reports

**Files:**
- Create: `crates/ploy-research/src/research_os/labels.rs`
- Modify: `crates/ploy-research/src/factors_v2.rs`
- Modify: `crates/ploy-research/examples/factor_walk_forward_v2.rs`
- Test: focused Rust tests in `labels.rs` and `factors_v2.rs`

- [x] Define horizon ids: `pm5d_settlement`, `repricing_30s`, `repricing_60s`, `repricing_5m`, `repricing_15m`.
- [x] Encode whether each horizon is event-level or row-level.
- [x] Make settlement targets require one event, one decision accounting.
- [x] Make repricing targets stay diagnostic unless explicitly promoted through a repricing runtime lane.
- [x] Verify with:

```bash
CARGO_TARGET_DIR=/tmp/ploy-label-contract rtk cargo test --locked -p ploy-research label_contract --lib
CARGO_TARGET_DIR=/tmp/ploy-label-contract rtk cargo test --locked -p ploy-research factors_v2 --lib
```

## Task 5: Make LOB Feature Store A First-Class Artifact Contract

**Files:**
- Create: `crates/ploy-research/src/research_os/feature_store.rs`
- Modify: `crates/ploy-research/examples/factor_walk_forward_v2.rs`
- Modify: `.github/workflows/research-snapshot.yml`
- Test: Rust feature-store unit tests and workflow security tests

- [x] Define `FeatureSnapshotManifest { snapshot_id, source_run_id, surfaces, window, symbols, feature_schema_hash }`.
- [x] Emit `feature-snapshot-manifest.json` beside retained research snapshots.
- [x] Require factor walk-forward hosted runs to copy this manifest into `snapshot-provenance/`.
- [x] Mark candidates depending on absent feature surfaces as `missing_blocks_promotion`.
- [x] Verify with:

```bash
CARGO_TARGET_DIR=/tmp/ploy-feature-store rtk cargo test --locked -p ploy-research feature_store --lib
CARGO_TARGET_DIR=/tmp/ploy-feature-store rtk cargo test --locked --test workflow_security
```

## Task 6: Research Manager Reads Trace Instead Of Reports

**Files:**
- Modify: `crates/ploy-research/src/research_os/manager.rs`
- Modify: `crates/ploy-research/examples/factor_evolve_daily_plan.rs`
- Modify: `.github/workflows/factor-evolve-daily-research.yml`
- Test: Rust `research_os` tests and workflow security tests

- [x] Extend `ResearchManagerInput` with `research_trace_summary`.
- [x] Summarize `autofactor-research-trace.json` into latest decisions, blockers, runtime replay requests, and rejected factor families.
- [x] Make `factor_evolve_daily_plan` prefer trace summaries over markdown/report parsing.
- [x] Verify with:

```bash
CARGO_TARGET_DIR=/tmp/ploy-research-manager-trace rtk cargo test --locked -p ploy-research research_os --lib
```

## Task 7: Retire Mixed Legacy Defaults

**Files:**
- Modify: `docs/ALPHA_FACTOR_SEARCH_CICD.md`
- Modify: `docs/runbooks/event-ml-automl-workflow.md`
- Modify: `.github/workflows/factor-walk-forward-v2-hosted-artifact.yml`
- Modify: `.github/workflows/autofactor-strategy-promotion.yml`
- Test: `tests/workflow_security.rs`

- [x] Document `research_only` as the default FactorEvolve mode.
- [x] Document `legacy_promotion` as an explicit compatibility mode.
- [x] Keep config PR creation only in explicit legacy promotion/config handoff paths.
- [x] Make tests fail if research-only defaults can create handoff issues or config PRs.
- [x] Verify with:

```bash
CARGO_TARGET_DIR=/tmp/ploy-research-os-cutover rtk cargo test --locked --test workflow_security
python3 -m unittest tests.test_factor_walk_forward_sweep tests.test_autofactor_strategy_promotion
```

## Cutover Rule

Research is restored when a `factor-evolve-daily-research.yml run_mode=search` dispatch can produce:

- `factor-walk-forward-v2/report.txt`
- `alpha-search/*/factor-registry-preview.json`
- `alpha-search/*/search-feedback.json`
- `alpha-search-chain/closed-loop-decision.json`
- `autofactor-research-trace.json` when promotion evaluation is explicitly requested

Scheduled research is restored when the daily workflow can resolve a retained
full snapshot artifact automatically and dispatch the hosted walk-forward path.
Runtime replay evidence may be dispatched automatically from the closed-loop
request, but dry-run/config promotion is restored separately when a runtime
replay artifact with `basis=runtime_market_update_replay` is fed into
`autofactor-strategy-promotion.yml` in explicit legacy promotion mode. Automatic
runtime replay feedback may run the promotion evaluator to produce
`autofactor-research-trace.json`; it does not create issues, PRs, or deploy.
