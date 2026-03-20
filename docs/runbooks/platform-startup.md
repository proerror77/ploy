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

2. Check the operator client boots:

```bash
cargo run -p ployctl
```

3. Run the smoke test:

```bash
rtk cargo test --test platform_smoke -- --nocapture
```

## Intended Operator Contract

The target operator flow is:

```bash
ployd start
ployctl deployments apply config/deployments/example.toml
ployctl deployments list
ployctl deployments inspect example.paper
```

At this stage the command wiring is still skeletal, but the workspace now has a stable platform shape for daemon, control client, deployment supervisor, and trading lifecycle crates.
