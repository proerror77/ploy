# Event ML AutoML Workflow

This is the canonical research workflow for PM 5-minute event datasets before
moving a signal toward DL, RL, backtest optimization, or live dry-run.

The main rule is simple: optimize the research process before optimizing model
parameters. AutoML-style factor attribution comes before hyperparameter search.

## Scope

Use this workflow when the input is an event-root dataset produced by
`factor_research` / `DatasetBuildManifest`, with Parquet observation splits:

- `event_manifest.json`
- `observations_train.parquet`
- `observations_val.parquet`
- `observations_test.parquet`

The workflow assumes binary option accounting:

- one 5-minute event is one decision unit
- train / validation / test events must not overlap
- each model trial uses one or a small fixed set of predeclared entry rows per
  event
- settlement, post-event rows, and future quote state are never allowed as
  features

## Foundation Architecture Artifact

Before adding a new model family, DL experiment, RL environment, or dry-run
handoff, generate the foundation artifact:

```bash
rtk cargo run -p ploy-research --example event_ml_architecture -- \
  --output-dir /tmp/ploy-event-ml-architecture
```

This writes:

- `event_ml_architecture.json`
- `event_ml_architecture.md`
- `event_ml_gate_matrix.json`

The artifact is the shared contract for agents and reviewers. It defines:

- canonical workflow phases
- supervised, DL, RL, and dry-run learning lanes
- required artifacts between phases
- readiness gates for each lane
- stop rules that prevent premature DL/RL work

The architecture layer is intentionally pure Rust and has no Polars, database,
or training-framework requirement. It is the foundation contract; the workflow
runner below is the executable data path.

## Phase 0 - Dataset Contract

Goal: prove the dataset can be trusted as an ML input.

Inputs:

- event-root dataset directory
- manifest generated with the dataset
- observation Parquet files for train, validation, and test

Required checks:

- manifest contract validates
- split event IDs are disjoint
- labels are binary and settled
- entry prices are valid `0 < ask < 1`
- selected rows are before settlement
- selected row count is reported by split

Stop gate:

- Do not train if train / val / test events overlap.
- Do not train if feature normalization would need validation or test data.
- Do not treat tiny selected-event counts as model-quality evidence.

## Workflow Runner

Use this runner when you want the agent or CI-style local execution to follow
the first executable phases in order:

```bash
rtk cargo run -p ploy-research --example event_ml_workflow \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-5sym-150-20260424 \
  --entry-secs 60 \
  --tolerance-secs 30
```

The runner executes:

1. `event_dataset_coverage`
2. `event_factor_attribution`
3. `event_dataset_baseline`
4. bounded logistic hyperparameter search using `event_dataset_baseline`
5. `event_ml_walk_forward`

It stops on the first failed phase. The attribution phase writes
`factor_attributions.json`, `feature_whitelist.txt`, and
`feature_whitelist.md`; if the user did not pass `--features`, the baseline
phase consumes that whitelist automatically. The runner also writes
`workflow_report.json` and `workflow_report.md` into the run directory.
The hyperparameter phase writes candidate-level `baseline_metrics.json` files
plus `hyperparameter_search.json` and `hyperparameter_search.md`.
The walk-forward phase writes `walk_forward_report.json` and
`walk_forward_report.md`. With only one workflow run, the report is expected to
mark DL/RL readiness as `blocked`; that is a useful gate, not a runner failure.

Use `--output-dir <dir>` to choose the artifact directory. Without it, the
runner writes under `<dataset>/workflow_runs/event_ml_<timestamp>`.

Use `--dry-run` to print the commands without running them, or
`--phases coverage,attribution,baseline,hyperparameter,walk-forward` to run a
subset in canonical order.

When you already have completed workflow runs from prior rolling event-root
datasets, pass them into the current run so the final gate evaluates multiple
windows:

```bash
rtk cargo run -p ploy-research --example event_ml_workflow \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-window-3 \
  --output-dir /tmp/ploy-event-ml-window-3 \
  --walk-forward-run-dir /tmp/ploy-event-ml-window-1 \
  --walk-forward-run-dir /tmp/ploy-event-ml-window-2
```

The current run is always included as one window. The extra run directories
must point at distinct completed workflow runs; repeated run dirs or repeated
dataset windows are treated as blocked evidence, not rolling validation.

## Phase 1 - Coverage Diagnostics

Goal: decide whether the dataset is ready for ML, or whether feature coverage
must be fixed first.

Command:

```bash
rtk cargo run -p ploy-research --example event_dataset_coverage \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-5sym-150-20260424 \
  --entry-secs 60 \
  --tolerances 30
```

Outputs to record:

