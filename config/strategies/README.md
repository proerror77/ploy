# Strategy Profiles

`config/strategies/` is for offline research, backtest preparation, and
archived compatibility strategy profiles.

It is not the default runtime entrypoint for the current workspace platform.

Use this directory when you are:

- running offline backtests or replay experiments
- preparing datasets for `ploy strategy backfill-*`
- keeping historical strategy TOML profiles around for reference

Do not point `ployctl deployments apply` at files in this directory.

For the current platform runtime:

- use `config/deployments/` for paper/live deployment manifests
- use `ployd` + `ployctl` + `ploytui` for operator actions

Naming convention:

- `*_backtest.toml`: research-only
- `*_live.toml`: compatibility profile, not a `ployd` deployment manifest
