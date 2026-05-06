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

## AutoFactor Strategy Promotion Gate

AutoFactor candidate rows are discovery evidence, not strategy handoff evidence.
After a `factor_walk_forward_v2` report is produced, run the strategy-promotion
evaluator before creating any dry-run handoff:

```bash
python3 scripts/evaluate_autofactor_strategy_promotion.py \
  --report /tmp/factor-walk-forward-v2/report.txt \
  --output-json /tmp/autofactor-strategy-promotion.json \
  --output-md /tmp/autofactor-strategy-promotion.md \
  --output-registry-json /tmp/autofactor-factor-registry.json \
  --output-handoff-json /tmp/autofactor-strategy-handoff.json \
  --output-handoff-md /tmp/autofactor-strategy-handoff.md
```

The evaluator requires all of the following:

- the surrounding PRD promotion gate reports `ready_for_dry_run_handoff=true`;
- the AutoFactor row has `decision=candidate` and `reason=passed`;
- the target is an allowed executable target, defaulting to
  `full_depth_settlement_executable_pnl`;
- the factor has an explicit runtime strategy-profile mapping; and
- the runtime profile matches the requested promotion lane, defaulting to
  `settlement_probability`.

This deliberately blocks cases where a factor is statistically good in the
wrong lane. For example, `spread_adjusted_external_move` can be a valid
repricing candidate while still being blocked from settlement-probability
promotion because its runtime mapping is `repricing_momentum`, not
`settlement_probability`.

The evaluator also emits two machine-readable downstream artifacts when the
corresponding output paths are provided:

- `autofactor-factor-registry.json`: every evaluated factor/target row, its
  AutoFactor status, blockers, runtime mapping, and metrics.
- `autofactor-strategy-handoff.json`: only qualified strategy rows. When no
  row qualifies, this manifest is intentionally `status=blocked` and
  `recommended_action=do_not_promote`.
- `autofactor-strategy-handoff.md`: a dry-run handoff issue/config draft only
  when the handoff is ready. In blocked runs it explicitly says no dry-run
  handoff issue or config should be created.

Factor Walk-Forward V2 also includes a constrained settlement-native generator
for the `full_depth_settlement_executable_pnl` target. It automatically expands
full-depth and conservative settlement edge primitives into `auto_settlement_*`
formula candidates with near-strike, capacity, spread, external-pressure, and
short-IV-change interactions. These rows are still discovery evidence only:
they must pass the same promotion evaluator and runtime mapping gates before
becoming a dry-run handoff.
The built-in promotion evaluator maps the generated `auto_settlement_*`
formula family to the `settlement_probability` strategy profile with
`autofactor_formula:<factor_name>` runtime score identifiers. That mapping only
removes the profile/mapping blocker; it does not override the PRD promotion
gate. Recorded replay parity and the other settlement gates must still be
ready before a handoff issue/config is created.

For an existing Factor Walk-Forward V2 artifact, use the hosted GitHub workflow
instead of waiting for the self-hosted research runner:

```bash
gh workflow run autofactor-strategy-promotion.yml \
  -f git_ref=main \
  -f factor_walk_forward_run_id=<run-id> \
  -f required_strategy_profile=settlement_probability \
  -f allowed_target=full_depth_settlement_executable_pnl
```

This downloads `factor-walk-forward-v2-<run-id>`, runs the same evaluator on
`report.txt`, uploads `autofactor-strategy-promotion-<run-id>` artifacts, and
can optionally comment on a research issue.

The hosted workflow also has a fail-closed `create_handoff_issue` input. Leave
it `false` for diagnostics. When set to `true`, the workflow creates a dry-run
handoff issue only if `autofactor-strategy-handoff.json` reports
`status=ready`; blocked handoffs are logged and skipped.

For the final config-promotion step, the same hosted workflow can create a
reviewable PR instead of requiring a manual TOML edit:

```bash
gh workflow run autofactor-strategy-promotion.yml \
  -f git_ref=main \
  -f factor_walk_forward_run_id=<run-id> \
  -f required_strategy_profile=settlement_probability \
  -f allowed_target=full_depth_settlement_executable_pnl \
  -f create_config_pr=true \
  -f strategy_config=config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml
```

`create_config_pr=true` is intentionally PR-only. It reads the ready handoff
artifact, updates only `three_layer_autofactor_runtime_score` in the target
dry-run config, and opens a normal CI-gated pull request. It does not deploy,
does not change execution/risk settings, and does not enable live trading.
If the handoff is blocked or the config already uses the selected score, no PR
is created.

When the missing piece is a fresh Factor Walk-Forward V2 report and a full
research snapshot artifact already exists, use the GitHub-hosted artifact-only
workflow instead of `ploy-ci-1`:

```bash
gh workflow run factor-walk-forward-v2-hosted-artifact.yml \
  -f git_ref=main \
  -f snapshot_run_id=<snapshot-run-id> \
  -f start_date=2026-04-21 \
  -f end_date=2026-04-25 \
  -f symbols=BTCUSDT,ETHUSDT,SOLUSDT,DOGEUSDT,BNBUSDT,XRPUSDT \
  -f stake_usd=15
```

This workflow runs on `ubuntu-latest`, never reads `PLOY_DB_URL`, and never
compiles a new snapshot. It downloads `research-snapshot-<snapshot-run-id>` by
default, with downstream embedded snapshot fallbacks from
`factor-walk-forward-v2-<snapshot-run-id>` or
`factor-review-v2-<snapshot-run-id>`. It fails before the Rust build if the
artifact only has provenance files and is missing the full payload required by
`load_research_snapshot`: observations, Deribit rows, and Polymarket full-book
rows.

