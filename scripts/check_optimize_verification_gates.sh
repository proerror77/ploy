#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workflow="${repo_root}/.github/workflows/optimize.yml"
optimizer="${repo_root}/crates/ploy-strategy-bundles/examples/optimize_backtest.rs"
stream_feed="${repo_root}/crates/ploy-strategy-bundles/src/feed/parquet_stream.rs"

failures=()

require_file() {
  local path="$1"
  if [[ ! -f "${path}" ]]; then
    failures+=("missing required file: ${path#${repo_root}/}")
  fi
}

require_text() {
  local path="$1"
  local needle="$2"
  local label="$3"
  if ! grep -Fq -- "${needle}" "${path}"; then
    failures+=("${label}: missing '${needle}' in ${path#${repo_root}/}")
  fi
}

require_any_text() {
  local path="$1"
  local label="$2"
  shift 2
  local found=0
  local needle
  for needle in "$@"; do
    if grep -Fq -- "${needle}" "${path}"; then
      found=1
      break
    fi
  done
  if [[ "${found}" -eq 0 ]]; then
    failures+=("${label}: missing all of [$*] in ${path#${repo_root}/}")
  fi
}

require_file "${workflow}"
require_file "${optimizer}"
require_file "${stream_feed}"

if [[ -f "${workflow}" ]]; then
  require_any_text "${workflow}" "preflight-only workflow gate" "run_mode" "preflight_only" "preflight-only"
  require_any_text "${workflow}" "large-window explicit override" "allow_large_window" "allow-large-window"
  require_any_text "${workflow}" "preflight row threshold" "max_preflight_rows" "max-preflight-rows"
  require_any_text "${workflow}" "preflight byte threshold" "max_preflight_bytes" "max-preflight-bytes"
  require_any_text "${workflow}" "smoke max-updates control" "max_updates" "max-updates"
  require_any_text "${workflow}" "DuckDB memory guard" "duckdb_memory_limit" "duckdb-memory-limit"
  require_any_text "${workflow}" "DuckDB temp-dir isolation" "duckdb_temp_dir" "duckdb-temp-dir"
  require_any_text "${workflow}" "timestamp train-start narrow-window control" "train_start_ts" "train-start-ts"
  require_any_text "${workflow}" "timestamp train-end narrow-window control" "train_end_ts" "train-end-ts"
  require_any_text "${workflow}" "timestamp validation-start narrow-window control" "val_start_ts" "val-start-ts"
  require_any_text "${workflow}" "timestamp validation-end narrow-window control" "val_end_ts" "val-end-ts"
  require_text "${workflow}" "timeout" "optimize process timeout wrapper"
  if grep -Fq "Swatinem/rust-cache" "${workflow}"; then
    failures+=("optimize workflow must not use Swatinem/rust-cache post steps after the backtest cache hang")
  fi
  if grep -Fq "download-artifact" "${workflow}" || grep -Fq "optimize-runner-\${{ github.sha }}" "${workflow}"; then
    failures+=("optimize workflow must build and run the optimize runner in one job, not pass the binary through artifacts")
  fi

  if ! python3 - "${workflow}" <<'PY'
import re
import sys
from pathlib import Path

lines = Path(sys.argv[1]).read_text().splitlines()

def default_for(input_name):
    in_input = False
    for line in lines:
        if re.match(rf"^\s+{re.escape(input_name)}:\s*$", line):
            in_input = True
            continue
        if in_input and re.match(r"^\s{6}\S", line):
            return None
        if in_input:
            match = re.match(r"^\s+default:\s*[\"']?([^\"'\n]+)", line)
            if match:
                return match.group(1).strip()
    return None

train_start = default_for("train_start")
train_end = default_for("train_end")
val_start = default_for("val_start")
val_end = default_for("val_end")
trials_raw = default_for("trials")
symbols_raw = default_for("symbols") or ""
symbols = [item.strip() for item in symbols_raw.split(",") if item.strip()]
try:
    trials = int(trials_raw or "0")
except ValueError:
    trials = 0

unsafe_full_window = (
    train_start == "2026-04-15"
    and train_end == "2026-04-19"
    and val_start == "2026-04-20"
    and val_end == "2026-04-22"
    and len(symbols) >= 6
    and trials >= 50
)

if unsafe_full_window:
    print(
        "unsafe workflow defaults still target Apr 15-22 with "
        f"{len(symbols)} symbols and {trials} trials"
    )
    sys.exit(1)
PY
  then
    failures+=("workflow defaults must not dispatch full Apr 15-22 six-symbol optimize before gates")
  fi
fi

if [[ -f "${optimizer}" ]]; then
  require_text "${optimizer}" "--preflight" "optimizer preflight mode"
  require_text "${optimizer}" "--max-updates" "optimizer max-updates smoke control"
  require_text "${optimizer}" "--allow-large-window" "optimizer large-window override"
  require_text "${optimizer}" "--duckdb-memory-limit" "optimizer DuckDB memory limit"
  require_text "${optimizer}" "--duckdb-temp-dir" "optimizer DuckDB temp dir"
  require_text "${optimizer}" "pm_quote_liquidity" "optimizer PM quote liquidity preflight output"
  require_text "${optimizer}" "executable_ask_rows" "optimizer executable ask-size preflight"
  require_text "${optimizer}" "zero quotes with executable ask_size" "optimizer no-liquidity hard gate"

  if grep -Eq "sample|downsample|bucket|stride" "${optimizer}"; then
    if ! grep -Fq "non-canonical" "${optimizer}"; then
      failures+=("optimizer contains sampling language without non-canonical smoke labeling")
    fi
  fi
fi

if [[ -f "${stream_feed}" ]]; then
  require_text "${stream_feed}" "CAST(bid_size AS DOUBLE) AS f3" "streaming PM quote bid-size replay"
  require_text "${stream_feed}" "CAST(ask_size AS DOUBLE) AS f4" "streaming PM quote ask-size replay"
  require_text "${stream_feed}" "bid_size," "streaming MarketUpdate quote bid-size propagation"
  require_text "${stream_feed}" "ask_size," "streaming MarketUpdate quote ask-size propagation"
  require_text "${stream_feed}" "pm_token_settlements" "streaming official settlement parquet input"
  require_text "${stream_feed}" "require_official_settlement" "streaming official-only replay gate"
  require_text "${stream_feed}" "resolve_up_won_from_settlements" "streaming settlement outcome resolver"
  require_text "${stream_feed}" "resolved_up_won," "streaming EventExpired official outcome propagation"
fi

if [[ "${#failures[@]}" -gt 0 ]]; then
  echo "FAIL: optimize verification gates are not ready"
  printf ' - %s\n' "${failures[@]}"
  exit 1
fi

echo "PASS: optimize verification gates are ready for bounded CI/self-hosted checks"
