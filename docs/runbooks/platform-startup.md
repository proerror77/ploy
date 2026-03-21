# Platform Startup Runbook

## Goal

Start the single-host trading platform runtime with:

- one `ployd` daemon
- one or more managed deployment workers
- one operator client path through `ployctl`
- one optional terminal console through `ploytui`

## Default Local Flow

1. Check the daemon boots:

```bash
cargo run -p ployd
```

If you want the control plane protected, export `PLOY_ADMIN_TOKEN` first.
Optionally set `PLOY_SIDECAR_AUTH_TOKEN` for read-only sidecar/agent access.
`ployctl` uses the admin bearer token surface directly. The frontend login
flow uses `/auth/login`, which sets an `HttpOnly` same-site signed session
cookie so `/api/events/stream` stays authenticated. Set
`PLOY_API_AUTH_COOKIE_SECRET` too if you want those browser sessions to survive
daemon restarts. Set `PLOY_REQUEST_RATE_LIMIT_PER_MINUTE` if you need a tighter
or looser daemon-side HTTP throttle; `0` disables it.

2. In a second shell, check the operator client against the control-plane surface:

```bash
cargo run -p ployctl -- system status
cargo run -p ployctl -- system audit
cargo run -p ployctl -- trading status
cargo run -p ployctl -- deployments list
cargo run -p ploytui
curl -N http://127.0.0.1:8081/api/events/stream
```

3. Run the smoke test:

```bash
rtk cargo test --test platform_smoke -- --nocapture
```

## Intended Operator Contract

The current operator flow is:

```bash
ployd
ployctl deployments apply config/deployments/example.paper.json
ployctl system status
ployctl system audit
ployctl trading status
ployctl deployments list
ployctl deployments inspect example.paper
ployctl trading cancel example.live <order-id>
ploytui
curl -N http://127.0.0.1:8081/api/events/stream
ployctl deployments pause example.paper
ployctl deployments resume example.paper
ployctl deployments stop example.paper
```

This branch now treats `ployd` as the default long-running daemon entrypoint and
`ployctl` as an HTTP-first operator client with snapshot fallback. `ploytui` is
the thin terminal console on top of the same control-plane API. Deployment CRUD
is still simplified, but the daemon/client/runtime contract is no longer a
placeholder. `ployd` also keeps an append-only `run/platform/audit-log.jsonl`
for authenticated control-plane actions, and `ployctl system audit` reads the
latest entries back over `/api/audit/logs`.
