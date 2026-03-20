# Deployment Manifests

`config/deployments/` is reserved for deployment manifests.

Each manifest should describe one deployment resource:

- `deployment_id`
- `bundle_id`
- `account_profile`
- `market_scope`
- `risk_profile`
- `execution_profile`
- `runtime_mode`
- `desired_state`

The operator manages deployment resources. The platform manages the worker processes behind them.
