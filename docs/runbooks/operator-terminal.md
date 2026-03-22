# Operator Terminal Runbook

## Purpose

Operator terminal v1 gives the dashboard a control-plane-backed operations surface.

It is intentionally limited to runtime ops:

- pause
- resume
- force close
- claims inspect
- claims run
- claims pause
- claims resume

It does not introduce a direct live order path.

## Startup

Start the control plane first:

```bash
export PLOY_API_ADMIN_TOKEN=change-me
cargo run -p ployd
```

Then start the dashboard in another shell:

```bash
export PLOY_API_ADMIN_TOKEN=change-me
cargo run -p ploytui -- --watch
```

Defaults:

- API base URL: `http://127.0.0.1:8081`
- admin header: `x-ploy-admin-token`
- dashboard fallback token variable: `PLOY_ADMIN_TOKEN`

## Operator Flow

Inside the operator surface:

1. Use `ployctl claims list` to inspect account-level auto-claim status.
2. Use `ployctl claims inspect <account-id>` to inspect redeemable positions and claim history.
3. Use `ployctl claims run <account-id>` to trigger a one-shot claim loop.
4. Use `ployctl claims pause <account-id>` or `ployctl claims resume <account-id>` to override the default-on loop.
5. Use `ploytui` to watch deployment, trading, and claim summaries from the same control plane.

All actions are dispatched through the `ployd` control plane.

## Safety Notes

- Missing admin token fails closed.
- Auto-claim is account-level, defaults on for live accounts only, and submits
  redeem transactions through the Polymarket relayer.
- Relay-backed auto-claim requires `POLY_SIGNATURE_TYPE=proxy` or
  `gnosis_safe`; plain EOA wallets are not supported. `gnosis_safe` wallets
  will auto-deploy their SAFE through the relayer before the first redeem.
- Claim failures degrade the account and retry with backoff; they do not stop the platform.
- The terminal is an operator client, not a source of truth; refresh from the API when in doubt.
