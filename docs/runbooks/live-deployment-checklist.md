# Minimal Live Deployment Checklist

Use this checklist when you want to bring up one real Polymarket deployment on
the current workspace platform.

This path is for `ployd` only.

Do not use the archived single-binary `ploy ...` runtime as your live entry
point.

## 1. Preconditions

- Deploy the CI-built platform bundle from `.github/workflows/release-platform.yml`
- Host has `ployd.service` installed
- Host has `/opt/ploy/bin/ployd`, `/opt/ploy/bin/ployctl`, and
  `/opt/ploy/bin/ploytui`
- Host has `/opt/ploy/config/deployments/example.live.json`
- Host `.env` has live credentials and control-plane auth set

## 2. Required Live `.env`

At minimum, set these in `/opt/ploy/.env`:

```bash
PLOY_ADMIN_TOKEN=change-me

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

Recommended for browser sessions that should survive daemon restarts:

```bash
PLOY_API_AUTH_COOKIE_SECRET=replace-me-with-32-bytes-of-randomness
```

If you run a SAFE-backed wallet instead of a proxy wallet:

- set `POLY_SIGNATURE_TYPE=gnosis_safe`
- omit `POLY_FUNDER` unless your signing setup still requires it

Optional hardening:

```bash
PLOY_SIDECAR_AUTH_TOKEN=read-only-token
PLOY_REQUEST_RATE_LIMIT_PER_MINUTE=240
PLOY_CLAIM_TICK_INTERVAL_MS=30000
PLOY_CLAIM_BACKOFF_BASE_MS=5000
PLOY_CLAIM_BACKOFF_MAX_MS=120000
PLOY_LIVE_RECONCILE_BACKOFF_BASE_MS=1000
PLOY_LIVE_RECONCILE_BACKOFF_MAX_MS=30000
```

## 3. Start The Daemon

```bash
sudo systemctl restart ployd
sudo systemctl status ployd --no-pager
sudo systemctl show ployd -p MemoryMax -p Restart -p OOMPolicy
curl -fsS http://127.0.0.1:8081/health
set -a; . /opt/ploy/.env; set +a
```

Expected:

- `ployd` is `active (running)`
- `Restart=always`
- `OOMPolicy=kill`
- `/health` returns `ok`

## 4. Verify Control Plane Before Applying Live Deployment

```bash
set -a; . /opt/ploy/.env; set +a
/opt/ploy/bin/ployctl system status
/opt/ploy/bin/ployctl system audit
/opt/ploy/bin/ployctl claims list
/opt/ploy/bin/ployctl deployments list
```

Treat the daemon as not-ready if:

- `system status` already shows `status=degraded`
- `live_reconcile_failures` is non-zero before any live deployment is running
- claim state shows repeated failures on the target live account

## 5. Apply The Minimal Live Manifest

Use the checked-in template:

```bash
/opt/ploy/bin/ployctl deployments apply /opt/ploy/config/deployments/example.live.json
/opt/ploy/bin/ployctl deployments inspect example.live
/opt/ploy/bin/ployctl trading inspect example.live
```

Expected:

- `mode=live`
- `lifecycle=Enabled`
- `desired=Running`
- `observed=Starting` or `observed=Running`

## 6. 24h Runtime Checks

Use these as the minimum operator loop:

```bash
set -a; . /opt/ploy/.env; set +a
/opt/ploy/bin/ployctl system status
/opt/ploy/bin/ployctl system audit
/opt/ploy/bin/ployctl claims list
/opt/ploy/bin/ployctl claims inspect acct-live
/opt/ploy/bin/ployctl trading status
/opt/ploy/bin/ployctl trading inspect example.live
curl -N http://127.0.0.1:8081/api/events/stream
```

Escalate immediately if:

- `system status` shows `status=degraded`
- `last_live_reconcile_error` is non-empty
- `next_live_reconcile_at` keeps moving forward for a long outage window
- `claims inspect acct-live` shows growing `consecutive_failures`
- `deployments inspect example.live` falls into `observed=failed`

## 7. Safe Operator Actions

```bash
/opt/ploy/bin/ployctl deployments drain example.live
/opt/ploy/bin/ployctl deployments pause example.live
/opt/ploy/bin/ployctl deployments resume example.live
/opt/ploy/bin/ployctl trading cancel example.live <order-id>
/opt/ploy/bin/ployctl claims run acct-live
```

Use `drain` when you want exits/cancels to continue but do not want fresh entry
intents.
