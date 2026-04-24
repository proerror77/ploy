#!/usr/bin/env bash
set -euo pipefail

HOST_ROOT="/opt/ploy"
ADDR="http://127.0.0.1:8081"
MANIFEST=""
DRYRUN_CONFIG=""
LIVE_CONFIG=""
DEPLOYMENT_ID=""
GO_LIVE=0
RUN_DRY_RUN_DRILL=1
WARNINGS=0

usage() {
  cat <<'EOF'
Usage: pm5d_threelayer_live_gate.sh [--go-live] [--host-root /opt/ploy] [--addr http://127.0.0.1:8081]

Prepares the PM5D ThreeLayer live deployment on a Tango host.

Default behavior:
  - verify daemon, env, manifest, and live/dry-run config parity
  - run the existing paper live-dry-run drill when available
  - apply pm5d.threelayer.live with desired_state=paused
  - stop before placing any live orders

With --go-live:
  - perform the same checks
  - apply the paused live manifest
  - resume pm5d.threelayer.live

The script never edits /opt/ploy/.env and never changes strategy parameters.
EOF
}

log_step() {
  printf '\n[%s] %s\n' "step" "$1"
}

note() {
  printf '[info] %s\n' "$1"
}

warn() {
  printf '[warn] %s\n' "$1"
  WARNINGS=$((WARNINGS + 1))
}

fail() {
  printf '[fail] %s\n' "$1" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

ployctl_capture() {
  "$PLOYCTL" "$@" 2>&1
}

strip_blank_lines() {
  printf '%s\n' "$1" | tr -d '\r' | sed '/^[[:space:]]*$/d'
}

env_has_key() {
  local key="$1"
  grep -Eq "^[[:space:]]*${key}=" "$ENV_FILE"
}

env_has_any_key() {
  local key
  for key in "$@"; do
    if env_has_key "$key"; then
      return 0
    fi
  done
  return 1
}

require_env_key() {
  local key="$1"
  env_has_key "$key" || fail "required env key missing from ${ENV_FILE}: ${key}"
}

require_any_env_key() {
  local label="$1"
  shift
  env_has_any_key "$@" || fail "required ${label} missing from ${ENV_FILE}: one of [$*]"
}

first_env_value() {
  local key
  for key in "$@"; do
    if env_has_key "$key"; then
      grep -E "^[[:space:]]*${key}=" "$ENV_FILE" | tail -n 1 | cut -d= -f2- | tr -d '[:space:]'
      return 0
    fi
  done
  return 1
}

source_env_file() {
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
}

extract_manifest_field() {
  local field="$1"
  python3 - "$MANIFEST" "$field" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    value = json.load(handle)[sys.argv[2]]
print(value)
PY
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --go-live)
      GO_LIVE=1
      shift
      ;;
    --host-root)
      HOST_ROOT="$2"
      shift 2
      ;;
    --addr)
      ADDR="$2"
      shift 2
      ;;
    --manifest)
      MANIFEST="$2"
      shift 2
      ;;
    --dryrun-config)
      DRYRUN_CONFIG="$2"
      shift 2
      ;;
    --live-config)
      LIVE_CONFIG="$2"
      shift 2
      ;;
    --skip-dry-run-drill)
      RUN_DRY_RUN_DRILL=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

PLOYCTL="${HOST_ROOT}/bin/ployctl"
ENV_FILE="${HOST_ROOT}/.env"
MANIFEST="${MANIFEST:-${HOST_ROOT}/config/deployments/pm5d.threelayer.live.json}"
DRYRUN_CONFIG="${DRYRUN_CONFIG:-${HOST_ROOT}/config/strategies/02-pm5d-threelayer.unified.toml}"
LIVE_CONFIG="${LIVE_CONFIG:-${HOST_ROOT}/config/strategies/02-pm5d-threelayer.live.toml}"

require_command systemctl
require_command curl
require_command python3
[[ -x "$PLOYCTL" ]] || fail "missing ployctl binary: $PLOYCTL"
[[ -f "$ENV_FILE" ]] || fail "missing env file: $ENV_FILE"
[[ -f "$MANIFEST" ]] || fail "missing live manifest: $MANIFEST"
[[ -f "$DRYRUN_CONFIG" ]] || fail "missing dry-run config: $DRYRUN_CONFIG"
[[ -f "$LIVE_CONFIG" ]] || fail "missing live config: $LIVE_CONFIG"

DEPLOYMENT_ID="${DEPLOYMENT_ID:-$(extract_manifest_field deployment_id)}"
[[ -n "$DEPLOYMENT_ID" ]] || fail "deployment id could not be determined"

source_env_file

log_step "daemon baseline"
[[ "$(systemctl is-active ployd)" == "active" ]] || fail "ployd.service is not active"
curl -fsS "${ADDR}/health" >/dev/null
STATUS_OUTPUT="$(ployctl_capture system status)"
ALERTS_OUTPUT="$(ployctl_capture system alerts)"
TRADING_OUTPUT="$(ployctl_capture trading status)"
printf '%s\n' "$STATUS_OUTPUT"
printf '%s\n' "$ALERTS_OUTPUT"
printf '%s\n' "$TRADING_OUTPUT"

ALERTS_NORMALIZED="$(strip_blank_lines "$ALERTS_OUTPUT")"
if [[ "$ALERTS_NORMALIZED" != "none" ]]; then
  if printf '%s\n' "$ALERTS_NORMALIZED" | grep -q ' critical '; then
    fail "critical alerts are active"
  fi
  warn "non-critical alerts are active"
