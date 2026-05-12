# Strategy Runtime Evidence Reset Runbook

This runbook is for clearing contaminated dry-run or paper strategy runtime
evidence after a strategy cutover. It intentionally does not reset raw market
data, official settlements, Polymarket metadata, Binance/Deribit data, or CLOB
orderbook history.

Use this only when the evidence stage is `dry_run_candidate`,
`runtime_parity`, or a later promotion review that is blocked by stale
`strategy_runtime_orders` / `strategy_runtime_fills` rows.

## Safety Contract

- Never run a destructive reset while the target deployment is desired or
  observed `running`.
- Never set `allow_running=true` for PM5D dry-run cleanup unless a separate
  incident issue explains why pausing is impossible.
- Always run a preview first and keep the uploaded backup artifact.
- Destructive execution requires both:
  - `execute=true`
  - `confirm=delete-strategy-runtime-evidence`
- The reset workflow is scoped to dry-run/paper modes only:
  `dry_run,dryrun,paper`.

## PM5D Settlement Dry-Run Target

Current PM5D settlement-probability cleanup target:

```text
deployment_id=pm5d.threelayer.settlement-probability-btc-eth.dryrun
strategy_id=three_layer
runtime_modes=dry_run,dryrun,paper
```

## Preflight

1. Confirm the research semantic contract and evidence stage:

   ```bash
   rtk read docs/PROJECT_SEMANTICS.md
   rtk read docs/runbooks/strategy-research-cicd.md
   ```

2. Read the current deployment state:

   ```bash
   curl -fsS --max-time 10 http://8.221.143.151/api/deployments \
     | jq '.[]? | select(.deployment_id=="pm5d.threelayer.settlement-probability-btc-eth.dryrun") | {deployment_id,desired_state,observed_state,deployment_state}'
   ```

3. Read the current dry-run report:

   ```bash
   curl -fsS --max-time 10 http://8.221.143.151/api/reports/dry-run \
     | jq '.strategies[]? | select(.deployment_id=="pm5d.threelayer.settlement-probability-btc-eth.dryrun") | {summary,metrics,execution_diagnostics}'
   ```

4. Confirm there is an operator approval record. For PM5D settlement dry-run,
   the approval text should explicitly say:

   ```text
   同意暂停 dry-run 并清空 settlement-probability 的 runtime evidence
   ```

## Preview Reset

Run the GitHub workflow from `main` with `execute=false`:

```bash
gh workflow run reset-strategy-runtime-evidence.yml \
  --repo proerror77/ploy \
  --ref main \
  -f git_ref=main \
  -f deployment_id=pm5d.threelayer.settlement-probability-btc-eth.dryrun \
  -f strategy_id=three_layer \
  -f runtime_modes=dry_run,dryrun,paper \
  -f execute=false \
  -f allow_running=false \
  -f confirm=
```

Download the artifact and inspect `manifest.json`. The preview should report
matched orders and fills, but `deleted_orders` must be `0`.

## Pause The Deployment

Pause the target with the operator control plane before destructive reset:

```bash
ssh tango-1-1 '/opt/ploy/bin/ployctl deployments pause pm5d.threelayer.settlement-probability-btc-eth.dryrun'
```

Then re-read `/api/deployments` until both desired and observed state are no
longer `running`.

Do not continue to destructive reset if either state remains `running`.

## Execute Reset

Run the workflow from `main`:

```bash
gh workflow run reset-strategy-runtime-evidence.yml \
  --repo proerror77/ploy \
  --ref main \
  -f git_ref=main \
  -f deployment_id=pm5d.threelayer.settlement-probability-btc-eth.dryrun \
  -f strategy_id=three_layer \
  -f runtime_modes=dry_run,dryrun,paper \
  -f execute=true \
  -f allow_running=false \
  -f confirm=delete-strategy-runtime-evidence
```

The `tango-1-1` environment approval gate must remain enabled. After the run,
download the reset artifact and verify:

- `guard-status.json` has `status=allowed`.
- `manifest.json` exists.
- `before.orders` / `before.fills` match the intended target rows.
- `deleted_orders` equals the expected target order count.
- `after.orders=0` and `after.fills=0` for the reset scope.
- Backup JSON files are present in the artifact.

## Resume Clean Observation

Resume only after reset artifacts are inspected:

```bash
ssh tango-1-1 '/opt/ploy/bin/ployctl deployments resume pm5d.threelayer.settlement-probability-btc-eth.dryrun'
```

Then verify:

```bash
curl -fsS --max-time 10 http://8.221.143.151/api/deployments \
  | jq '.[]? | select(.deployment_id=="pm5d.threelayer.settlement-probability-btc-eth.dryrun") | {deployment_id,desired_state,observed_state,deployment_state}'

curl -fsS --max-time 10 http://8.221.143.151/api/reports/dry-run \
  | jq '.strategies[]? | select(.deployment_id=="pm5d.threelayer.settlement-probability-btc-eth.dryrun") | {summary,metrics,execution_diagnostics}'
```

The post-reset dry-run report should start from a clean baseline. A new
profitability claim needs a fresh observation window, not the reset itself.

## Promotion Blockers After Reset

Do not promote a PM5D strategy after reset until all of these are true:

- retained market data coverage is clean for the declared window;
- executable-price replay or walk-forward evidence is present;
- official settlement accounting is present for settlement-probability lanes;
- recorded replay/dry-run parity is strict-ready on the fresh sample;
- dry-run PnL, drawdown, fills, quote age, and capacity support the stated
  risk limits.
