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
- `config/deployments/example.paper.json`
- `config/deployments/example.live.json`
- `deployment/ployd.service`
- `scripts/install-platform-service.sh`
- `data/state/deployments.json.sample`

## Host Installation Flow

After the bundle lands on the host, the deploy workflow:

1. Installs `ployd`, `ployctl`, and `ploytui` into `/opt/ploy/bin`
2. Installs `config/deployments/example.paper.json` and `example.live.json`
   into `/opt/ploy/config/deployments`
3. Installs `deployment/ployd.service` into `/opt/ploy/deployment`
4. Installs `scripts/install-platform-service.sh` into `/opt/ploy/scripts`
5. Seeds `/opt/ploy/data/state/deployments.json` if missing
6. Runs `install-platform-service.sh`
7. Restarts `ployd`
8. Verifies:
   - `systemctl status ployd`
   - `systemctl show ployd -p MemoryMax -p Restart -p OOMPolicy`
   - `curl -fsS http://127.0.0.1:8081/health`
   - `/opt/ploy/bin/ployctl system status`
   - `/opt/ploy/bin/ployctl system audit`
   - `/opt/ploy/bin/ployctl claims list`
   - `/opt/ploy/bin/ployctl trading status`
   - `/opt/ploy/bin/ploytui`
   - `curl -N http://127.0.0.1:8081/api/events/stream`

## Required Host Paths

The install script ensures directories and seed files, not daemon-generated
runtime snapshots.

Install-time guarantees:

- `/opt/ploy/.env`
- `/opt/ploy/config/deployments/`
- `/opt/ploy/config/deployments/example.paper.json`
- `/opt/ploy/config/deployments/example.live.json`
- `/opt/ploy/data/state/deployments.json`
- `/opt/ploy/run/platform/`

Daemon-generated after boot:

- `/opt/ploy/run/platform/system-status.json`
- `/opt/ploy/run/platform/deployments.json`
- `/opt/ploy/run/platform/trading-state.json`
- `/opt/ploy/run/platform/account-claims.json`
- `/opt/ploy/run/platform/audit-log.jsonl`

## Required Live Host `.env`

For a minimal live host, set at least:

```bash
PLOY_ADMIN_TOKEN=change-me
PLOY_API_AUTH_COOKIE_SECRET=replace-me-with-randomness

POLYMARKET_PRIVATE_KEY=0x...
POLYMARKET_API_KEY=...
POLYMARKET_API_SECRET=...
POLYMARKET_PASSPHRASE=...
POLY_SIGNATURE_TYPE=proxy
POLY_FUNDER=0x...

POLY_RELAYER_URL=https://relayer-v2.polymarket.com
POLY_BUILDER_API_KEY=...
POLY_BUILDER_SECRET=...
POLY_BUILDER_PASSPHRASE=...
POLYGON_RPC_URL=https://polygon.drpc.org
```

If you use a SAFE-backed wallet, set `POLY_SIGNATURE_TYPE=gnosis_safe`.

Optional hardening:

- `PLOY_SIDECAR_AUTH_TOKEN=...`
- `PLOY_REQUEST_RATE_LIMIT_PER_MINUTE=...`
- `PLOY_CLAIM_TICK_INTERVAL_MS=...`
- `PLOY_CLAIM_BACKOFF_BASE_MS=...`
- `PLOY_CLAIM_BACKOFF_MAX_MS=...`
- `PLOY_LIVE_RECONCILE_BACKOFF_BASE_MS=...`
- `PLOY_LIVE_RECONCILE_BACKOFF_MAX_MS=...`

`/opt/ploy/bin/ployctl` automatically reuses `PLOY_ADMIN_TOKEN`,
`PLOY_API_ADMIN_TOKEN`, or `PLOY_API_KEY` from the host environment.

Browser operator sessions authenticate through `/auth/login`; `ployd` issues an
`HttpOnly` same-site signed session cookie so the frontend and SSE event stream
remain authenticated without storing the raw admin token in browser storage.

## Post-Deploy Checks

```bash
sudo systemctl status ployd --no-pager
sudo systemctl show ployd -p MemoryMax -p Restart -p OOMPolicy
sudo journalctl -u ployd -n 200 --no-pager
curl -fsS http://127.0.0.1:8081/health
/opt/ploy/bin/ployctl system status
/opt/ploy/bin/ployctl system audit
/opt/ploy/bin/ployctl claims list
/opt/ploy/bin/ployctl trading status
/opt/ploy/bin/ployctl deployments list
/opt/ploy/bin/ploytui
curl -N http://127.0.0.1:8081/api/events/stream
```

When `system status` shows non-zero `live_reconcile_failures`, a future
`next_live_reconcile_at`, or a non-empty `last_live_reconcile_error`, treat the
daemon as live-venue degraded even if the process is still up.

## Deployment Operator Flow

Use
[`config/deployments/example.live.json`](../../config/deployments/example.live.json)
as the template for a minimal live deployment resource.

```bash
/opt/ploy/bin/ployctl deployments apply /opt/ploy/config/deployments/example.live.json
/opt/ploy/bin/ployctl deployments inspect example.live
/opt/ploy/bin/ployctl claims list
/opt/ploy/bin/ployctl claims inspect acct-live
/opt/ploy/bin/ployctl claims run acct-live
/opt/ploy/bin/ployctl trading inspect example.live
/opt/ploy/bin/ployctl trading cancel example.live <order-id>
/opt/ploy/bin/ployctl deployments drain example.live
/opt/ploy/bin/ployctl deployments pause example.live
/opt/ploy/bin/ployctl deployments resume example.live
/opt/ploy/bin/ployctl deployments stop example.live
```

For a smaller local smoke path, keep using
[`config/deployments/example.paper.json`](../../config/deployments/example.paper.json).

## Legacy Workflows

Older workflows that still build or deploy the single-binary `ploy` path are
legacy only. They are not the default release path for the trading-platform
workspace.
