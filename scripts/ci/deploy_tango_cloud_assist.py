#!/usr/bin/env python3
"""Deploy the tango bundle through Aliyun Cloud Assistant.

This is a transport fallback for deploy-tango-1-1.yml when public SSH reaches
the instance but cannot complete banner exchange. The bundle is still built by
GitHub Actions and uploaded to OSS; Cloud Assistant only runs the same remote
install/restart/postflight sequence on the ECS instance.
"""

from __future__ import annotations

import base64
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time
import tempfile


def require_env(name: str) -> str:
    value = os.environ.get(name, "")
    if not value:
        raise SystemExit(f"missing required environment variable: {name}")
    return value


def run_text(args: list[str]) -> str:
    completed = subprocess.run(args, text=True, capture_output=True)
    if completed.returncode != 0:
        if completed.stdout:
            print(completed.stdout, file=sys.stderr)
        if completed.stderr:
            print(completed.stderr, file=sys.stderr)
        raise subprocess.CalledProcessError(
            completed.returncode,
            args[0],
            output=completed.stdout,
            stderr=completed.stderr,
        )
    return completed.stdout


def run_json(args: list[str]) -> dict:
    completed = run_text(args)
    return json.loads(completed)


def shell_quote(value: str) -> str:
    return shlex.quote(value)


