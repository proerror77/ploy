# Deployment Manifests

`config/deployments/` is reserved for deployment manifests.

Each manifest should describe one deployment resource.
The default operator path uses JSON manifests such as:

- `config/deployments/example.paper.json` for local paper smoke and dry-run verification
- `config/deployments/example.live.json` for a minimal live deployment template

Each manifest includes:

- `deployment_id`
- `account_id`
- `max_gross_exposure`
- `runtime_mode`
- `bundle_id`
- `desired_state`

Use this directory for `ployd` deployment resources only.

Do not put strategy backtest profiles or dataset-prep configs here. Those stay
under `config/strategies/` and the offline research/backtest command family.

The operator manages deployment resources. The platform manages the worker
processes behind them.
