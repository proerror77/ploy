#!/usr/bin/env bash
set -euo pipefail

# Run factor research on a remote host (default: tango-1-1).
# All arguments are forwarded to the factor-research binary.
#
# Usage:
#   ./scripts/run_factor_research.sh \
#     --start-date 2026-04-01 --end-date 2026-04-13 \
#     --symbols BTCUSDT,ETHUSDT --max-windows 20
#   # Optional first step when the materialized view exists:
#   ./scripts/refresh_research_valid_windows.sh
#
# The binary connects to localhost:5432 on the remote host (no SSH tunnel needed).
# stderr streams back in real time. Pipe through `tee` to save locally:
#   ./scripts/run_factor_research.sh ... 2>&1 | tee research-output.log

HOST="${PLOY_RESEARCH_HOST:-tango-1-1}"
BINARY="${PLOY_RESEARCH_BINARY:-/opt/ploy/bin/factor-research}"
DB_URL="${PLOY_RESEARCH_DB_URL:-postgresql://postgres:postgres@localhost:5432/ploy}"

exec ssh -o ServerAliveInterval=30 -o ServerAliveCountMax=10 \
  "${HOST}" systemd-run --scope --quiet \
  -p MemoryMax=3G \
  "${BINARY}" \
  --db-url "${DB_URL}" \
  --discover-valid-5m-windows \
  "$@"
