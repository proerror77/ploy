# Deployment Manifests

`config/deployments/` is reserved for deployment manifests.

Each manifest should describe one deployment resource.
The current minimal operator path uses JSON manifests such as
`config/deployments/example.paper.json`.

Each manifest includes:

- `deployment_id`
- `bundle_id`
- `runtime_mode`
- `desired_state`

The operator manages deployment resources. The platform manages the worker processes behind them.
