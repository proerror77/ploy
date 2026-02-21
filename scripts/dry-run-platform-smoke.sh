#!/usr/bin/env bash
set -euo pipefail

# Quick dry-run smoke test for platform startup without live orders.
# This script intentionally never sends real orders.

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

export PLOY_DRY_RUN__ENABLED="${PLOY_DRY_RUN__ENABLED:-true}"
export PLOY_RUN_SQLX_MIGRATIONS="${PLOY_RUN_SQLX_MIGRATIONS:-true}"
export PLOY_REQUIRE_SQLX_MIGRATIONS="${PLOY_REQUIRE_SQLX_MIGRATIONS:-false}"
export PLOY_DEPLOYMENTS_FILE="${PLOY_DEPLOYMENTS_FILE:-deployment/deployments.json}"
export PLOY_COORDINATOR__HEARTBEAT_STALE_WARN_COOLDOWN_SECS="${PLOY_COORDINATOR__HEARTBEAT_STALE_WARN_COOLDOWN_SECS:-300}"

if [[ ! -f "$PLOY_DEPLOYMENTS_FILE" ]]; then
  echo "Deployment matrix not found: $PLOY_DEPLOYMENTS_FILE"
  exit 1
fi

echo "[1/3] static checks"
cargo fmt --check >/tmp/ploy_dryrun_fmt.log 2>&1
cargo check -q >/tmp/ploy_dryrun_check.log 2>&1

echo "[2/3] platform command validation"
cargo run --bin ploy -- platform --help >/tmp/ploy_dryrun_platform_help.log 2>&1

echo "[3/3] 45s dry-run startup smoke"
set +e
if command -v timeout >/dev/null 2>&1; then
  timeout 45s cargo run --bin ploy -- platform start --dry-run --crypto --sports >/tmp/ploy_dryrun_start.log 2>&1
  rc=$?
else
  # shell fallback
  cargo run --bin ploy -- platform start --dry-run --crypto --sports > /tmp/ploy_dryrun_start.log 2>&1 &
  sleep 45
  kill $! >/dev/null 2>&1
  rc=$?
fi
set -e

if (( rc != 0 && rc != 124 )); then
  echo "dry-run start failed (exit $rc). last logs:"
  tail -n 80 /tmp/ploy_dryrun_start.log
  exit $rc
fi

echo "Smoke done. log path:"
echo " - /tmp/ploy_dryrun_fmt.log"
echo " - /tmp/ploy_dryrun_check.log"
echo " - /tmp/ploy_dryrun_platform_help.log"
echo " - /tmp/ploy_dryrun_start.log"
echo "rc=$rc (124 expected when timeout expires)"
exit 0
