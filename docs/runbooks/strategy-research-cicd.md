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
  -> if factor/search evidence is positive, run candidate strategy executable replay
  -> if replay proves the same scorer, event-level decisions, full-depth executable prices, fills, and official settlement, create an implementation issue / PR
  -> PR validation through Platform CI
  -> merge to protected main
  -> deploy dry-run from main through protected Runtime CD
  -> verify remote service, config, persistence, and data freshness
  -> compare recorded dry-run behavior with replayed recorded MarketUpdate evidence
  -> decide: promote, collect-more, revise, fix-data, fix-runtime, fix-workflow, or reject
```

The loop is not allowed to enter dry-run after a factor/search result alone.
The replay gate before dry-run is a strategy-level `executable_replay` artifact,
not an IC/ICIR row and not recorded replay parity. It must use the same scorer
that would be configured at runtime, one event-level decision per market event,
full-depth executable entry/depth/fill assumptions, and official settlement.

The loop is not complete after a profitable replay. It is complete only when
the dry-run behavior is reconciled against recorded replay expectations, or
when the mismatch is understood and tracked as a follow-up issue.

## Workflow Map

| Purpose | Workflow | Default role |
| --- | --- | --- |
| PR validation | `.github/workflows/test.yml` | Required Platform CI gate for code, contracts, frontend, integration, dependency audit, and workflow lint |
| Research snapshot | `.github/workflows/research-snapshot.yml` | Compile reusable research evidence from remote data |
| Factor diagnostics | `.github/workflows/factor-review-v2-hosted-artifact.yml` | GitHub-hosted factor review from a retained full research snapshot artifact |
| Legacy factor diagnostics | `.github/workflows/factor-review-v2.yml` | Fresh DB/snapshot fallback on `ploy-ci-1`; avoid when a full snapshot artifact already exists |
| Walk-forward diagnostics | `.github/workflows/factor-walk-forward-v2.yml` | Rolling factor validation across train/test windows |
| Parameter optimization | `.github/workflows/optimize.yml` | Bounded train/validation optimization from a snapshot or explicit debug data source |
| Replay/backtest accounting | `.github/workflows/backtest.yml` | Build and run replay/backtest accounting in one job on `ploy-ci-1` |
| Candidate strategy replay | `autofactor-strategy-promotion.yml` consumer contract, then a replay-producing workflow/artifact | Required pre-dry-run proof that the selected runtime score is profitable under historical executable replay |
| Replay/dry-run parity | `.github/workflows/replay-dryrun-parity.yml` | Compare replay/backtest evidence against a dry-run JSON report |
| Recorded replay/dry-run parity | `.github/workflows/recorded-replay-parity.yml` | Replay a canonical MarketUpdate recording on `tango-1-1` with the deployed binary, then compare against the matching dry-run report slice |
| Runtime evidence reset | `.github/workflows/reset-strategy-runtime-evidence.yml` | Backup-first cleanup for contaminated dry-run/paper `strategy_runtime_orders` and `strategy_runtime_fills`; follow `docs/runbooks/strategy-runtime-evidence-reset.md` |
| Dry-run config sync | `.github/workflows/sync-dryrun-strategy-config-tango-1-1.yml` | Protected config-only sync for a reviewed dry-run strategy TOML; restarts only the target dry-run worker and avoids collector restarts during clean-window waits |
| Event ML rolling evidence | `.github/workflows/event-ml-rolling-evidence.yml` | Produce event-root rolling ML datasets and compact reports; use `source_dataset_run_id` for the GitHub-hosted artifact path |
| Market data audit | `.github/workflows/market-data-gap-audit.yml` | Scheduled/manual Tango data freshness and gap gate |
| Image build | `.github/workflows/build-push-acr.yml` | Build ACK images; push only immutable checked-out SHA tags |
| ACK deploy | `.github/workflows/deploy-ack.yml` | Deploy immutable SHA image tags through the protected `ack` environment |
| Tango deploy | `.github/workflows/deploy-tango-1-1.yml` | Ship CI-built artifacts to `tango-1-1` and verify host health |
| Trade deploy | `.github/workflows/deploy-trade.yml` | Deploy runner/configs to `ploy-trade-1` through a protected environment |
| Platform release | `.github/workflows/release-platform.yml` | Build platform bundle and optionally deploy it |

`recorded-replay-parity.yml` defaults `since=auto` and `until=auto`. In auto
mode it scans the target recording on `tango-1-1`, intersects that recording
coverage with the dry-run report for the target deployment, prefers the latest
closed dry-run rows when available, and falls back to current open rows when a
fresh recording has not yet accumulated closed events. The workflow records the resolved window in the workflow summary, issue comment, and
`resolved-window.json` artifact. Manual timestamps remain supported for
reproducing a known incident window or an older research issue.
The workflow is read-only evidence generation: it builds `new-ploy-runner` on
the GitHub runner from the requested workflow ref, copies that temporary binary
to the replay scratch directory on `tango-1-1`, and uses a temporary replay
config plus report/database reads. It does not deploy artifacts, restart
services, replace `/opt/ploy/bin/ploy-runner`, or enable live orders. Its
`approval_environment` input therefore defaults to
`tango-1-1-build-only` so parity checks can run without the protected live
deploy approval gate. Use `approval_environment=tango-1-1` only when an operator
explicitly wants the protected environment approval for an incident replay.
The build-only environment must still provide `TANGO_1_1_KNOWN_HOSTS` for
pinned host verification. The SSH key may come from the protected environment
secret or from the repository-level `TANGO_SSH_KEY` / `ALIYUN_ECS_SSH_KEY`
fallbacks used by read-only research workflows.
For the active settlement-probability BTC/ETH dry-run profile, use the matching
recording path
`/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson`;
the older `pm5d-threelayer-canonical.ndjson` recording is historical and can
fail auto-window resolution because it does not overlap current dry-run rows.
If auto-window resolution reports that dry-run rows do not overlap recording
coverage, first check whether the recording has reached its configured
`record_market_updates_max_records` or `record_market_updates_max_bytes` cap.
Use `sync-dryrun-strategy-config-tango-1-1.yml` with `execute=true`,
`force_restart=true`, and `rotate_recording=true` to archive the capped
recording and restart only the target dry-run worker before collecting a fresh
parity window. This keeps the old recording as an archive instead of deleting
raw evidence.

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

1. `decision:continue` or a positive factor/search artifact means run
   candidate strategy executable replay next; it does not mean dry-run.
2. `decision:promote` on the research issue requires a ready candidate
   strategy replay artifact for the exact runtime score, profile, event-level
   decision contract, full-depth executable accounting, and official settlement.
   Only then can it become an implementation issue or PR.
3. Runtime code/config changes must pass Platform CI and merge to `main`.
4. Dry-run deployment must be triggered from `main` through a protected
   environment.
5. Remote verification must prove service health, expected config, persistence,
   data freshness, and no on-host Rust build.
6. Recorded replay/dry-run parity must either report `strict_parity_ready=true`
   or create a follow-up issue explaining the mismatch.
7. Live promotion requires a separate approval with explicit stake, loss, and
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
- Artifact-backed PM5D/PM15D research should default to GitHub-hosted runners.
  Use `factor-review-v2-hosted-artifact.yml`,
  `factor-walk-forward-v2-hosted-artifact.yml`, or
  `settlement-probability-prd-gate.yml` with `snapshot_run_id`, when a retained
  full research snapshot artifact already exists.
- For settlement-probability searches using the `pm5d-execution` data profile,
  Deribit IV/Greeks are not a required promotion surface. Set
  `options_json.require_deribit=true` only for PRD or volatility hypotheses that
  intentionally require `pm5d-vol` / Deribit evidence.
- For one-day PM5D OOS smoke runs, keep the evidence stage as `walk_forward`
  but use hourly windows such as
  `options_json.train_window_hours=12`, `test_window_hours=12`, and
  `step_hours=12` on a clean 24h snapshot. This is an early OOS filter, not a
  replacement for longer promotion-grade rolling evidence.
- Event ML rolling evidence should also default to GitHub-hosted runners after
  the source event-root dataset is artifactized. Pass `source_dataset_run_id`
  to `event-ml-rolling-evidence.yml`; only the fresh DB export branch should
  touch `ploy-ci-1`.
- `ploy-ci-1` is now a legacy DB-adjacent fallback for compiling fresh research
  snapshots from Tango PostgreSQL. Do not route AutoFactor mining,
  walk-forward promotion, or dry-run handoff checks to `ploy-ci-1` when a full
  snapshot artifact can be reused on `ubuntu-latest`.
- Legacy `ploy-ci-1` research workflows read Tango PostgreSQL through GitHub
  Actions secrets `PLOY_RESEARCH_DATABASE_URL` and `PLOY_DB_URL`; verify the
  private endpoint with Aliyun CLI before changing those secrets.
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