def remote_script() -> str:
    deploy_root = require_env("DEPLOY_ROOT")
    bundle_name = require_env("DEPLOY_BUNDLE_NAME")
    github_sha = require_env("GITHUB_SHA")
    oss_region = require_env("DEPLOY_OSS_REGION")
    oss_bucket = require_env("ALIYUN_OSS_BUCKET")
    oss_prefix = require_env("DEPLOY_OSS_PREFIX")
    oss_key_id = require_env("ALIYUN_OSS_ACCESS_KEY_ID")
    oss_key_secret = require_env("ALIYUN_OSS_ACCESS_KEY_SECRET")
    migrations = os.environ.get("DEPLOY_MIGRATIONS", "")

    return f"""#!/usr/bin/env bash
set -euo pipefail

DEPLOY_ROOT={shell_quote(deploy_root)}
DEPLOY_BUNDLE_NAME={shell_quote(bundle_name)}
GITHUB_SHA={shell_quote(github_sha)}
DEPLOY_MIGRATIONS={shell_quote(migrations)}
DEPLOY_STARTED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
WORKDIR="${{DEPLOY_ROOT}}/tmp/deploy-${{GITHUB_SHA}}-cloud-assist"

require_postgres() {{
  systemctl is-active --quiet postgresql@16-main
  PGPASSWORD=postgres psql -h localhost -U postgres -d ploy -v ON_ERROR_STOP=1 -Atqc "SELECT 1" >/dev/null
}}

service_exists() {{
  systemctl cat "$1" >/dev/null 2>&1
}}

require_pm5d_live_paused() {{
  local live_status
  local live_line
  live_status="$("${{DEPLOY_ROOT}}/bin/ployctl" deployments inspect pm5d.threelayer.live 2>&1)"
  echo "${{live_status}}"
  live_line="$(printf '%s\n' "${{live_status}}" | awk '$1 == "pm5d.threelayer.live" {{ print; exit }}')"
  case "${{live_line}}" in
    *"desired=Paused"*"observed=Paused"*) ;;
    *)
      echo "pm5d.threelayer.live must remain desired=Paused observed=Paused after deploy" >&2
      exit 1
      ;;
  esac
}}

require_service_guardrails() {{
  local unit="$1"
  test "$(systemctl show -P Restart "${{unit}}")" = "always"
  test "$(systemctl show -P OOMPolicy "${{unit}}")" = "kill"
  test "$(systemctl show -P MemoryHigh "${{unit}}")" != "[not set]"
  test "$(systemctl show -P MemoryMax "${{unit}}")" != "[not set]"
}}

wait_for_recent_rows() {{
  local sql="$1"
  local message="$2"
  local attempts="${{3:-20}}"
  local sleep_secs="${{4:-3}}"
  local result
  for _ in $(seq 1 "${{attempts}}"); do
    result="$(PGPASSWORD=postgres psql -U postgres -d ploy -Atqc "${{sql}}")"
    if [ "${{result}}" = "t" ]; then
      return 0
    fi
    sleep "${{sleep_secs}}"
  done
  echo "${{message}}" >&2
  exit 1
}}

assert_no_recent_logs() {{
  local unit="$1"
  local pattern="$2"
  local message="$3"
  local since="${{DEPLOY_VERIFY_SINCE:-${{DEPLOY_STARTED_AT}}}}"
  if journalctl -u "${{unit}}" --since "${{since}}" --no-pager | grep -E "${{pattern}}"; then
    echo "${{message}}" >&2
    exit 1
  fi
}}

ensure_ossutil() {{
  export PATH="/usr/local/bin:/usr/bin:/bin:${{PATH}}"
  if command -v ossutil >/dev/null 2>&1; then
    return 0
  fi
  curl -fsSL https://gosspublic.alicdn.com/ossutil/install.sh | bash
  export PATH="/usr/local/bin:/usr/bin:/bin:${{PATH}}"
  command -v ossutil >/dev/null 2>&1
}}

mkdir -p "${{DEPLOY_ROOT}}"/{{bin,config/strategies,config/deployments,migrations,scripts,scripts/drills,tmp}}
rm -rf "${{WORKDIR}}"
mkdir -p "${{WORKDIR}}"

ensure_ossutil
export OSS_ACCESS_KEY_ID={shell_quote(oss_key_id)}
export OSS_ACCESS_KEY_SECRET={shell_quote(oss_key_secret)}
export OSS_REGION={shell_quote(oss_region)}
export OSS_ENDPOINT="https://oss-{oss_region}-internal.aliyuncs.com"
OSS_BUCKET={shell_quote(oss_bucket)}
OSS_OBJECT_PREFIX={shell_quote(f"{oss_prefix}/{github_sha}")}

ossutil cp "oss://${{OSS_BUCKET}}/${{OSS_OBJECT_PREFIX}}/${{DEPLOY_BUNDLE_NAME}}" "${{WORKDIR}}/${{DEPLOY_BUNDLE_NAME}}" \\
  -i "${{OSS_ACCESS_KEY_ID}}" -k "${{OSS_ACCESS_KEY_SECRET}}" -e "${{OSS_ENDPOINT}}"
ossutil cp "oss://${{OSS_BUCKET}}/${{OSS_OBJECT_PREFIX}}/${{DEPLOY_BUNDLE_NAME}}.sha256" "${{WORKDIR}}/${{DEPLOY_BUNDLE_NAME}}.sha256" \\
  -i "${{OSS_ACCESS_KEY_ID}}" -k "${{OSS_ACCESS_KEY_SECRET}}" -e "${{OSS_ENDPOINT}}"

cd "${{WORKDIR}}"
sha256sum -c "${{DEPLOY_BUNDLE_NAME}}.sha256"
tar -xzf "${{DEPLOY_BUNDLE_NAME}}"

# Stop DB-writing collectors before additive migrations so stale idle
# transactions cannot block CREATE INDEX / partition repair statements.
for unit in \\
  ploy-binance-aggtrade-collector.service \\
  ploy-binance-lob-collector.service \\
  ploy-binance-price-collector.service \\
  ploy-deribit-iv-collector.service \\
  ploy-deribit-greeks-collector.service \\
  ploy-market-discovery.service \\
  ploy-quote-collector.service \\
  ploy-pm-trade-collector.service; do
  if service_exists "${{unit}}"; then
    systemctl stop "${{unit}}" || true
  fi
done

install -m 0755 ./bin/ployd "${{DEPLOY_ROOT}}/bin/ployd"
install -m 0755 ./bin/ploy-runner "${{DEPLOY_ROOT}}/bin/ploy-runner"
install -m 0755 ./bin/factor-research "${{DEPLOY_ROOT}}/bin/factor-research"
install -m 0755 ./bin/optimize-backtest "${{DEPLOY_ROOT}}/bin/optimize-backtest"
install -m 0644 ./config/strategies/*.toml "${{DEPLOY_ROOT}}/config/strategies/"
install -m 0644 ./config/deployments/*.json "${{DEPLOY_ROOT}}/config/deployments/"
install -m 0755 ./scripts/*.py "${{DEPLOY_ROOT}}/scripts/"
install -m 0644 ./scripts/*.sql "${{DEPLOY_ROOT}}/scripts/"
install -m 0755 ./scripts/drills/*.sh "${{DEPLOY_ROOT}}/scripts/drills/"
for migration in ${{DEPLOY_MIGRATIONS}}; do
  migration="$(printf '%s' "${{migration}}" | xargs)"
  [ -n "${{migration}}" ] || continue
  install -m 0644 "./migrations/${{migration}}" "${{DEPLOY_ROOT}}/migrations/${{migration}}"
done

install -d /etc/systemd/system/ploy-quote-collector.service.d
install -m 0644 ./deployment/systemd/ploy-binance-aggtrade-collector.service /etc/systemd/system/ploy-binance-aggtrade-collector.service
install -m 0644 ./deployment/systemd/ploy-binance-lob-collector.service /etc/systemd/system/ploy-binance-lob-collector.service
install -m 0644 ./deployment/systemd/ploy-deribit-greeks-collector.service /etc/systemd/system/ploy-deribit-greeks-collector.service
install -m 0644 ./deployment/systemd/ploy-market-discovery.service /etc/systemd/system/ploy-market-discovery.service
install -m 0644 ./deployment/systemd/ploy-pm-trade-collector.service /etc/systemd/system/ploy-pm-trade-collector.service
install -m 0644 ./deployment/systemd/ploy-polymarket-v2-indexer-import.service /etc/systemd/system/ploy-polymarket-v2-indexer-import.service
install -m 0644 ./deployment/systemd/ploy-polymarket-v2-indexer-import.timer /etc/systemd/system/ploy-polymarket-v2-indexer-import.timer
install -m 0644 ./deployment/systemd/polymarket-v2-indexer.service /etc/systemd/system/polymarket-v2-indexer.service
install -m 0644 ./deployment/env.polymarket-v2-indexer.example "${{DEPLOY_ROOT}}/env.polymarket-v2-indexer.example"
install -m 0644 ./deployment/systemd/ploy-quote-collector.hardening.conf /etc/systemd/system/ploy-quote-collector.service.d/hardening.conf
systemctl daemon-reload
systemctl enable --now ploy-polymarket-v2-indexer-import.timer

for migration in ${{DEPLOY_MIGRATIONS}}; do
  migration="$(printf '%s' "${{migration}}" | xargs)"
  [ -n "${{migration}}" ] || continue
  migration="${{DEPLOY_ROOT}}/migrations/${{migration}}"
  if [ -f "${{migration}}" ]; then
    PGPASSWORD=postgres psql -U postgres -d ploy -v ON_ERROR_STOP=1 -f "${{migration}}"
  fi
done
PGPASSWORD=postgres psql -U postgres -d ploy -v ON_ERROR_STOP=1 -f "${{DEPLOY_ROOT}}/scripts/fix_binance_lob_partitions.sql"
PGPASSWORD=postgres psql -U postgres -d ploy -v ON_ERROR_STOP=1 -f "${{DEPLOY_ROOT}}/scripts/fix_deribit_partitions.sql"
PGPASSWORD=postgres psql -U postgres -d ploy -v ON_ERROR_STOP=1 -f "${{DEPLOY_ROOT}}/scripts/fix_clob_trade_partitions.sql"
require_postgres

systemctl enable --now ploy-binance-aggtrade-collector.service
systemctl restart ploy-binance-aggtrade-collector.service
systemctl enable --now ploy-binance-lob-collector.service
systemctl restart ploy-binance-lob-collector.service
if service_exists ploy-binance-price-collector.service; then
  systemctl restart ploy-binance-price-collector.service
fi
if service_exists ploy-deribit-iv-collector.service; then
  systemctl restart ploy-deribit-iv-collector.service
fi
systemctl enable --now ploy-deribit-greeks-collector.service
systemctl restart ploy-deribit-greeks-collector.service
systemctl enable --now ploy-market-discovery.service
systemctl restart ploy-market-discovery.service
if service_exists ploy-quote-collector.service; then
  systemctl enable --now ploy-quote-collector.service
  systemctl restart ploy-quote-collector.service
fi
systemctl enable --now ploy-pm-trade-collector.service
systemctl restart ploy-pm-trade-collector.service
if service_exists ployd.service; then
  systemctl restart ployd.service
fi

DEPLOY_VERIFY_SINCE="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
sleep 5
require_postgres
systemctl is-active --quiet ploy-binance-aggtrade-collector.service
systemctl is-active --quiet ploy-binance-lob-collector.service
require_service_guardrails ploy-binance-lob-collector.service
if service_exists ploy-quote-collector.service; then
  systemctl is-active --quiet ploy-quote-collector.service
  require_service_guardrails ploy-quote-collector.service
fi
systemctl is-active --quiet ploy-pm-trade-collector.service
require_service_guardrails ploy-pm-trade-collector.service
if service_exists ploy-binance-price-collector.service; then
  systemctl is-active --quiet ploy-binance-price-collector.service
fi
if service_exists ploy-deribit-iv-collector.service; then
  systemctl is-active --quiet ploy-deribit-iv-collector.service
fi
systemctl is-active --quiet ploy-deribit-greeks-collector.service
require_service_guardrails ploy-deribit-greeks-collector.service
systemctl is-active --quiet ploy-market-discovery.service
require_service_guardrails ploy-market-discovery.service
if service_exists ployd.service; then
  systemctl is-active --quiet ployd.service
  curl -fsS http://127.0.0.1:8081/health
  curl -fsS http://127.0.0.1:8081/api/reports/dry-run | "${{DEPLOY_ROOT}}/scripts/check_dryrun_report_contract.py"
  "${{DEPLOY_ROOT}}/bin/ployctl" deployments list
  require_pm5d_live_paused
fi

wait_for_recent_rows \\
  "SELECT EXISTS (SELECT 1 FROM binance_agg_trade_ticks WHERE trade_time >= NOW() - INTERVAL '10 minutes')" \\
  "binance_agg_trade_ticks is not fresh after deploy"
wait_for_recent_rows \\
  "SELECT EXISTS (SELECT 1 FROM binance_lob_ticks WHERE event_time >= NOW() - INTERVAL '10 minutes')" \\
  "binance_lob_ticks is not fresh after deploy"
wait_for_recent_rows \\
  "SELECT EXISTS (SELECT 1 FROM deribit_atm_greeks_ticks WHERE fetched_at >= NOW() - INTERVAL '15 minutes')" \\
  "deribit_atm_greeks_ticks is not fresh after deploy"
wait_for_recent_rows \\
  "SELECT EXISTS (SELECT 1 FROM pm_market_catalog WHERE market_family = 'crypto' AND strategy_symbol IS NOT NULL AND end_time >= NOW())" \\
  "pm_market_catalog has no active crypto markets after deploy"
wait_for_recent_rows \\
  "SELECT EXISTS (SELECT 1 FROM pm_market_metadata WHERE symbol IS NOT NULL AND end_time >= NOW())" \\
  "pm_market_metadata has no active crypto markets after deploy"
wait_for_recent_rows \\
  "SELECT EXISTS (SELECT 1 FROM clob_trade_ticks WHERE received_at >= NOW() - INTERVAL '5 minutes')" \\
  "clob_trade_ticks is not receiving PM trade prints after deploy"

if service_exists ployd.service; then
  assert_no_recent_logs \\
    "ployd.service" \\
    "DB connection failed|running without persistence|Signal recorder disabled" \\
    "ployd restarted in degraded persistence mode"
fi
assert_no_recent_logs \\
  "ploy-deribit-greeks-collector.service" \\
  "Traceback|psql failed|column \\"option_type\\" does not exist|column \\"strike\\" does not exist" \\
  "deribit greeks collector failed after deploy"
assert_no_recent_logs \\
  "ploy-market-discovery.service" \\
  "Failed to connect to database|panic|thread '.*' panicked" \\
  "market discovery collector failed after deploy"
assert_no_recent_logs \\
  "ploy-pm-trade-collector.service" \\
  "no partition of relation \\"clob_trade_ticks\\"" \\
  "pm trade collector still lacks active partitions"

systemctl status ploy-binance-aggtrade-collector.service --no-pager
systemctl status ploy-binance-lob-collector.service --no-pager
systemctl status ploy-deribit-greeks-collector.service --no-pager
systemctl status ploy-market-discovery.service --no-pager
systemctl status ploy-pm-trade-collector.service --no-pager
if service_exists ployd.service; then
  systemctl status ployd.service --no-pager
fi
"""