Advanced promotion and issue-handoff controls live in `options_json` to stay
within the GitHub Actions 10-input limit. Defaults are
`required_strategy_profile=settlement_probability`,
`allowed_target=full_depth_settlement_executable_pnl`,
`create_handoff_issue=false`, `create_config_pr=false`, and
`fail_if_blocked=false`.

To make the hosted walk-forward workflow run the final PR-only config promotion
step in the same run, pass `create_config_pr=true` inside `options_json`:

```bash
gh workflow run factor-walk-forward-v2-hosted-artifact.yml \
  -f git_ref=main \
  -f snapshot_run_id=<snapshot-run-id> \
  -f start_date=2026-04-21 \
  -f end_date=2026-04-25 \
  -f symbols=BTCUSDT,ETHUSDT \
  -f stake_usd=15 \
  -f options_json='{"create_config_pr":true,"strategy_config":"config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml"}'
```

This keeps the full chain on GitHub-hosted runners after snapshot creation:
snapshot artifact -> hosted walk-forward -> promotion evaluator -> ready
handoff -> CI-gated config PR. The generated PR updates only
`three_layer_autofactor_runtime_score`; deployment remains a separate explicit
dry-run step from `main`.

For the full settlement-probability PRD gate, prefer passing an existing full
snapshot artifact into the orchestrator so downstream AutoFactor mining and
promotion run on GitHub-hosted runners:

```bash
gh workflow run settlement-probability-prd-gate.yml \
  -f git_ref=main \
  -f snapshot_run_id=<full-snapshot-run-id> \
  -f start_date=2026-05-01 \
  -f end_date=2026-05-05 \
  -f symbols=BTCUSDT,ETHUSDT \
  -f stake_usd=15 \
  -f audit_lookback_hours=168:event-complete \
  -f replay_parity_run_id=<optional-replay-parity-run-id>
```

With `snapshot_run_id` set, the gate skips the legacy `ploy-ci-1` snapshot
build and dispatches `factor-walk-forward-v2-hosted-artifact.yml` on
`ubuntu-latest`. If `snapshot_run_id` is omitted, the orchestrator still falls
back to `research-snapshot.yml`; that path remains a legacy DB-adjacent path
until snapshot export is moved to a hosted-safe data source.
When replay parity evidence uses a non-default artifact name, encode the input
as `replay_parity_run_id=<run-id>:<artifact-name>` so the workflow stays within
GitHub's 10-input dispatch limit.

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

For a full multi-window run from multiple event-root datasets, prefer the
rolling orchestrator:

```bash
rtk cargo run -p ploy-research --example event_ml_rolling_workflow \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-window-1 \
  --dataset /tmp/ploy-event-root-window-2 \
  --dataset /tmp/ploy-event-root-window-3 \
  --output-root /tmp/ploy-event-ml-rolling
```

The orchestrator creates `window_001_event_ml`, `window_002_event_ml`, and so
on under `--output-root`. Each window runs the canonical `event_ml_workflow`;
later windows automatically receive earlier completed run dirs as
`--walk-forward-run-dir` inputs. Duplicate dataset paths are rejected before any
work starts.

When you have one larger event-root dataset and need to create distinct rolling
dataset windows first, use the dataset splitter:

```bash
rtk cargo run -p ploy-research --example event_dataset_rolling_windows \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-large \
  --window-events 150 \
  --output-root /tmp/ploy-event-root-rolling
```

The splitter reads the source `event_index.parquet`, observation split Parquet,
and event summary split Parquet, then writes child event-root datasets such as
`event_root_window_001`, `event_root_window_002`, and so on. Each child dataset
gets fresh chronological train / validation / test assignments inside that
window, updated manifest stats, and the standard event-root artifact set. It
also writes `rolling_datasets.txt`, which can be passed to the rolling workflow
runner:

```bash
rtk cargo run -p ploy-research --example event_ml_rolling_workflow \
  --features polars-export -- \
  --datasets "$(paste -sd, /tmp/ploy-event-root-rolling/rolling_datasets.txt)" \
  --output-root /tmp/ploy-event-ml-rolling
```

The default canonical split policy needs at least 134 events per child window
so validation and test each retain at least 20 events. Final remainders smaller
than that are skipped and recorded in `rolling_datasets_report.json`.

## GitHub Rolling Evidence Workflow

For real remote data, prefer the manual GitHub workflow instead of running
database-backed research on a local machine:

```bash
gh workflow run event-ml-rolling-evidence.yml \
  -f git_ref=main \
  -f start_date=2026-04-24 \
  -f end_date=2026-04-25 \
  -f symbols=BTCUSDT,ETHUSDT,SOLUSDT,DOGEUSDT,BNBUSDT,XRPUSDT \
  -f source_windows=450 \
  -f child_window_events=150 \
  -f run_workflow=true
```

The workflow builds the required Rust examples on a GitHub-hosted Ubuntu
runner first, uploads those binaries as an artifact, then runs the DB-adjacent
evidence phase on `ploy-ci-1`. `ploy-ci-1` should only download and execute
the prebuilt binaries while reading the remote research database at
`172.16.0.204`; do not reintroduce `cargo build` or `cargo run` steps there.
The self-hosted phase performs:

1. `factor_research --export-event-dataset`
2. `event_dataset_rolling_windows`
3. `event_ml_rolling_workflow`

`event_ml_rolling_workflow` also expects the sibling `event_ml_workflow`
binary to be present next to it in the downloaded artifact, so rolling windows
do not spawn nested Cargo builds on `ploy-ci-1`.

It uploads a compact report artifact by default and deliberately avoids
uploading raw Parquet datasets unless `upload_parquet_datasets=true` is passed.
Keep the default `source_windows=450` / `child_window_events=150` shape for the
first full run because it should produce three distinct child datasets while
keeping each child comfortably above the 134-event split-policy floor.

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
