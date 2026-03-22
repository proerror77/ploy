# Research And Backtest Routing

This branch supports both live runtime and offline research, but they do not
share the same entrypoint.

## Operator / Live Runtime

Use this path when you want a daemon, deployment lifecycle control, live/paper
runtime state, claims, and order management:

- `cargo run -p ployd`
- `cargo run -p ployctl -- ...`
- `cargo run -p ploytui -- --watch`
- `config/deployments/*.json`

This is the only default operator path for the current workspace platform.

## Research / Backtest / Dataset Prep

Use this path when you want offline datasets, replay, or backtest preparation:

- `ploy collect`
- `ploy collect --check-only`
- `ploy orderbook-history`
- `ploy deribit-iv-backfill`
- `ploy strategy backfill-*`
- `crates/ploy-research`
- `config/strategies/*.toml`

These are offline or compatibility-oriented workflows. They do not run inside
`ployd`.

## Rule Of Thumb

- Want a 24x7 daemon or a deployment resource: use `ployd`
- Want historical data, replay tables, or backtest prep: use the offline
  `ploy ...` command family

## What Not To Do

- Do not try to run backtests inside `ployd`
- Do not point `ployctl deployments apply` at `config/strategies/*.toml`
- Do not treat archived single-binary `ploy platform start ...` commands as the
  default live path on this branch

For the data-prep side, see [`docs/COLLECTOR_RUNBOOK.md`](../COLLECTOR_RUNBOOK.md).
For the current daemon path, see
[`docs/runbooks/platform-startup.md`](platform-startup.md) and
[`docs/runbooks/live-deployment-checklist.md`](live-deployment-checklist.md).
