#!/usr/bin/env bash
set -euo pipefail

# Install/update the auto-claimer systemd unit on the trading host via AWS SSM.
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
    awk -F '"' '/^INSTANCE_ID=/{print $2; exit}' "$deploy" 2>/dev/null || true
    return
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

export SERVICE_FILE
CMD_JSON="$(
  python3 - <<'PY'
import base64
import json
import os
from pathlib import Path

service_b64 = base64.b64encode(Path(os.environ["SERVICE_FILE"]).read_bytes()).decode("ascii")

cmds = [
    "set -euo pipefail",
    # Install the systemd unit
    "printf '%s' '" + service_b64 + "' | base64 -d > /etc/systemd/system/ploy-claimer.service",
    # Ensure env dir exists (optional override file)
    "mkdir -p /opt/ploy/env",
    "chown -R ploy:ploy /opt/ploy/env",
    "chmod 700 /opt/ploy/env",
    "if [ ! -f /opt/ploy/env/claimer.env ]; then install -o ploy -g ploy -m 600 /dev/null /opt/ploy/env/claimer.env; fi",
    # Enable + start (only when key exists)
    "systemctl daemon-reload",
    "systemctl enable ploy-claimer.service",
    (
        "if grep -Eq '^(POLYMARKET_PRIVATE_KEY|PRIVATE_KEY)=' /opt/ploy/.env 2>/dev/null || "
        "grep -Eq '^(POLYMARKET_PRIVATE_KEY|PRIVATE_KEY)=' /opt/ploy/env/claimer.env 2>/dev/null; then "
        "systemctl restart ploy-claimer.service; "
        "systemctl --no-pager --full status ploy-claimer.service | head -n 25; "
        "else echo 'ploy-claimer installed but NOT started (missing POLYMARKET_PRIVATE_KEY/PRIVATE_KEY).'; fi"
    ),
]

print(json.dumps(cmds))
PY
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

