# Seven-Layer Trading Agent Framework

## Purpose

This note archives the user's "seven-layer agent hierarchy" for trading, then
maps it onto the current `ploy` repo so future research can distinguish:

- what is already present in code or workflow;
- what only exists as a partial/local heuristic; and
- what is still missing as a first-class research lane.

This document does not change runtime behavior. It is a research and
architecture reference.

## Source Theory

From the user's image, the seven layers are:

1. `Lvl I` Market Structure And Cycle
   - market trend
   - cyclical transitions
2. `Lvl II` Extreme Risk And Fragility
   - tail risk
   - crash warning
3. `Lvl III` Price-Volume Dynamics
   - liquidity
   - order imbalance
   - price-volume coordination
4. `Lvl IV` Price-Volatility Behavior
   - trend persistence
   - reversal
   - volatility clustering
   - asymmetry
5. `Lvl V` Multi-Scale Complexity
   - retracement
   - fractal features
   - long memory
6. `Lvl VI` Stability And Regime Gating
   - stability evaluation
   - adaptive gating
7. `Lvl VII` Geometry And Fusion
   - K-line morphology
   - factor fusion
   - herd effects

## Executive Assessment

The current repo does not implement this seven-layer theory as one explicit
strategy architecture.

What exists today is closer to:

- a `ThreeLayer` runtime decision pipeline for PM5D execution;
- a regime-aware factor research layer;
- workflow/promotion gates that block premature dry-run or live handoff.

So the correct answer is:

- the theory is **not absent** from the trading stack;
- but it is **not yet implemented as a complete seven-layer framework**;
- several layers exist only as scattered factors, gates, or diagnostics.

## Current Mapping

| Layer | Status | Current repo mapping | Main gap |
| --- | --- | --- | --- |
| L1 Market structure and cycle | Partial | time-remaining `Regime`, directional/trend features, strategy profiles | lacks a first-class market-structure ontology and cycle-state model |
| L2 Extreme risk and fragility | Weak partial | generic runtime/risk/promotion gates | lacks explicit fragility, jump, liquidity-withdrawal, and crash-warning research surfaces |
| L3 Price-volume dynamics | Applied partial | LOB imbalance, depth, spread, microprice, trade imbalance, confirmation layer | present locally, but not formalized as a named theory layer |
| L4 Price-volatility behavior | Partial | `sigma_horizon`, `vol_gap`, volatility-aware scoring | lacks a unified persistence/reversal/asymmetry research module |
| L5 Multi-scale complexity | Weak partial | multiple lookbacks and rolling windows | lacks explicit fractal, long-memory, and path-complexity factor families |
| L6 Stability and regime gating | Strong partial | `Regime`, ICIR stability, fail-closed workflow gates, handoff blockers | needs layer-aware stability reporting instead of only global gating |
| L7 Geometry and fusion | Partial | event geometry in search space, AutoFactor/runtime-score fusion | lacks first-class K-line morphology and herd-behavior feature families |

## Evidence In Current Repo

### L1 Market Structure And Cycle

Current evidence:

- `crates/ploy-operator-contracts/src/trading.rs`
  - canonical `Regime` enum with `Early`, `Middle`, `Late`, `Expiry`
- `crates/ploy-strategy-bundles/src/strategies/three_layer.rs`
  - runtime branches behavior by regime/time-remaining window
- `crates/ploy-research/examples/factor_research.rs`
  - regime-specific direction and gating logic

Assessment:

This is a useful state segmentation layer, but it is narrower than the user's
"market structure and cycle" concept. It mostly captures event time regime, not
full structural state transitions across trend, rotation, compression, or
expansion.

### L2 Extreme Risk And Fragility

Current evidence:

- `docs/PROJECT_SEMANTICS.md`
  - promotion gates, blocked states, and fail-closed research semantics
- strategy/runtime code has normal execution/risk filters

Assessment:

The repo has safety gates, but not a dedicated fragility model. There is no
first-class factor family for tail-risk buildup, crash precursors, abrupt
liquidity withdrawal, or market-structure brittleness.

### L3 Price-Volume Dynamics

Current evidence:

- `crates/ploy-research/src/factors_new/scan.rs`
  - factors include `obi_10`, `depth_imbalance`, `microprice_offset_bps`,
    `spread_bps`, `cum_trade_imbalance_5m`, and `cum_mprice_drift_5m`
- `crates/ploy-strategy-bundles/src/strategies/three_layer.rs`
  - the current runtime tracks drift, liquidity, and book confirmation inputs
- `crates/ploy-research/examples/factor_research.rs`
  - Layer 2 confirmation is explicitly `drift_30s + obi_10 + depth_imbalance
    + cum_mprice_drift_5m`

Assessment:

This is the most obviously implemented part of the seven-layer theory. It is
already active in research factors and in the PM5D runtime confirmation path.

### L4 Price-Volatility Behavior

Current evidence:

- `crates/ploy-research/src/factors_new/scan.rs`
  - factors include `sigma_horizon` and `vol_gap`
- `crates/ploy-strategy-bundles/src/strategies/three_layer.rs`
  - runtime computes drift and short-horizon volatility state
- `crates/ploy-strategy-bundles/src/strategies/directional_bayes.rs`
  - local volatility state machinery exists
