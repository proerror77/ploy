# Live Deploy And Research Boundary Design

## Goal

Make the current workspace branch operable in two explicit modes:

- `live/operator`: `ployd` + `ployctl` + `ploytui` + deployment manifests
- `research/backtest`: offline dataset prep and replay paths

The branch already has the new platform runtime. What is still missing is a
clear operator contract for first live deployment and a cleaner repo narrative
that prevents operators from mixing archived single-binary commands with the
current daemon path.

## Scope

This cut does not add new trading behavior. It does three narrower things:

1. add a canonical minimal live deployment checklist
2. add a checked-in live deployment manifest template
3. make paper/live and research/live boundaries more visible in docs and
   operator surfaces

## Decisions

### 1. Live deploy path stays single-host and API-first

The default runtime remains:

- `ployd` as the only long-running daemon
- `ployctl` for operator actions
- `ploytui` for terminal observation
- CI-built artifacts via `release-platform.yml`

We will not reintroduce old `ploy platform start` guidance.

### 2. Deployment manifests become the only operator-facing runtime configs

`config/deployments/` will carry both:

- `example.paper.json`
- `example.live.json`

The install/deploy path will also provision `/opt/ploy/config/deployments/`
so the host-side runbook matches the actual bundle layout.

### 3. Runtime mode must be visible in deployment summaries

Operators should not infer paper vs live from the deployment id. The
deployment summary contract will include `runtime_mode`, and CLI/TUI output
will render it directly.

### 4. Research/backtest stays out of `ployd`

Backtesting and dataset preparation remain offline paths:

- `ploy collect`
- `ploy orderbook-history`
- `ploy deribit-iv-backfill`
- `ploy strategy backfill-*`
- `crates/ploy-research`

`ployd` is for paper/live deployment runtime only.

## Planned Changes

### Docs and runbooks

- Add `docs/runbooks/live-deployment-checklist.md`
- Add `docs/runbooks/research-backtest-routing.md`
- Rewrite `README.md` so the top-level usage only presents:
  - current platform path
  - research/backtest routing
  - archived compatibility references by link only
- Update `docs/runbooks/platform-startup.md`
- Update `docs/runbooks/platform-deploy.md`
- Clarify config boundaries in:
  - `config/deployments/README.md`
  - `config/platform/README.md`
  - `config/strategies/README.md`

### Operator contract

- Add `runtime_mode` to `DeploymentSummary`
- Render `runtime_mode` in `ployctl deployments ...`
- Render `runtime_mode` in `ploytui` deployment rows

### Release/deploy path

- Bundle deployment examples in `.github/workflows/release-platform.yml`
- Create `/opt/ploy/config/deployments/` in
  `scripts/install-platform-service.sh`
- Install `example.paper.json` and `example.live.json` onto the host
- Add systemd guardrail verification to the deploy runbook and workflow guard

## Verification

- `rtk cargo test -p ploy-operator-contracts -p ploy-platform -p ployctl -p ploytui`
- `rtk cargo test --test platform_release_workflow`
- `rtk cargo test --test workflow_security`
- `rtk cargo test --test platform_smoke`
- doc consistency review across README and both runbooks
