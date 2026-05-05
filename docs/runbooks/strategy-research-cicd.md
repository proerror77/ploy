# Strategy-Agnostic Research and Runtime CI/CD Runbook

This runbook defines the generic path for any strategy family to move from an
idea to research evidence, implementation, dry-run observation, parity review,
and promotion or rejection.

PM5D is one current strategy profile, not the center of the CI/CD model. New
families such as sports, copy-trading, market-making, or event-ML should reuse
the same control loop and add only profile-specific configs, data requirements,
and verification thresholds.

## Four-Layer Model

| Layer | Purpose | Output |
| --- | --- | --- |
| Platform CI | Prove code, contracts, dependencies, workflow syntax, frontend, and integration lanes are healthy | Mergeable PR |
| Research CI | Turn a hypothesis into auditable evidence without mutating runtime state | Evidence artifact, issue comment, decision labels |
| Runtime CD | Deploy only reviewed `main` artifacts to protected environments | Remote health and config verification |
| Promotion Gate | Reconcile replay expectations with dry-run behavior before scaling risk | `promote`, `collect-more`, `revise`, `fix-*`, or `reject` |

## Generic Control Loop

```text
Idea / hypothesis
  -> create a research issue
  -> declare strategy_family, profile, data window, assumptions, and success criteria
  -> run the smallest research workflow that can falsify the hypothesis
  -> attach artifact-backed evidence and labels to the issue
  -> if evidence supports runtime work, create an implementation issue / PR
  -> PR validation through Platform CI
  -> merge to protected main
  -> deploy dry-run from main through protected Runtime CD
  -> verify remote service, config, persistence, and data freshness
  -> compare replay/backtest evidence with dry-run evidence
  -> decide: promote, collect-more, revise, fix-data, fix-runtime, fix-workflow, or reject
```

The loop is not complete after a profitable backtest. It is complete only when
the dry-run behavior is reconciled against replay expectations, or when the
mismatch is understood and tracked as a follow-up issue.

## Workflow Map

| Purpose | Workflow | Default role |
| --- | --- | --- |
| PR validation | `.github/workflows/test.yml` | Required Platform CI gate for code, contracts, frontend, integration, dependency audit, and workflow lint |
| Research snapshot | `.github/workflows/research-snapshot.yml` | Compile reusable research evidence from remote data |
| Factor diagnostics | `.github/workflows/factor-review-v2.yml` | Snapshot-backed factor review on `ploy-ci-1` |
| Walk-forward diagnostics | `.github/workflows/factor-walk-forward-v2.yml` | Rolling factor validation across train/test windows |
| Parameter optimization | `.github/workflows/optimize.yml` | Bounded train/validation optimization from a snapshot or explicit debug data source |
| Replay/backtest accounting | `.github/workflows/backtest.yml` | Build and run replay/backtest accounting in one job on `ploy-ci-1` |
| Replay/dry-run parity | `.github/workflows/replay-dryrun-parity.yml` | Compare replay/backtest evidence against a dry-run JSON report |
| Recorded replay/dry-run parity | `.github/workflows/recorded-replay-parity.yml` | Replay a canonical MarketUpdate recording on `tango-1-1` with the deployed binary, then compare against the matching dry-run report slice |
| Event ML rolling evidence | `.github/workflows/event-ml-rolling-evidence.yml` | Produce event-root rolling ML datasets and compact reports |
| Market data audit | `.github/workflows/market-data-gap-audit.yml` | Scheduled/manual Tango data freshness and gap gate |
| Image build | `.github/workflows/build-push-acr.yml` | Build ACK images; push only immutable checked-out SHA tags |
| ACK deploy | `.github/workflows/deploy-ack.yml` | Deploy immutable SHA image tags through the protected `ack` environment |
| Tango deploy | `.github/workflows/deploy-tango-1-1.yml` | Ship CI-built artifacts to `tango-1-1` and verify host health |
| Trade deploy | `.github/workflows/deploy-trade.yml` | Deploy runner/configs to `ploy-trade-1` through a protected environment |
| Platform release | `.github/workflows/release-platform.yml` | Build platform bundle and optionally deploy it |

## Research Issue Contract

Every research issue must describe one testable claim. Use child issues when a
strategy direction splits into independent factors, filters, data checks, or
execution assumptions.

Required fields:

- `strategy_family`: broad family such as binary-options, sports, copy-trading,
  market-making, or event-ML.
- `strategy_profile`: concrete profile/config under test, or `new` if it does
  not exist yet.
- `hypothesis`: the single claim this issue should prove or falsify.
- `expected edge mechanism`: why the idea should survive spread, fees, stale
  quotes, queue position, settlement, and execution friction.
- `required data`: tables, symbols, windows, labels, settlement fields, and
  freshness checks.
- `workflow plan`: the workflow and exact inputs to run.
- `success and failure criteria`: thresholds for continue, revise, reject, or
  promote.
