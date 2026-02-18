#!/usr/bin/env bash
set -euo pipefail

# Installs/updates the auto-claimer systemd unit on the trading host via AWS SSM.
#
# Usage:
#   scripts/install_claimer_service_ssm.sh
#   PLOY_AWS_INSTANCE_ID=i-... scripts/install_claimer_service_ssm.sh
#
# Notes:
# - This does NOT print any secret values.
# - The service requires POLYMARKET_PRIVATE_KEY/PRIVATE_KEY on the host
#   (typically already in /opt/ploy/.env).

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

infer_instance_id() {
  if [[ -n "${PLOY_AWS_INSTANCE_ID:-}" ]]; then
    echo "$PLOY_AWS_INSTANCE_ID"
    return
  fi
  if [[ -n "${AWS_SSM_INSTANCE_ID:-}" ]]; then
    echo "$AWS_SSM_INSTANCE_ID"
    return
  fi
  local deploy="$ROOT_DIR/deploy_to_tango21.sh"
  if [[ -f "$deploy" ]]; then
    local id
    id="$(sed -n 's/^INSTANCE_ID=\"\\([^\"]\\+\\)\".*/\\1/p' "$deploy" | head -n 1 || true)"
    if [[ -n "$id" ]]; then
      echo "$id"
      return
    fi
  fi
  echo ""
}

INSTANCE_ID="$(infer_instance_id)"
if [[ -z "$INSTANCE_ID" ]]; then
  echo "ERROR: missing instance id. Set PLOY_AWS_INSTANCE_ID." >&2
  exit 2
fi

SERVICE_FILE="$ROOT_DIR/infra/systemd/ploy-claimer.service"
if [[ ! -f "$SERVICE_FILE" ]]; then
  echo "ERROR: missing $SERVICE_FILE" >&2
  exit 2
fi

SERVICE_CONTENT="$(cat "$SERVICE_FILE")"

CMD_JSON="$(
  cat <<EOF | python3 - <<'PY'
import json,sys
cmds = sys.stdin.read().splitlines()
print(json.dumps([c for c in cmds if c.strip() != ""]))
PY
set -euo pipefail

cat >/etc/systemd/system/ploy-claimer.service <<'UNIT'
$SERVICE_CONTENT
UNIT

mkdir -p /opt/ploy/env
chown -R ploy:ploy /opt/ploy/env
chmod 700 /opt/ploy/env

# If an override env file doesn't exist yet, create an empty one.
# The main /opt/ploy/.env typically already contains keys; this file is optional.
if [[ ! -f /opt/ploy/env/claimer.env ]]; then
  install -o ploy -g ploy -m 600 /dev/null /opt/ploy/env/claimer.env
fi

systemctl daemon-reload
systemctl enable ploy-claimer.service

# Only start if a key exists somewhere on the host.
if grep -Eq "^(POLYMARKET_PRIVATE_KEY|PRIVATE_KEY)=" /opt/ploy/.env 2>/dev/null || \
   grep -Eq "^(POLYMARKET_PRIVATE_KEY|PRIVATE_KEY)=" /opt/ploy/env/claimer.env 2>/dev/null; then
  systemctl restart ploy-claimer.service
  systemctl --no-pager --full status ploy-claimer.service | head -n 25
else
  echo "ploy-claimer installed but NOT started (missing POLYMARKET_PRIVATE_KEY/PRIVATE_KEY)."
fi
EOF
)"

COMMAND_ID="$(
  aws ssm send-command \
    --instance-ids "$INSTANCE_ID" \
    --document-name "AWS-RunShellScript" \
    --parameters "commands=$CMD_JSON" \
    --query 'Command.CommandId' \
    --output text
)"

aws ssm wait command-executed --command-id "$COMMAND_ID" --instance-id "$INSTANCE_ID"

aws ssm get-command-invocation \
  --command-id "$COMMAND_ID" \
  --instance-id "$INSTANCE_ID" \
  --query '{Status:Status,Out:StandardOutputContent,Err:StandardErrorContent}' \
  --output json
