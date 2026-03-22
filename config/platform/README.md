# Platform Config

`config/platform/` is reserved for daemon-level configuration such as:

- listen address
- database connection
- credential references
- worker supervisor defaults

This directory is for `ployd` host- and daemon-level settings only.

Deployment-specific runtime configuration should not live here.
Research/backtest strategy profiles should not live here either.

Use:

- `config/deployments/` for paper/live deployment manifests
- `config/strategies/` for research, backtest, and compatibility strategy
  profiles