- train / val / test event counts
- label coverage
- valid price coverage
- finite selected-feature coverage
- per-feature missingness
- selected row coverage at each entry window

Pass criteria:

- enough train events for a fixed baseline
- validation and test have nonzero tradable coverage
- missingness is explained at feature level, not hidden by implicit imputation

Stop gate:

- If row-complete coverage is low, first fix feature export or intentionally
  narrow the feature set. Do not compensate with hyperparameter search.

## Phase 2 - AutoML-Style Factor Attribution

Goal: rank candidate factors and register stable attribution metadata before
model tuning.

Command:

```bash
rtk cargo run -p ploy-research --example event_factor_attribution \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-5sym-150-20260424 \
  --entry-secs 60 \
  --tolerance-secs 30 \
  --top-n 20
```

What this phase does:

- selects one tradable observation per event near the configured entry time
- computes train / validation / test AUC lift per feature
- computes correlation diagnostics
- ranks by validation AUC lift magnitude
- preserves train-derived direction for registry metadata
- registers factors as `automl:<feature>`

Outputs to record:

- top factors by validation AUC lift
- train / validation / test direction agreement
- stability score
- factor direction
- sample counts
- features rejected for missingness or unstable direction

Pass criteria:

- validation signal is not isolated from train direction
- test direction is not obviously contradictory
- factor has enough coverage at the chosen entry window
- factor is understandable enough to monitor in production

Stop gate:

- AutoML attribution is not a live-edge claim.
- Do not promote a factor only because validation AUC is high.
- Do not use validation or test direction to define registry direction.

## Phase 3 - Feature Set Governance

Goal: freeze a candidate feature schema before model comparison.

Process:

1. Start from the attributed factor list.
2. Keep factors with stable direction and adequate event coverage.
3. Remove near-duplicate factors unless both have a clear reason to stay.
4. Keep negative-direction factors if the direction is stable.
5. Write the feature list into the experiment record.
6. Use train-only normalization parameters.

Artifacts:

- feature whitelist
- rejected-feature list with reasons
- entry-window selection
- normalization metadata

The current executable artifact path is:

- `feature_whitelist.txt`: newline-delimited baseline feature schema
- `feature_whitelist.md`: reviewable feature governance summary
- `factor_attributions.json`: full train/validation/test attribution table

Stop gate:

- Do not let each model trial choose a different feature set unless the feature
  selection step itself is explicitly part of the trial and counted as a test.

## Phase 4 - Fixed Baseline

Goal: establish a simple baseline before model selection or tuning.

Command:

```bash
rtk cargo run -p ploy-research --example event_dataset_baseline \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-5sym-150-20260424 \
  --entry-secs 60 \
  --tolerance-secs 30
```

Default baseline:

- logistic model
- fixed hyperparameters
- train-only normalization
- event-held-out validation and test
- one-event-one-trade simple PnL diagnostic

Outputs to record:

- accuracy
- logloss
- Brier score
- AUC
- simple binary-option PnL
- ROI
- average entry price
- selected event counts

Pass criteria:

- fixed baseline is not broken
- validation and test are not directionally absurd
- PnL accounting uses executable entry prices, not only label accuracy

Stop gate:

- If the fixed baseline fails, do not move to DL, RL, or hyperparameter search.

## Phase 5 - Model Family Selection

Goal: compare model families only after factor governance and fixed baseline are
working.

Recommended order:

1. regularized logistic / linear model
2. small tree or shallow ensemble
3. small neural network
4. sequence model only after multi-row event state is stable
5. RL only after the execution environment and reward accounting are faithful

Rules:

- model families use the same feature schema unless the comparison explicitly
  tracks feature-selection degrees of freedom
- validation chooses the model family
- test is reserved for final confirmation
- every rejected model family counts as a hypothesis test

Stop gate:

- Do not choose the most complex model if it only wins in validation and fails
  basic stability checks.

## Phase 6 - Hyperparameter Search

Goal: tune a selected model family without converting validation into a
backtest-overfit machine.

Use hyperparameter search only after:

- coverage is acceptable
- AutoML attribution produced a governed feature set
- a fixed baseline has passed
- the model family is chosen

Recommended constraints:

- keep parameter count small
- use bounded grids before broad randomized search
- record every search space and rejected trial
- never tune on test
- prefer walk-forward validation once enough days are available

Examples of acceptable search spaces:

- logistic regularization strength
- tree depth / number of leaves
- calibration threshold
- entry threshold
- small ensemble weights

The current executable runner supports a bounded logistic search:

```bash
rtk cargo run -p ploy-research --example event_ml_workflow \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-5sym-150-20260424 \
  --entry-secs 60 \
  --tolerance-secs 30 \
  --search-l2 0,0.001,0.01 \
  --search-min-edge 0,0.02,0.05 \
  --search-learning-rate 0.03,0.05
```

