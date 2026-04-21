#!/usr/bin/env bash
set -euo pipefail

echo "V2 claim/redeem gate preflight"
echo
echo "This script records local dependency evidence only."
echo "It does not prove post-V2 auto-redeem behavior."
echo

run() {
  printf '\n$ %s\n' "$*"
  "$@"
}

run cargo tree -p polymarket-client-sdk --no-default-features --features gamma -i alloy
run cargo tree -p polymarket-client-sdk --no-default-features --features data -i alloy
run cargo tree -p polymarket-client-sdk --no-default-features --features ctf -i alloy
if cargo metadata --format-version 1 --no-deps | grep -q '"name":"ploy-claimer"'; then
  run cargo tree -p ploy-claimer -i ethers-core
  run cargo tree -p ploy-claimer -i ethers-signers
else
  echo
  echo "ploy-claimer is retired from the workspace."
fi

cat <<'MSG'

Gate status:
- Phase 9 SDK slimming remains blocked until V2 claim/redeem evidence exists.
- Phase 10 claimer retirement has been applied. Keep verifying no downstream
  crate reintroduces ploy-claimer or ethers.
MSG