def bootstrap_script(script_object: str) -> str:
    deploy_root = require_env("DEPLOY_ROOT")
    github_sha = require_env("GITHUB_SHA")
    oss_region = require_env("DEPLOY_OSS_REGION")
    oss_bucket = require_env("ALIYUN_OSS_BUCKET")
    oss_key_id = require_env("ALIYUN_OSS_ACCESS_KEY_ID")
    oss_key_secret = require_env("ALIYUN_OSS_ACCESS_KEY_SECRET")

    return f"""#!/usr/bin/env bash
set -euo pipefail

DEPLOY_ROOT={shell_quote(deploy_root)}
GITHUB_SHA={shell_quote(github_sha)}
WORKDIR="${{DEPLOY_ROOT}}/tmp/deploy-${{GITHUB_SHA}}-cloud-assist-bootstrap"

ensure_ossutil() {{
  export PATH="/usr/local/bin:/usr/bin:/bin:${{PATH}}"
  if command -v ossutil >/dev/null 2>&1; then
    return 0
  fi
  curl -fsSL https://gosspublic.alicdn.com/ossutil/install.sh | bash
  export PATH="/usr/local/bin:/usr/bin:/bin:${{PATH}}"
  command -v ossutil >/dev/null 2>&1
}}

mkdir -p "${{WORKDIR}}"
ensure_ossutil
export OSS_ACCESS_KEY_ID={shell_quote(oss_key_id)}
export OSS_ACCESS_KEY_SECRET={shell_quote(oss_key_secret)}
export OSS_REGION={shell_quote(oss_region)}
export OSS_ENDPOINT="https://oss-{oss_region}-internal.aliyuncs.com"
OSS_BUCKET={shell_quote(oss_bucket)}
SCRIPT_OBJECT={shell_quote(script_object)}

ossutil cp "oss://${{OSS_BUCKET}}/${{SCRIPT_OBJECT}}" "${{WORKDIR}}/cloud-assist-deploy.sh" \\
  -i "${{OSS_ACCESS_KEY_ID}}" -k "${{OSS_ACCESS_KEY_SECRET}}" -e "${{OSS_ENDPOINT}}"
chmod 0700 "${{WORKDIR}}/cloud-assist-deploy.sh"
exec /bin/bash "${{WORKDIR}}/cloud-assist-deploy.sh"
"""


