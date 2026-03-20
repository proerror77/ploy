# Platform Startup Runbook

## Goal

Start the single-host trading platform runtime with:

- one `ployd` daemon
- one or more managed deployment workers
- one operator client path through `ployctl`

## Default Local Flow

1. Check the daemon boots:

```bash
cargo run -p ployd
```

2. In a second shell, check the operator client against the control-plane surface:

```bash
cargo run -p ployctl -- system status
cargo run -p ployctl -- deployments list
```

3. Run the smoke test:

```bash
rtk cargo test --test platform_smoke -- --nocapture
```

## Intended Operator Contract

The current operator flow is:

```bash
ployd
ployctl system status
ployctl deployments list
ployctl deployments inspect example.paper
```

This branch now treats `ployd` as the default long-running daemon entrypoint and
`ployctl` as an HTTP-first operator client with snapshot fallback. Deployment
CRUD is still simplified, but the daemon/client/runtime contract is no longer a
placeholder.
