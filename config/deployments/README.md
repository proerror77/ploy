# Deployment Manifests

`config/deployments/` is reserved for deployment manifests.

Each manifest should describe one deployment resource.
The current minimal operator path uses JSON manifests such as
`config/deployments/example.paper.json`.

Current examples:

- `example.paper.json`
  - local or remote paper deployment smoke path
- `example.live.dry-run.json`
  - remote live-host readiness drill
  - still uses `runtime_mode: "paper"` so the drill never touches real funds

Each manifest includes:

- `deployment_id`
- `bundle_id`
- `runtime_mode`
- `desired_state`

The operator manages deployment resources. The platform manages the worker processes behind them.

Do not point the dry-run drill at a real live manifest. The drill is meant to
prove host readiness, not to enable production trading.
