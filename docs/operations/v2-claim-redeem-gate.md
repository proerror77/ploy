# V2 Claim/Redeem Gate For SDK And Claimer Cleanup

Status: blocked until Polymarket V2 claim/redeem behavior is observed after
cutover and stabilization.

This gate controls Phase 9 and Phase 10 of the codebase slimming plan. Do not
remove SDK signing dependencies, migrate claimer flows, or retire
`ploy-claimer` before this evidence exists.

## Current Dependency Evidence

Read-only checks from the local workspace show why the gate matters:

```sh
cargo tree -p polymarket-client-sdk --no-default-features --features gamma -i alloy
cargo tree -p polymarket-client-sdk --no-default-features --features data -i alloy
cargo tree -p polymarket-client-sdk --no-default-features --features ctf -i alloy
cargo tree -p ploy-claimer -i ethers-core
cargo tree -p ploy-claimer -i ethers-signers
```

Observed state before V2 evidence:

- `polymarket-client-sdk` pulls `alloy` even for public `gamma` and `data`
  feature checks because `alloy` is still an unconditional dependency in the
  vendored SDK.
- `ctf` correctly needs `alloy` for contract/provider paths.
- `ploy-claimer` pulls both `alloy` and `ethers-core` / `ethers-signers`.
- `ethers` is concentrated in relayer legacy support and proxy calldata/signing
  helpers.

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
| V2 auto-redeems or equivalent settlement credit is verified | Keep SDK signing features isolated; `ploy-claimer` becomes a retirement candidate | Remove claimer only after live runner no longer depends on it and no account has outstanding manual claims |
| V2 still requires manual redeem but relayer works | Keep CTF/on-chain capability; make SDK public DTO paths alloy-free if possible | Retain claimer, migrate legacy relayer flow from ethers to alloy |
| V2 still requires manual redeem and relayer is unavailable | Keep CTF/on-chain capability | Retain claimer, delegate direct redeem to SDK CTF client, then remove duplicated `sol!` bindings |

## Forbidden Before Gate Opens

- Do not remove `ploy-claimer` from the workspace.
- Do not remove `ethers-core` or `ethers-signers` unless relayer legacy tests
  have alloy replacements.
- Do not replace direct redeem with SDK CTF unless behavior is verified against
  V2 positions.
- Do not change live runner claim startup wiring based on documentation alone.

## Gate Verification Commands

```sh
scripts/check_v2_claim_redeem_gate.sh
```

The script does not prove V2 behavior. It records the current dependency state
and exits successfully only as a preflight/inventory check.