- `accounting contract`: whether the workflow is exploratory diagnostics or
  executable strategy accounting.
- `parity plan`: how replay/backtest behavior will be compared against dry-run
  behavior.

Evidence block:

```text
Evidence:
- Workflow:
- Run URL:
- Git ref:
- Strategy family/profile:
- Dataset/window:
- Symbols/markets:
- Config:
- Artifact:
- Headline metrics:
- Replay/dry-run parity:
- Caveats:
- Decision:
```

Backtest evidence must explicitly state whether it is exploratory diagnostics or
executable strategy accounting. Event-scoped strategies must not count multiple
diagnostic rows as multiple deployable trades unless the runtime can execute the
same entries under the same risk rules.

## Decision Labels

Research workflows write evidence comments and apply labels so the issue queue
can be filtered without reading every artifact.

Evidence labels:

- `evidence:factor-review`
- `evidence:walk-forward`
- `evidence:optimize`
- `evidence:backtest`
- `evidence:parity`
- `evidence:missing-artifact`
- `evidence:missing-metrics`

Decision labels:

- `decision:pending`
- `decision:continue`
- `decision:collect-more`
- `decision:promote`
- `decision:reject`
- `decision:revise`
- `decision:fix-data`
- `decision:fix-runtime`
- `decision:fix-workflow`

Parity labels:

- `parity:blocked`
- `parity:ready`

## Promotion Rules

Do not promote a strategy from research directly to live. Promotion is staged:

1. `decision:promote` on the research issue means it can become an
   implementation issue or PR.
2. Runtime code/config changes must pass Platform CI and merge to `main`.
3. Dry-run deployment must be triggered from `main` through a protected
   environment.
4. Remote verification must prove service health, expected config, persistence,
   data freshness, and no on-host Rust build.
5. Replay/dry-run parity must either report `strict_parity_ready=true` or create
   a follow-up issue explaining the mismatch.
6. Live promotion requires a separate approval with explicit stake, loss, and
   rollback limits.

## Current Strategy Profiles

Current workflows still include profile defaults for the active binary-options
line. Treat these as defaults, not architecture:

| Family | Profile/config examples | Notes |
| --- | --- | --- |
| binary-options | `02-pm5d-threelayer.*.toml`, `pm5d.threelayer.*.dryrun` | Current most exercised strategy family |
| event-ML | event-root rolling evidence workflows | Research/data pipeline, not a live strategy by itself |

When adding a new family, update configs and workflow inputs so the family can
reuse the same evidence, PR, deploy, and parity loop. Do not fork a separate
CI/CD architecture unless the data plane or runtime target is genuinely
different.

## Control-Plane Rules

- Research workflows can run on feature branches when they do not mutate
  deployment state.
- Keep `workflow_dispatch` inputs at or below GitHub's 10-input limit. Put
  advanced or rarely changed knobs into `options_json`, validate keys in the
  workflow, and fail on unknown options.
- Deployment workflows that affect `tango-1-1`, `ploy-trade-1`, ACK, or
  production state must run from `main`.
- Host deployment workflows must be dispatched from `main` with `git_ref=main`
  before mutating remote state.
- Tango and trade SSH deploys require pinned `known_hosts` secrets
  (`TANGO_1_1_KNOWN_HOSTS` and `PLOY_TRADE_1_KNOWN_HOSTS`). Entries should be
  keyed by the workflow aliases `tango-1-1` and `ploy-trade-1` because the deploy
  SSH config sets `HostKeyAlias`.
- Remote data and heavy research belong on `ploy-ci-1`, `tango-1-1`, ACK, or
  CI-built artifacts, not local PostgreSQL assumptions.
- `ploy-ci-1` research workflows read Tango PostgreSQL through GitHub Actions
  secrets `PLOY_RESEARCH_DATABASE_URL` and `PLOY_DB_URL`; verify the private
  endpoint with Aliyun CLI before changing those secrets.
- DB-mode research workflows must fail closed unless the research database URL
  targets Tango's private VPC endpoint `172.16.0.204`. A public Tango endpoint
  can turn large backtest query results into billable公网出流量.
- ACK/ACR image workflows must use immutable checked-out commit SHA tags only.
  Do not push or deploy `latest`. ACK deployments must also pass through the
  protected `ack` environment before mutating the cluster.
- Runtime deployment evidence must include remote host verification, not only a
  successful workflow conclusion.

## Remaining Improvements

1. Make dry-run reports and replay artifacts expose the same strict event-level
   fields so parity can become a full proof instead of a readiness gate.
2. Move family/profile selection into first-class workflow inputs where the
   implementation currently relies on profile-specific defaults.
3. Add automated metric parsers for factor, walk-forward, and optimize evidence
   so workflows can label `decision:collect-more`, `decision:reject`, or
   `decision:promote` without manual review.
