#!/usr/bin/env bash
set -euo pipefail

# This collector runs as a long-lived systemd service and periodically calls:
#   ploy orderbook-history --resume-from-db
# for a rolling set of recently-active token_ids.
#
# Defaults are set for safety. Tune via env vars (typically in /opt/ploy/env/crypto-dryrun.env):
#   PLOY_ORDERBOOK_HISTORY_*  (see deployment/env.crypto-dryrun.example)

CONFIG_PATH="${PLOY_ORDERBOOK_HISTORY_CONFIG:-/opt/ploy/config/crypto_dry_run.toml}"
LOOKBACK_SECS="${PLOY_ORDERBOOK_HISTORY_LOOKBACK_SECS:-900}"
LEVELS="${PLOY_ORDERBOOK_HISTORY_LEVELS:-20}"
SAMPLE_MS="${PLOY_ORDERBOOK_HISTORY_SAMPLE_MS:-1000}"
PAGE_LIMIT="${PLOY_ORDERBOOK_HISTORY_LIMIT:-500}"
MAX_PAGES="${PLOY_ORDERBOOK_HISTORY_MAX_PAGES:-5}"
SLEEP_SECS="${PLOY_ORDERBOOK_HISTORY_SLEEP_SECS:-60}"
TOKEN_WINDOW_MINS="${PLOY_ORDERBOOK_HISTORY_TOKEN_WINDOW_MINS:-30}"
TOKEN_LIMIT="${PLOY_ORDERBOOK_HISTORY_TOKEN_LIMIT:-50}"
FALLBACK_ACTIVE_TICKS="${PLOY_ORDERBOOK_HISTORY_FALLBACK_ACTIVE_TICKS:-false}"

count_tokens() {
  local ids="$1"
  if [[ -z "$ids" ]]; then
    echo 0
    return
  fi
  echo "$ids" | tr ',' ' ' | wc -w | tr -d ' '
}

while true; do
  # Note: this assumes local Postgres DB name "ploy" and peer auth for service user.
  # If your DB differs, set PLOY_DB_NAME and adjust the psql invocation.

  # Preferred: explicit token targets (crypto + today's NBA) maintained by agents.
  ASSET_IDS="$(
    psql -d ploy -At -c \
      "SELECT string_agg(token_id, ',') FROM (SELECT token_id FROM collector_token_targets WHERE (domain = 'CRYPTO' OR (domain = 'SPORTS_NBA' AND target_date BETWEEN (CURRENT_DATE - 1) AND (CURRENT_DATE + 1))) AND (expires_at IS NULL OR expires_at > NOW()) ORDER BY updated_at DESC LIMIT ${TOKEN_LIMIT}) t;" \
      2>/dev/null
  )" || true

  # Optional fallback: old heuristic based on recently-active quote ticks.
  # Disabled by default because it can accidentally collect non-target markets.
  if [[ -z "${ASSET_IDS}" && "${FALLBACK_ACTIVE_TICKS}" == "true" ]]; then
    ASSET_IDS="$(
      psql -d ploy -At -c \
        "SELECT string_agg(token_id, ',') FROM (SELECT DISTINCT token_id FROM clob_quote_ticks WHERE received_at > NOW() - interval '${TOKEN_WINDOW_MINS} minutes' ORDER BY token_id LIMIT ${TOKEN_LIMIT}) t;" \
        2>/dev/null
    )" || true
  fi

  if [[ -z "${ASSET_IDS}" ]]; then
    echo "[orderbook-history] no active tokens found; sleeping ${SLEEP_SECS}s"
    sleep "${SLEEP_SECS}"
    continue
  fi

  echo "[orderbook-history] collecting tokens=$(count_tokens "${ASSET_IDS}") lookback_secs=${LOOKBACK_SECS}"

  /opt/ploy/bin/ploy --config "${CONFIG_PATH}" orderbook-history \
    --asset-ids "${ASSET_IDS}" \
    --lookback-secs "${LOOKBACK_SECS}" \
    --levels "${LEVELS}" \
    --sample-ms "${SAMPLE_MS}" \
    --limit "${PAGE_LIMIT}" \
    --max-pages "${MAX_PAGES}" \
    --resume-from-db || true

  sleep "${SLEEP_SECS}"
done
