# Platform Deploy Runbook

## Default Release Path

The workspace-default deploy path is:

- GitHub Actions workflow: `.github/workflows/release-platform.yml`
- Long-running systemd unit: `deployment/ployd.service`
- Host install helper: `scripts/install-platform-service.sh`

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

## CI Bundle Contents

The release bundle contains:

- `bin/ployd`
- `bin/ployctl`
- `bin/ploytui`
- `deployment/ployd.service`
- `scripts/install-platform-service.sh`
- `data/state/deployments.json.sample`

## Host Installation Flow

After the bundle lands on the host, the deploy workflow:

1. Installs `ployd`, `ployctl`, and `ploytui` into `/opt/ploy/bin`
2. Installs `deployment/ployd.service` into `/opt/ploy/deployment`
3. Installs `scripts/install-platform-service.sh` into `/opt/ploy/scripts`
4. Seeds `/opt/ploy/data/state/deployments.json` if missing
5. Runs `install-platform-service.sh`
6. Restarts `ployd`
7. Verifies:
   - `systemctl status ployd`
   - `curl -fsS http://127.0.0.1:8081/health`
   - `/opt/ploy/bin/ployctl system status`
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

Optional hardening:

- Set `PLOY_ADMIN_TOKEN=...` in `/opt/ploy/.env` to require `Authorization: Bearer ...`
  or `x-ploy-admin-token` on the control-plane API.
- `/opt/ploy/bin/ployctl` will automatically pick up `PLOY_ADMIN_TOKEN`,
  `PLOY_API_ADMIN_TOKEN`, or `PLOY_API_KEY` from the host environment.
- Browser operator sessions should authenticate through `/auth/login`; `ployd`
  will issue an `HttpOnly` same-site session cookie so the frontend and SSE
  event stream remain authenticated without persisting the raw admin token in
  browser storage.

## Post-Deploy Checks

```bash
sudo systemctl status ployd --no-pager
sudo journalctl -u ployd -n 200 --no-pager
curl -fsS http://127.0.0.1:8081/health
/opt/ploy/bin/ployctl system status
/opt/ploy/bin/ployctl trading status
/opt/ploy/bin/ployctl deployments list
/opt/ploy/bin/ploytui
curl -N http://127.0.0.1:8081/api/events/stream
```

## Deployment Operator Flow

Use a deployment manifest like
[`config/deployments/example.paper.json`](../../config/deployments/example.paper.json)
as the template for remote deployment resources.

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