def upload_remote_script(script: str) -> str:
    region = require_env("DEPLOY_OSS_REGION")
    bucket = require_env("ALIYUN_OSS_BUCKET")
    prefix = require_env("DEPLOY_OSS_PREFIX")
    github_sha = require_env("GITHUB_SHA")
    key_id = require_env("ALIYUN_OSS_ACCESS_KEY_ID")
    key_secret = require_env("ALIYUN_OSS_ACCESS_KEY_SECRET")

    script_object = f"{prefix}/{github_sha}/cloud-assist-deploy.sh"
    script_path = Path(tempfile.gettempdir()) / f"ploy-cloud-assist-deploy-{github_sha[:12]}.sh"
    script_path.write_text(script, encoding="utf-8")
    run_text(
        [
            "ossutil",
            "cp",
            str(script_path),
            f"oss://{bucket}/{script_object}",
            "-i",
            key_id,
            "-k",
            key_secret,
            "-e",
            f"https://oss-{region}.aliyuncs.com",
        ]
    )
    return script_object


def main() -> int:
    region = require_env("DEPLOY_OSS_REGION")
    instance_id = require_env("TANGO_1_1_INSTANCE_ID")
    command_name = f"ploy-cloud-assist-deploy-{os.environ.get('GITHUB_SHA', 'unknown')[:12]}"
    script = remote_script()
    script_object = upload_remote_script(script)
    command_content = f"/bin/bash -lc {shell_quote(bootstrap_script(script_object))}"
    print(
        f"Uploaded Cloud Assistant deploy script oss://{require_env('ALIYUN_OSS_BUCKET')}/{script_object} "
        f"({len(script.encode('utf-8'))} bytes); bootstrap is {len(command_content.encode('utf-8'))} bytes"
    )

    run_result = run_json(
        [
            "aliyun",
            "ecs",
            "RunCommand",
            "--RegionId",
            region,
            "--Type",
            "RunShellScript",
            "--Name",
            command_name,
            "--Timeout",
            "1800",
            "--CommandContent",
            command_content,
            "--InstanceId.1",
            instance_id,
        ]
    )
    invoke_id = run_result["InvokeId"]
    print(f"Cloud Assistant invoke_id={invoke_id}")

    for _ in range(240):
        result = run_json(
            [
                "aliyun",
                "ecs",
                "DescribeInvocationResults",
                "--RegionId",
                region,
                "--InvokeId",
                invoke_id,
            ]
        )
        rows = result["Invocation"]["InvocationResults"]["InvocationResult"]
        if not rows:
            time.sleep(5)
            continue
        row = rows[0]
        status = row.get("InvocationStatus")
        exit_code = row.get("ExitCode")
        print(f"Cloud Assistant status={status} exit={exit_code}")
        if status in {"Pending", "Running"}:
            time.sleep(5)
            continue
        output = base64.b64decode(row.get("Output") or b"").decode("utf-8", "replace")
        print(output)
        return 0 if status == "Success" and exit_code == 0 else 1

    print("Cloud Assistant deploy did not finish before polling timeout", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
