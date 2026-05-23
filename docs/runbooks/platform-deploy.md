# Platform Deploy Runbook

## Default Release Path

The workspace-default deploy path is:

- GitHub Actions workflow: `.github/workflows/release-platform.yml`
- Long-running systemd unit: `deployment/ployd.service`
- Host install helper: `scripts/install-platform-service.sh`

Use `deploy=false` when dispatching the workflow if you only want CI to build,
package, checksum, and upload the platform bundle artifact without touching the
remote host. The default `deploy=true` preserves the normal release path.

This path deploys three binaries:

- `/opt/ploy/bin/ployd`
- `/opt/ploy/bin/ployctl`
- `/opt/ploy/bin/ploytui`

`ployd` is the only default long-running process. `ployctl` is an operator
client used for post-deploy verification and remote inspection. `ploytui` is
the thin terminal console built on the same daemon HTTP surface.

Deployment resources are managed separately from the daemon process. Operators
apply a deployment manifest and then use `ployctl` to inspect or change desired
state.

This runbook covers deploy/install only. Remote host acceptance now has its own
path:

- [`live-deployment-checklist.md`](./live-deployment-checklist.md)
- [`live-dry-run-drill.md`](./live-dry-run-drill.md)

## CI Bundle Contents

The release bundle contains:

- `bin/ployd`
- `bin/ployctl`
- `bin/ploytui`
- `deployment/ployd.service`
- `deployment/ploy-maintenance.service`
- `deployment/ploy-maintenance.timer`
- `deployment/ploy-platform-watchdog.service`
- `deployment/ploy-platform-watchdog.timer`
- `scripts/install-platform-service.sh`
- `scripts/ploy_maintenance.sh`
- `scripts/ploy_platform_watchdog.sh`
- `data/state/deployments.json.sample`

## Host Installation Flow

After the bundle lands on the host, the deploy workflow:

1. Installs `ployd`, `ployctl`, and `ploytui` into `/opt/ploy/bin`
2. Installs `deployment/ployd.service` and host-support maintenance/watchdog
   unit files into `/opt/ploy/deployment`
3. Installs `scripts/install-platform-service.sh`,
   `scripts/ploy_maintenance.sh`, and `scripts/ploy_platform_watchdog.sh` into
   `/opt/ploy/scripts`
4. Seeds `/opt/ploy/data/state/deployments.json` if missing
5. Runs `install-platform-service.sh`, which installs `ployd.service` and the
   maintenance/watchdog timers into systemd
6. Restarts `ployd`
7. Verifies:
   - `systemctl status ployd`
   - `systemctl status ploy-platform-watchdog.timer`
   - `systemctl status ploy-maintenance.timer`
   - `curl -fsS http://127.0.0.1:8081/health`
   - `/opt/ploy/bin/ployctl system status`
   - `/opt/ploy/bin/ployctl system metrics`
   - `/opt/ploy/bin/ployctl system alerts`
   - `/opt/ploy/bin/ployctl system audit`
   - `/opt/ploy/bin/ployctl trading status`
   - `/opt/ploy/bin/ploytui`
   - `curl -N http://127.0.0.1:8081/api/events/stream`

## Required Host Paths

The install script ensures:

- `/opt/ploy/.env`
- `/opt/ploy/data/state/deployments.json`
- `/opt/ploy/run/platform/system-status.json`
- `/opt/ploy/run/platform/deployments.json`
- `/opt/ploy/run/platform/trading-state.json`
- `/opt/ploy/run/platform/audit-log.jsonl`

Optional hardening:

- Set `PLOY_ADMIN_TOKEN=...` in `/opt/ploy/.env` to require `Authorization: Bearer ...`
  or `x-ploy-admin-token` on the control-plane API.
- Set `PLOY_OPERATOR_TOKEN=...` in `/opt/ploy/.env` if automation or `ployctl`
  should be allowed to mutate deployments and trading controls without gaining
  full admin access.
