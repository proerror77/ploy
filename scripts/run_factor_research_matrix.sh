#!/usr/bin/env bash
set -euo pipefail

# Run factor research sequentially across multiple symbols on the remote host.
# This avoids overloading PostgreSQL with concurrent research jobs.
#
# Usage:
#   ./scripts/run_factor_research_matrix.sh \
#     --symbols ETHUSDT,SOLUSDT,XRPUSDT,BNBUSDT,DOGEUSDT \
#     --start-date 2026-04-09 \
#     --end-date 2026-04-11 \
#     --max-windows 12 \
#     --lob-sample-secs 5
#
# Results are written to ./tmp/factor-research/<symbol>.log by default.
#
# This matrix calls run_factor_research.sh and therefore inherits its
# PLOY_ALLOW_DIRECT_FACTOR_RESEARCH break-glass ACK requirement.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_DIR="${PLOY_RESEARCH_OUTPUT_DIR:-${ROOT_DIR}/tmp/factor-research}"
DEFAULT_SYMBOLS="ETHUSDT,SOLUSDT,XRPUSDT,BNBUSDT,DOGEUSDT"

mkdir -p "${OUTPUT_DIR}"

symbols_csv="${DEFAULT_SYMBOLS}"
args=()

while (($#)); do
  case "$1" in
    --symbols)
      shift
      symbols_csv="${1:?missing value for --symbols}"
      ;;
    *)
      args+=("$1")
      ;;
  esac
  shift || true
done

IFS=',' read -r -a symbols <<< "${symbols_csv}"

for sym in "${symbols[@]}"; do
  sym="$(echo "${sym}" | xargs)"
  [[ -z "${sym}" ]] && continue

  log_file="${OUTPUT_DIR}/${sym}.log"
  echo "=== ${sym} ==="
  "${ROOT_DIR}/scripts/run_factor_research.sh" \
    --symbols "${sym}" \
    "${args[@]}" \
    2>&1 | tee "${log_file}"
  echo
done
