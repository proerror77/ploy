#!/usr/bin/env bash
set -euo pipefail

duration_secs="${SOAK_DURATION_SECS:-86400}"
interval_secs="${SOAK_INTERVAL_SECS:-30}"
base_url="${SOAK_BASE_URL:-http://localhost}"
rpc_config="${SOAK_RPC_CONFIG:-config/default.toml}"
log_dir="${SOAK_LOG_DIR:-data/soak}"
compose_cmd="${SOAK_COMPOSE_CMD:-docker compose -f docker-compose.prod.yml}"

inject_db_restart=false
inject_process_restart=false
inject_network_blip=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --duration)
      duration_secs="$2"
      shift 2
      ;;
    --interval)
      interval_secs="$2"
      shift 2
      ;;
    --base-url)
      base_url="$2"
      shift 2
      ;;
    --rpc-config)
      rpc_config="$2"
      shift 2
      ;;
    --inject-db-restart)
      inject_db_restart=true
      shift
      ;;
    --inject-process-restart)
      inject_process_restart=true
      shift
      ;;
    --inject-network-blip)
      inject_network_blip=true
      shift
      ;;
    -h|--help)
      cat <<'EOF'
Usage:
  scripts/soak_test_24h.sh [options]

Options:
  --duration <seconds>          Test duration (default: 86400)
  --interval <seconds>          Probe interval (default: 30)
  --base-url <url>              Probe base URL (default: http://localhost)
  --rpc-config <path>           Config path for `ploy rpc` (default: config/default.toml)
  --inject-db-restart           Restart postgres once around 1/3 of test
  --inject-process-restart      Restart backend once around 2/3 of test
  --inject-network-blip         Pause backend container briefly around 1/2 of test

Env:
  SOAK_COMPOSE_CMD              Compose command (default: docker compose -f docker-compose.prod.yml)
  SOAK_LOG_DIR                  Output dir (default: data/soak)
EOF
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

mkdir -p "$log_dir"
ts_tag="$(date +%Y%m%d-%H%M%S)"
log_file="$log_dir/soak-$ts_tag.jsonl"

start_epoch="$(date +%s)"
end_epoch="$((start_epoch + duration_secs))"

db_restarted=false
proc_restarted=false
network_blipped=false

passes=0
fails=0

detect_ploy_bin() {
  if [[ -x target/release/ploy ]]; then
    echo "target/release/ploy"
    return 0
  fi
  if [[ -x target/debug/ploy ]]; then
    echo "target/debug/ploy"
    return 0
  fi
  echo ""
}

ploy_bin="$(detect_ploy_bin)"

json_escape() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

log_event() {
  local ts="$1"
  local health="$2"
  local ready="$3"
  local api="$4"
  local rpc_ok="$5"
  local status="$6"
  local note="$7"
  printf '{"ts":"%s","health":"%s","ready":"%s","api":"%s","rpc_ok":%s,"status":"%s","note":"%s"}\n' \
    "$ts" "$health" "$ready" "$api" "$rpc_ok" "$status" "$(json_escape "$note")" >>"$log_file"
}

run_probe() {
  local ts
  ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  local health_code ready_code api_code rpc_ok status note

  health_code="$(curl -sS -o /dev/null -w '%{http_code}' "$base_url/health" || echo 000)"
  ready_code="$(curl -sS -o /dev/null -w '%{http_code}' "$base_url/readyz" || echo 000)"
  api_code="$(curl -sS -o /dev/null -w '%{http_code}' "$base_url/api/system/status" || echo 000)"

  rpc_ok=false
  if [[ -n "$ploy_bin" ]]; then
    rpc_body='{"jsonrpc":"2.0","id":"soak","method":"system.ping","params":{}}'
    rpc_resp="$(printf '%s' "$rpc_body" | "$ploy_bin" rpc --config "$rpc_config" 2>/dev/null || true)"
    if printf '%s' "$rpc_resp" | rg -q '"ok"\s*:\s*true'; then
      rpc_ok=true
    fi
  fi

  status="pass"
  note=""
  if [[ "$health_code" != "200" || "$ready_code" != "200" || "$api_code" != "200" || "$rpc_ok" != "true" ]]; then
    status="fail"
    note="health=$health_code ready=$ready_code api=$api_code rpc_ok=$rpc_ok"
  fi

  if [[ "$status" == "pass" ]]; then
    passes=$((passes + 1))
  else
    fails=$((fails + 1))
  fi

  log_event "$ts" "$health_code" "$ready_code" "$api_code" "$rpc_ok" "$status" "$note"
}

restart_postgres() {
  eval "$compose_cmd restart postgres" >/dev/null
}

restart_backend() {
  eval "$compose_cmd restart ploy-backend" >/dev/null
}

network_blip_backend() {
  docker pause ploy-backend >/dev/null
  sleep 15
  docker unpause ploy-backend >/dev/null
}

echo "soak test started"
echo "duration=${duration_secs}s interval=${interval_secs}s base_url=${base_url}"
echo "log_file=$log_file"

while [[ "$(date +%s)" -lt "$end_epoch" ]]; do
  now_epoch="$(date +%s)"
  elapsed="$((now_epoch - start_epoch))"

  if [[ "$inject_db_restart" == "true" && "$db_restarted" == "false" && "$elapsed" -ge "$((duration_secs / 3))" ]]; then
    restart_postgres
    db_restarted=true
    log_event "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "n/a" "n/a" "n/a" false "info" "injected:postgres_restart"
  fi

  if [[ "$inject_network_blip" == "true" && "$network_blipped" == "false" && "$elapsed" -ge "$((duration_secs / 2))" ]]; then
    network_blip_backend
    network_blipped=true
    log_event "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "n/a" "n/a" "n/a" false "info" "injected:backend_pause_unpause"
  fi

  if [[ "$inject_process_restart" == "true" && "$proc_restarted" == "false" && "$elapsed" -ge "$((duration_secs * 2 / 3))" ]]; then
    restart_backend
    proc_restarted=true
    log_event "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "n/a" "n/a" "n/a" false "info" "injected:backend_restart"
  fi

  run_probe
  sleep "$interval_secs"
done

echo "soak test completed"
echo "passes=$passes fails=$fails log_file=$log_file"

if [[ "$fails" -gt 0 ]]; then
  exit 1
fi