- Set `PLOY_SIDECAR_AUTH_TOKEN=...` in `/opt/ploy/.env` if agent/sidecar clients
  only need read-only access to platform snapshots and `/api/events/stream`.
- Set `PLOY_API_AUTH_COOKIE_SECRET=...` in `/opt/ploy/.env` if browser operator
  sessions need to stay valid across daemon restarts or multiple `ployd`
  instances.
- Set `PLOY_REQUEST_RATE_LIMIT_PER_MINUTE=...` in `/opt/ploy/.env` if you want
  to tune daemon-side HTTP throttling. Set `0` to disable it.
- Set `PLOY_LIVE_RECONCILE_BACKOFF_BASE_MS=...` and
  `PLOY_LIVE_RECONCILE_BACKOFF_MAX_MS=...` in `/opt/ploy/.env` if you want to
  tune live venue reconcile retry backoff under exchange/API outages.
- `/opt/ploy/bin/ployctl` will automatically pick up `PLOY_ADMIN_TOKEN`,
  `PLOY_API_ADMIN_TOKEN`, `PLOY_API_KEY`, `PLOY_OPERATOR_TOKEN`,
  `PLOY_API_OPERATOR_TOKEN`, or `PLOY_SIDECAR_AUTH_TOKEN` from the host
  environment.
- Browser operator sessions should authenticate through `/auth/login`; `ployd`
  will issue an `HttpOnly` same-site signed session cookie so the frontend and
  SSE event stream remain authenticated without persisting the raw admin token
  in browser storage.
- Access bands are `public`, `read_only`, `operator`, and `admin`. `sidecar`
  tokens stop at `read_only`; `operator` tokens can drive deployment controls
  and trading mutations; only `admin` can read audit logs or mint browser
  sessions through `/auth/login`.

## Remote Acceptance

After deployment succeeds, do not enable real live trading immediately.

Run:

1. [`live-deployment-checklist.md`](./live-deployment-checklist.md)
2. [`live-dry-run-drill.md`](./live-dry-run-drill.md)

The dry-run drill uses a paper deployment on the live host so you can verify
control-plane and worker behavior without touching real funds.

## Post-Deploy Checks

```bash
sudo systemctl status ployd --no-pager
sudo journalctl -u ployd -n 200 --no-pager
curl -fsS http://127.0.0.1:8081/health
/opt/ploy/bin/ployctl system status
/opt/ploy/bin/ployctl system metrics
/opt/ploy/bin/ployctl system alerts
/opt/ploy/bin/ployctl system audit
/opt/ploy/bin/ployctl trading status
/opt/ploy/bin/ployctl deployments list
/opt/ploy/bin/ploytui
curl -N http://127.0.0.1:8081/api/events/stream
```

When `system status` shows non-zero `live_reconcile_failures`, a future
`next_live_reconcile_at`, or a non-empty `last_live_reconcile_error`, treat the
daemon as live-venue degraded even if the process is still up.

## Deployment Operator Flow

Use a deployment manifest like
[`config/deployments/example.paper.json`](../../config/deployments/example.paper.json)
as the template for remote deployment resources.

For remote live-host readiness checks, use the paper-mode drill manifest
[`config/deployments/example.live.dry-run.json`](../../config/deployments/example.live.dry-run.json)
and the drill script from [`live-dry-run-drill.md`](./live-dry-run-drill.md).

```bash
/opt/ploy/bin/ployctl deployments apply /opt/ploy/config/deployments/example.paper.json
/opt/ploy/bin/ployctl deployments inspect example.paper
/opt/ploy/bin/ployctl trading cancel example.live <order-id>
/opt/ploy/bin/ployctl deployments pause example.paper
/opt/ploy/bin/ployctl deployments resume example.paper
/opt/ploy/bin/ployctl deployments stop example.paper
```

## Legacy Workflows

Older workflows that still build or deploy the single-binary `ploy` path are
legacy only. They are not the default release path for the trading-platform
workspace.