- `crates/ploy-strategy-bundles/src/strategies/mean_reversion.rs`
  - local volatility state machinery exists

Assessment:

Volatility is already part of decision-making, but the implementation is
feature-local. The repo does not yet expose one unified volatility-behavior
research layer covering persistence, reversal probability, clustering, and
asymmetry as one explicit framework.

### L5 Multi-Scale Complexity

Current evidence:

- `crates/ploy-research/src/factors_new/scan.rs`
  - multiple time-window features exist, such as `drift_10s`,
    `drift_30s`, `cex_bar_return_30s`, and `cex_bar_return_60s`
- rolling operators are allowed in the alpha-search/search-space contract

Assessment:

The repo has multi-window statistics, but not a real "complexity" layer yet.
There is no explicit fractal geometry, Hurst/long-memory, retracement-state, or
path-complexity package in the canonical research flow.

### L6 Stability And Regime Gating

Current evidence:

- `crates/ploy-research/src/factors_new/registry.rs`
  - factor metadata records `stability`
- `crates/ploy-research/src/factors_new/scan.rs`
  - stability is computed from ICIR-like bucket behavior
- `docs/PROJECT_SEMANTICS.md`
  - dry-run/live promotion is blocked until named gates are passed
- `docs/runbooks/event-ml-automl-workflow.md`
  - workflow is ordered and fail-closed; it stops on failed readiness phases

Assessment:

This is the strongest alignment with the user's theory. The repo already thinks
in terms of stability, readiness, and gated progression. The missing step is to
make those gates explicitly seven-layer-aware instead of treating all upstream
signal structure as one undifferentiated factor pool.

### L7 Geometry And Fusion

Current evidence:

- `docs/ALPHA_FACTOR_SEARCH_CICD.md`
  - feature pool explicitly includes `event geometry`
- the same search-space contract supports bounded factor fusion
- `docs/runbooks/event-ml-automl-workflow.md`
  - AutoFactor and runtime handoff already support factor fusion into runtime
    scores

Assessment:

Geometry and fusion are present, but only partially. The repo already supports
event geometry and formula fusion, yet it does not have first-class K-line
morphology, herd-state modeling, or a canonical geometry-family registry.

## Relationship To The Existing Three-Layer Runtime

The current PM5D runtime is named `ThreeLayer`, but that is not the same thing
as the user's seven-layer framework.

Current `ThreeLayer` is a compressed execution decision stack:

1. direction / state estimate
2. microstructure confirmation
3. edge and reward-risk filter

The seven-layer theory is broader. It is a research ontology spanning
macro-to-micro structure, fragility, volatility behavior, multi-scale
complexity, adaptive gating, and final factor fusion.

The practical interpretation is:

- today's `ThreeLayer` runtime can be one downstream consumer of the theory;
- the seven-layer framework should live primarily in research, feature
  taxonomy, and promotion logic first;
- only after evidence is strong should it be compressed into runtime heuristics
  or model contracts.

## Recommended Research Order

Follow the repo's existing semantics and workflow order. Do not jump straight
from this theory to hyperparameter search or live dry-run.

### Phase 1 - Layer Taxonomy

Create a canonical factor taxonomy that assigns each current factor to one or
more of the seven layers.

Output:

- `layer -> factors -> current source file -> status`

Target surfaces:

- `crates/ploy-research/src/factors_new/scan.rs`
- `docs/ALPHA_FACTOR_SEARCH_CICD.md`
- `docs/runbooks/event-ml-automl-workflow.md`

### Phase 2 - Coverage Audit

For each layer, identify:

- already measured factors
- runtime-consumable factors
- missing feature families
- blocked data dependencies

Priority missing families today:

- L2 fragility
- L5 complexity
- L7 morphology and herd effects

### Phase 3 - Factor Attribution By Layer

Run factor-attribution work by `layer x regime x label`, not only as one flat
registry.

This should stay inside the existing evidence stages:

- `diagnostic`
- `factor_attribution`
- `walk_forward`
- `runtime_parity`
- `dry_run_candidate`

Do not promote a layer because it "sounds right." Promote it only if the repo's
semantic gates pass.

### Phase 4 - Controlled Fusion

Once per-layer factor evidence is visible, allow bounded fusion in the existing
AutoFactor / alpha-search path.

Preferred rule:

- fuse only audited layers;
- keep typed mutation boundaries;
- report which layers each candidate uses;
- penalize crowded fusion trees that do not improve stability or capacity.

### Phase 5 - Runtime Compression

Only after walk-forward and parity evidence exists should the theory be
compressed into runtime logic, for example:

- per-layer feature gates in `ThreeLayer`
- layer-aware runtime scores
- stability-aware stake reduction
- fragility-aware kill switches

## Immediate Next R&D Slice

The best next slice is not "rewrite the strategy."

The best next slice is:

1. define a seven-layer factor taxonomy;
2. label the current factor pool against that taxonomy;
3. expose layer coverage and per-layer attribution in research artifacts;
4. only then decide which missing layers deserve new feature engineering.

This keeps the work aligned with `docs/PROJECT_SEMANTICS.md` and the canonical
`event_ml_workflow` / AutoFactor promotion path.