Selection rule:

- maximize validation PnL
- break ties with lower validation logloss
- record test metrics, but never use test metrics for selection

Artifacts:

- `hyperparameter/candidate_*/baseline_metrics.json`
- `hyperparameter/hyperparameter_search.json`
- `hyperparameter/hyperparameter_search.md`

Examples to avoid early:

- large neural-network architecture search
- RL reward shaping search
- search spaces with many interacting timing and sizing parameters
- optimizing directly for one tiny test slice

Stop gate:

- If validation improvement does not survive test, rolling windows, or realistic
  PnL accounting, discard the tuned parameter set.

## Phase 7 - Walk-Forward And Backtest

Goal: prove the result is not a single split artifact.

Executable gate command for one or more completed workflow run directories:

```bash
rtk cargo run -p ploy-research --example event_ml_walk_forward -- \
  --run-dir /tmp/ploy-event-ml-run-1 \
  --run-dir /tmp/ploy-event-ml-run-2 \
  --run-dir /tmp/ploy-event-ml-run-3 \
  --output-dir /tmp/ploy-event-ml-walk-forward
```

The gate consumes:

- `workflow_report.json`
- `hyperparameter/hyperparameter_search.json`
- the selected candidate's `baseline_metrics.json`

The gate writes:

- `walk_forward_report.json`
- `walk_forward_report.md`

Default readiness gates:

- at least `3` workflow windows
- at least `3` distinct event-root dataset windows
- each window has at least one test trade
- executable entry accounting is present: cost, ROI, and average entry
- window-level drawdown is reported
- validation/test direction agreement is reported

The workflow runner calls this gate automatically for its current run. A
single-run report should remain `blocked` for DL/RL because it lacks rolling
window evidence.

Required once enough history exists:

- rolling train / validation / test windows
- per-window factor attribution
- per-window selected features
- per-window tuned params, if any
- test PnL distribution by window
- trade count / event count by window

Backtest requirements:

- one-event-one-trade accounting for 5-minute binaries unless a different
  entry policy is explicitly declared
- executable entry quote
- fees / slippage / latency assumptions called out
- payout and average entry price reported
- drawdown and bankroll framing reported

Stop gate:

- Do not advance a strategy on win rate alone.
- Do not call a result production-ready without price, payout, and drawdown
  evidence.

## Phase 8 - DL Gate

DL starts only after the tabular workflow is stable.

Minimum requirements:

- enough events across multiple days
- stable feature export and labels
- sequence/state representation defined before training
- fixed baseline and shallow model baselines recorded
- train-only normalization and no future rows
- OOS test has enough events to matter

DL should answer a specific question, such as:

- does sequence state beat one-row entry features?
- does nonlinear interaction beat the governed factor set?
- does calibration improve executable PnL after costs?

Stop gate:

- Do not use DL because the current dataset is small and noisy.

## Phase 9 - RL Gate

RL is last because it requires an executable environment, not only a label.

Minimum requirements:

- state includes only information available at decision time
- action space is explicit: no trade, buy up, buy down, optional exit
- reward matches binary payout and entry price
- quote availability and latency are modeled
- position sizing and bankroll accounting are modeled
- environment replay produces the same accounting as the backtest

RL should answer a specific question, such as:

- does dynamic entry timing beat fixed entry timing?
- does exit policy improve EV after costs?
- does sizing improve drawdown-adjusted return?

Stop gate:

- Do not start RL while the supervised baseline cannot produce trustworthy
  executable-price accounting.

## Phase 10 - Strategy Handoff

Goal: convert research into a dry-run candidate without changing live behavior
by accident.

Required handoff packet:

- dataset manifest
- feature whitelist
- factor attribution report
- baseline report
- model family decision
- hyperparameter search record, if used
- walk-forward report, if available
- backtest report with executable-price accounting
- rejected alternatives and known risks

Dry-run requirements:

- signal metadata includes model version, feature schema, entry window, and
  factor set ID
- quote availability is monitored
- fill feasibility is monitored
- latency is measured
- no hard live-trade promotion without a separate deployment checklist

## Canonical Order

```text
event-root dataset
  -> coverage diagnostics
  -> AutoML-style factor attribution
  -> governed feature set
  -> fixed baseline
  -> model family selection
  -> hyperparameter search
  -> walk-forward + executable-price backtest
  -> DL gate, if justified
  -> RL gate, if justified
  -> dry-run strategy handoff
```

In short: AutoML finds and governs factors. Hyperparameter search tunes a
chosen model family. DL and RL wait until data, accounting, and execution
semantics are trustworthy.
