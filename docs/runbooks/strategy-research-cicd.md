# Strategy Research CI/CD Runbook

This runbook defines how strategy ideas move from research issues to GitHub
Actions, evidence, implementation, and deployment decisions.

## Current Workflow Map

| Purpose | Workflow | Default role |
| --- | --- | --- |
| PR validation | `.github/workflows/test.yml` | Required correctness gate for code, contracts, frontend, integration, and research-heavy compile checks |
| Main CI failure tracking | `.github/workflows/test.yml` | Opens or updates a GitHub issue when `main` CI fails |
| PM5D factor diagnostics | `.github/workflows/factor-review-v2.yml` | Remote DB factor review on `ploy-ci-1` with structured JSON artifact |
| Tick-preserving optimization | `.github/workflows/optimize.yml` | Bounded train/validation optimization on synced Tango Parquet data |
| PM5D backtest | `.github/workflows/backtest.yml` | Build and run `run_backtest` on `ploy-ci-1` against remote DB or synced Parquet |
| Replay/dry-run parity | `.github/workflows/replay-dryrun-parity.yml` | Compare replay/backtest artifact evidence against a dry-run JSON report |
| ACK backtest | `.github/workflows/backtest-ack.yml` | Submit Kubernetes backtest job to ACK |
| Image build | `.github/workflows/build-push-acr.yml` | Build runner, collector, and research images for ACK |
| ACK deploy | `.github/workflows/deploy-ack.yml` | Roll selected image tags into ACK deployments |
| Tango deploy | `.github/workflows/deploy-tango-1-1.yml` | Build CI artifacts, ship to `tango-1-1`, restart collectors/runtime, and verify host health |
| Trade deploy | `.github/workflows/deploy-trade.yml` | Build and deploy runner/configs to `ploy-trade-1` |
| Platform release | `.github/workflows/release-platform.yml` | Build platform bundle and optionally deploy it |
| Runtime health | `.github/workflows/healthcheck-tango-1-1.yml` | Scheduled and manual host/data freshness guard |

## Research Lifecycle

1. Create a parent strategy issue for a strategy direction, for example
   `PM5D three-layer factor expansion`.
2. Create child research issues for individual factors, filters, execution
   assumptions, data-source checks, or accounting questions.
3. Run the smallest workflow that can falsify the child issue's hypothesis:
   `factor-review-v2.yml` for factor diagnostics, `optimize.yml` for parameter
   search, and `backtest.yml` for executable replay/backtest checks.
4. Attach evidence back to the issue: workflow URL, git ref, input window,
   symbols, config, artifact name, headline metrics, caveats, and decision.
5. Close rejected ideas with the failure reason. Keep promising ideas open until
   they are linked to an implementation issue or PR.
6. Promote only after the research issue has a concrete decision:
   `continue`, `revise`, `reject`, or `promote to runtime`.
7. Create a separate implementation issue/PR for runtime changes. Do not mix
   exploratory research conclusions and deployable runtime edits in one issue.
8. Deploy only after PR validation passes and the deployment workflow is run
   from `main`.

## Idea Validation Loop

For a new strategy idea, the default loop is:

```text
Idea
  -> define hypothesis and expected edge
  -> design the backtest/replay experiment
  -> run replay/backtest on remote data
  -> inspect accounting, labels, fills, quote age, and rejected opportunities
  -> implement the smallest runtime change needed for dry-run
  -> deploy dry-run from main through GitHub Actions
  -> compare replay vs dry-run on the same market/event/time window
  -> decide: revise idea, fix data/runtime mismatch, keep collecting, or promote
```

The loop is not complete after a profitable backtest. It is complete only when
the dry-run behavior can be reconciled against replay expectations or the
mismatch is understood and turned into the next issue.

Required loop gates:

- **Backtest design gate**: define the hypothesis, event window, data sources,
  execution assumptions, entry/exit rules, stake model, and expected metrics
  before running the workflow.
- **Replay evidence gate**: record the workflow run, git ref, config, dataset
  window, selected events, predicted entries, fills, PnL, quote age, and known
  data gaps.
- **Dry-run gate**: deploy only from `main`, keep risk limits explicit, and
  verify the remote service/config after the workflow completes.
- **Parity gate**: compare replay and dry-run on event id, decision timestamp,
  observed quote, signal inputs, intended side, entry price, fill/not-fill,
  settlement, and PnL.
- **Decision gate**: close the loop with one of `revise`, `fix-data`,
  `fix-runtime`, `collect-more`, `reject`, or `promote`.

Replay/dry-run mismatches are first-class findings. Do not hide them inside a
single research report. Open or link a follow-up issue for the specific mismatch:
data freshness, quote reconstruction, runtime config drift, fill model,
settlement label, signal timing, or execution friction.

## Issue Taxonomy

Use labels consistently:

- `strategy:pm5d` for five-minute PM strategy work.
- `research:hypothesis` for factor or signal hypotheses.
- `research:data-quality` for source freshness, joins, labels, or settlement
  correctness.
- `research:backtest` for replay/backtest accounting and result issues.
- `research:optimize` for parameter search and validation-window issues.
- `research:parity` for replay vs dry-run consistency checks.
- `runtime:strategy` for implementation work that changes strategy behavior.
- `ops:cicd` for workflow, runner, artifact, or deployment pipeline issues.
- `decision:continue`, `decision:revise`, `decision:reject`, and
  `decision:promote` for final research status.

## Evidence Contract

Every research issue should end with an evidence block:

```text
Evidence:
- Workflow:
- Run URL:
- Git ref:
- Dataset/window:
- Symbols:
- Config:
- Artifact:
- Headline metrics:
- Caveats:
- Decision:
```

Backtest evidence must explicitly state whether it is exploratory diagnostics or
executable strategy accounting. For PM5D, one event should not be counted as
multiple deployable trades just because multiple entry-time rows were observed.

## CI/CD Architecture

The intended control loop is:

```text
GitHub issue
  -> workflow_dispatch with explicit git_ref and inputs
  -> GitHub Actions run on ubuntu-latest, ploy-ci-1, tango-1-1, or ACK
  -> structured artifact plus step summary
  -> issue evidence comment and decision label
  -> implementation issue / PR when promoted
  -> PR validation
  -> main merge
  -> deploy workflow from main
  -> remote service/config/health verification
```

Keep the control planes separate:

- Research workflows can run on feature branches when they do not mutate
  deployment state.
- Deployment workflows that affect `tango-1-1`, `ploy-trade-1`, or production
  state must run from `main`.
- Remote data and heavy research belong on `ploy-ci-1`, `tango-1-1`, ACK, or
  CI-built artifacts, not local PostgreSQL assumptions.
- `ploy-ci-1` research workflows read Tango PostgreSQL through GitHub Actions
  secrets `PLOY_RESEARCH_DATABASE_URL` and `PLOY_DB_URL`; verify the private
  endpoint with Aliyun CLI before changing those secrets.
- ACK/ACR image workflows must use immutable checked-out commit SHA tags only.
  Do not push or deploy `latest`.
- Runtime deployment evidence must include remote host verification, not only
  a successful workflow conclusion.
- Replay/dry-run parity is promotion evidence only when the parity artifact says
  `strict_parity_ready=true`. Missing strict fields are a follow-up issue, not a
  pass.

## Recommended Improvements

1. Add label automation for research decisions after evidence is posted.
2. Make the dry-run report expose stricter event-level parity fields when the
   operator API contract is ready.
3. Keep ACK workflows marked as cluster/deployment workflows, separate from the
   current Tango-first PM5D research loop unless ACK becomes the canonical
   research runner.
