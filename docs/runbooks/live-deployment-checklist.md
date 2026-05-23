# Live Deployment Checklist

## Goal

Validate a remote `ployd` host before enabling real live trading.

This checklist is the operator-facing go/no-go gate. It assumes:

- `ployd.service` is already installed on the host
- validation happens remotely through `/opt/ploy/bin/ployctl`
- the default acceptance path is a dry-run drill using a paper deployment

## Before You Start

- Confirm you are on the workspace-default release path:
  - `.github/workflows/release-platform.yml`
  - `deployment/ployd.service`
  - `scripts/install-platform-service.sh`
  - `deployment/ploy-maintenance.timer`
  - `deployment/ploy-platform-watchdog.timer`
- Confirm the host has:
  - `/opt/ploy/bin/ployd`
  - `/opt/ploy/bin/ployctl`
  - `/opt/ploy/.env`
- Confirm `ployd` is the only long-running platform process.
- Confirm host-support timers are installed:
  - `ploy-platform-watchdog.timer`
  - `ploy-maintenance.timer`

## Required Host Inputs

At minimum, `/opt/ploy/.env` should define:

- `PLOY_ADMIN_TOKEN`
- `POLYMARKET_PRIVATE_KEY`

Recommended:

- `PLOY_OPERATOR_TOKEN` for remote `ployctl` mutation without full admin access
- `PLOY_API_AUTH_COOKIE_SECRET` if browser sessions are used
- `POLY_SIGNATURE_TYPE`
- `POLY_FUNDER` when `POLY_SIGNATURE_TYPE=proxy` or `POLY_SIGNATURE_TYPE=gnosis_safe`

## Baseline Service Checks

Run on the remote host:

```bash
systemctl is-active ployd
curl -fsS http://127.0.0.1:8081/health
/opt/ploy/bin/ployctl system status
/opt/ploy/bin/ployctl system metrics
/opt/ploy/bin/ployctl system alerts
/opt/ploy/bin/ployctl system audit
```

Go only if:

- `ployd` is active
- `/health` returns success
- `ployctl system status` is `running`, `recovering`, or `degraded`
- no critical alerts are active

Stop if:

- `/health` fails
- `system status` is unexpected
- `system alerts` contains critical entries

## Dry-Run Acceptance Path

The default host acceptance path is:

1. Run the dry-run drill script:

```bash
/opt/ploy/scripts/drills/live_dry_run.sh
```

2. Confirm the script reports `PASS` or an understood `WARN`.
3. Review any warnings before considering real live enablement.

The drill uses a paper deployment and does not touch real funds.

## Manual Review Before Real Live Enablement

Only consider real live enablement after the dry-run drill passes and you have
manually reviewed:

- `ployctl system status`
- `ployctl system metrics`
- `ployctl system alerts`
- `ployctl trading status`
- the most recent audit log entries

## PM5D ThreeLayer Live Gate

For the current PM5D ThreeLayer setup, stage the live deployment in paused mode:

```bash
/opt/ploy/scripts/drills/pm5d_threelayer_live_gate.sh
```

This verifies `02-pm5d-threelayer.live.toml` against the dry-run config, applies
`pm5d.threelayer.live` with `desired_state=paused`, and stops before any live
orders can be placed.

Only after explicit operator approval, run:

```bash
/opt/ploy/scripts/drills/pm5d_threelayer_live_gate.sh --go-live
```

## Not Covered By This Checklist

This checklist does not:

- place live orders
- cancel live venue orders
- trigger real redeem / claim flows
- replace a separate game-day rehearsal for production funds