fi

log_step "credential presence"
require_any_env_key "Polymarket private key" "POLYMARKET_PRIVATE_KEY" "PRIVATE_KEY"
SIGNATURE_TYPE="$(first_env_value "POLY_SIGNATURE_TYPE" "POLYMARKET_SIGNATURE_TYPE" || true)"
FUNDER_PRESENT=0
if env_has_any_key "POLY_FUNDER" "POLYMARKET_FUNDER" "POLYMARKET_FUNDER_ADDRESS"; then
  FUNDER_PRESENT=1
fi
case "$SIGNATURE_TYPE" in
  proxy|gnosis_safe)
    require_any_env_key "proxy funder" "POLY_FUNDER" "POLYMARKET_FUNDER" "POLYMARKET_FUNDER_ADDRESS"
    ;;
  eoa)
    ;;
  "")
    if [[ "$FUNDER_PRESENT" -eq 1 ]]; then
      warn "POLY_SIGNATURE_TYPE is not explicit; current SDK defaults to proxy when a funder is present"
    else
      warn "POLY_SIGNATURE_TYPE and funder are absent; current SDK defaults to eoa"
    fi
    ;;
  *)
    fail "unsupported POLY_SIGNATURE_TYPE: $SIGNATURE_TYPE"
    ;;
esac

log_step "manifest and config parity"
python3 - "$MANIFEST" "$DRYRUN_CONFIG" "$LIVE_CONFIG" "$GO_LIVE" <<'PY'
import json
import pathlib
import re
import sys

manifest_path = pathlib.Path(sys.argv[1])
dryrun_path = pathlib.Path(sys.argv[2])
live_path = pathlib.Path(sys.argv[3])
go_live = sys.argv[4] == "1"

manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
if manifest.get("deployment_id") != "pm5d.threelayer.live":
    raise SystemExit("manifest deployment_id must be pm5d.threelayer.live")
if manifest.get("bundle_id") != "02-pm5d-threelayer.live":
    raise SystemExit("manifest bundle_id must be 02-pm5d-threelayer.live")
if manifest.get("runtime_mode") != "live":
    raise SystemExit("manifest runtime_mode must be live")
if manifest.get("desired_state") != "paused":
    raise SystemExit("manifest desired_state must stay paused; --go-live resumes explicitly")

dryrun = dryrun_path.read_text(encoding="utf-8")
live = live_path.read_text(encoding="utf-8")
if not re.search(r'(?m)^mode = "dryrun"$', dryrun):
    raise SystemExit("dry-run config must contain mode = \"dryrun\"")
if not re.search(r'(?m)^mode = "live"$', live):
    raise SystemExit("live config must contain mode = \"live\"")

def material_config(text):
    return "\n".join(
        line.rstrip()
        for line in text.splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    )

normalized = re.sub(r'(?m)^mode = "dryrun"$', 'mode = "live"', dryrun, count=1)
if material_config(normalized) != material_config(live):
    raise SystemExit("live config must match dry-run config exactly except [runtime].mode")

if go_live:
    print("manifest/config gate: go-live requested")
else:
    print("manifest/config gate: paused apply only")
PY

if [[ "$RUN_DRY_RUN_DRILL" -eq 1 && -x "${HOST_ROOT}/scripts/drills/live_dry_run.sh" ]]; then
  log_step "paper live-host drill"
  "${HOST_ROOT}/scripts/drills/live_dry_run.sh"
elif [[ "$RUN_DRY_RUN_DRILL" -eq 1 ]]; then
  warn "paper drill script not found or not executable; skipping ${HOST_ROOT}/scripts/drills/live_dry_run.sh"
fi

log_step "apply paused live deployment"
APPLY_OUTPUT="$(ployctl_capture deployments apply "$MANIFEST")"
printf '%s\n' "$APPLY_OUTPUT"
INSPECT_OUTPUT="$(ployctl_capture deployments inspect "$DEPLOYMENT_ID")"
printf '%s\n' "$INSPECT_OUTPUT"
case "$INSPECT_OUTPUT" in
  *"desired=Paused"*) ;;
  *) fail "live deployment was not applied as paused" ;;
esac

if [[ "$GO_LIVE" -ne 1 ]]; then
  log_step "result"
  if [[ "$WARNINGS" -gt 0 ]]; then
    printf 'READY-WITH-WARNINGS: %s is staged paused; review warnings before --go-live.\n' "$DEPLOYMENT_ID"
  else
    printf 'READY: %s is staged paused. Run with --go-live only after manual live approval.\n' "$DEPLOYMENT_ID"
  fi
  exit 0
fi

log_step "resume live deployment"
RESUME_OUTPUT="$(ployctl_capture deployments resume "$DEPLOYMENT_ID")"
printf '%s\n' "$RESUME_OUTPUT"
FINAL_INSPECT="$(ployctl_capture deployments inspect "$DEPLOYMENT_ID")"
printf '%s\n' "$FINAL_INSPECT"
case "$FINAL_INSPECT" in
  *"desired=Running"*) ;;
  *) fail "live deployment did not enter desired=Running" ;;
esac

log_step "result"
printf 'LIVE: %s resume command accepted. Watch ployctl trading status and worker logs immediately.\n' "$DEPLOYMENT_ID"
