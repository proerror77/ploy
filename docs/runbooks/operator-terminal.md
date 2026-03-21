# Operator Terminal Runbook

## Purpose

Operator terminal v1 gives the dashboard a coordinator-backed operations surface.

It is intentionally limited to runtime ops:

- pause
- resume
- force close
- claim check
- claim run

It does not introduce a direct live order path.

## Startup

Start the API server first:

```bash
export PLOY_API_ADMIN_TOKEN=change-me
ploy serve --port 8081
```

Then start the dashboard in another shell:

```bash
export PLOY_API_ADMIN_TOKEN=change-me
ploy dashboard
```

Defaults:

- API base URL: `http://127.0.0.1:${PLOY_API_PORT:-8081}`
- admin header: `x-ploy-admin-token`
- dashboard fallback token variable: `PLOY_ADMIN_TOKEN`

## Operator Flow

Inside the `Operator` tab:

1. Press `g` to refresh the operator snapshot.
2. Use the domain selector to choose `global` or a specific domain.
3. Press `p` to pause, `r` to resume, or `x` to force close.
4. Press `c` for claim check.
5. Press `C` for claim run.
6. Confirm the modal before the action is sent.

All actions are dispatched through the API control plane and then through the existing coordinator control surface.

## Safety Notes

- Missing admin token fails closed.
- `claim_run` only succeeds when the build includes the claimer capability.
- If the runtime has no coordinator, pause/resume/force-close return `503`.
- The dashboard is an operator client, not a source of truth; refresh from the API when in doubt.
