# V2 Claim/Redeem Gate For SDK And Claimer Cleanup

Status: claimer retirement applied by operator decision.

This gate still controls Phase 9 SDK slimming. Phase 10 claimer retirement has
been applied after the operator confirmed the account settlement flow has
converted away from the self-claim path.

Operational update on 2026-04-24: account-level Polymarket auto-claim is enabled
outside Ploy. Runtime hosts should store only official relayer credentials
(`RELAYER_API_KEY` and `RELAYER_API_KEY_ADDRESS`) for gasless account operations;
do not restore `CLAIMER_*`, `POLY_BUILDER_*`, or the old in-process claimer
daemon path.

## Current Dependency Evidence

Read-only checks from the local workspace show why the gate matters:

```sh
cargo tree -p polymarket-client-sdk --no-default-features --features gamma -i alloy
cargo tree -p polymarket-client-sdk --no-default-features --features data -i alloy
cargo tree -p polymarket-client-sdk --no-default-features --features ctf -i alloy
cargo metadata --format-version 1 --no-deps | grep '"name":"ploy-claimer"'
```

Observed state before V2 evidence:

- `polymarket-client-sdk` pulls `alloy` even for public `gamma` and `data`
  feature checks because `alloy` is still an unconditional dependency in the
  vendored SDK.
- `ctf` correctly needs `alloy` for contract/provider paths.
- Before retirement, `ploy-claimer` pulled both `alloy` and `ethers-core` /
  `ethers-signers`.
- After retirement, `ploy-claimer` should not appear in workspace metadata or
  any `ploy-strategy-runtime` feature graph.

## Required Post-V2 Evidence

Capture this evidence before changing code:

1. Pick a resolved V2 market where the account held a winning position.
2. Record the market id, condition id, token id, settlement timestamp, and
   account address.
3. Check the Polymarket Data API position state:
   - `redeemable`
   - payout amount
   - whether the position disappears or becomes paid without a manual redeem
4. Check wallet balance movement for settlement proceeds.
5. If no auto-credit happens, test the gasless relayer path first.
6. If relayer is unavailable or incomplete, test direct on-chain redeem on the
   smallest safe position.
7. Save tx hash, relayer id, API response, and before/after balances.

## Decision Table

| Evidence | Phase 9 action | Phase 10 action |
| --- | --- | --- |
| V2 auto-redeems or equivalent settlement credit is verified | Keep SDK signing features isolated | `ploy-claimer` retired |
| V2 still requires manual redeem but relayer works | Keep CTF/on-chain capability; make SDK public DTO paths alloy-free if possible | Retain claimer, migrate legacy relayer flow from ethers to alloy |
| V2 still requires manual redeem and relayer is unavailable | Keep CTF/on-chain capability | Retain claimer, delegate direct redeem to SDK CTF client, then remove duplicated `sol!` bindings |

## Retired Claimer Guardrails

- Do not reintroduce `ploy-claimer` as a default/live dependency.
- Do not reintroduce `ethers-core` or `ethers-signers`.
- If manual redeem becomes necessary again, add a new account-ops capability
  instead of restoring the in-process live-runner daemon.

## Gate Verification Commands

```sh
scripts/check_v2_claim_redeem_gate.sh
```

The script records the current SDK dependency state and confirms whether
`ploy-claimer` remains retired from the workspace.
