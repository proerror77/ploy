#!/usr/bin/env bash
set -euo pipefail

# Build and deploy ploy to an AWS EC2 host.
# This script uploads source code and builds on EC2 to avoid local/remote arch mismatch.

HOST=""
USER_NAME="ubuntu"
SSH_KEY=""
START_AFTER_DEPLOY="true"
ENABLE_ON_BOOT="true"
SERVICES="ploy-sports-pm,ploy-crypto-dryrun,ploy-maintenance.timer"

usage() {
  cat <<'USAGE'
Usage:
  scripts/aws_ec2_deploy.sh --host <ec2-host-or-ip> [options]

Options:
  --host <host>            EC2 public IP / hostname (required)
  --user <user>            SSH user (default: ubuntu)
  --key <path>             SSH private key path
  --start <true|false>     Start services after deploy (default: true)
  --enable <true|false>    Enable services on boot (default: true)
  --services <csv>         Services to enable/start
                           allowed: ploy,ploy-sports-pm,ploy-crypto-dryrun,ploy-maintenance.timer
                           default: ploy-sports-pm,ploy-crypto-dryrun,ploy-maintenance.timer
  -h, --help               Show help

Examples:
  scripts/aws_ec2_deploy.sh --host 1.2.3.4 --key ~/.ssh/my-ec2.pem
  scripts/aws_ec2_deploy.sh --host 1.2.3.4 --services ploy
  scripts/aws_ec2_deploy.sh --host 1.2.3.4 --services ploy-sports-pm,ploy-crypto-dryrun

What it does:
  1) Upload source bundle to /tmp on EC2
  2) Install build/runtime dependencies (apt)
  3) Install Rust (if missing) and build release binary
  4) Install/refresh systemd services (ploy, ploy@, sports, crypto dry-run)
  5) Install workload config/env templates under /opt/ploy/config and /opt/ploy/env
  6) Enable/start selected services (optional)
USAGE
}

is_allowed_service() {
  case "$1" in
    ploy|ploy-sports-pm|ploy-crypto-dryrun|ploy-maintenance.timer) return 0 ;;
    *) return 1 ;;
  esac
}

normalize_services_csv() {
  local csv="$1"
  local out=()
  IFS=',' read -r -a raw <<<"$csv"
  for item in "${raw[@]}"; do
    local svc
    svc="$(printf '%s' "$item" | xargs)"
    [[ -n "$svc" ]] || continue
    if ! is_allowed_service "$svc"; then
      echo "invalid service in --services: $svc" >&2
      echo "allowed: ploy, ploy-sports-pm, ploy-crypto-dryrun, ploy-maintenance.timer" >&2
      exit 2
    fi
    out+=("$svc")
  done

  if [[ ${#out[@]} -eq 0 ]]; then
    echo "--services must include at least one valid service" >&2
    exit 2
  fi

  (IFS=','; printf '%s' "${out[*]}")
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --host)
      HOST="${2:-}"
      shift 2
      ;;
    --user)
      USER_NAME="${2:-}"
      shift 2
      ;;
    --key)
      SSH_KEY="${2:-}"
      shift 2
      ;;
    --start)
      START_AFTER_DEPLOY="${2:-}"
      shift 2
      ;;
    --enable)
      ENABLE_ON_BOOT="${2:-}"
      shift 2
      ;;
    --services)
      SERVICES="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if [[ -z "$HOST" ]]; then
  echo "--host is required" >&2
  usage
  exit 2
fi

if [[ "$START_AFTER_DEPLOY" != "true" && "$START_AFTER_DEPLOY" != "false" ]]; then
  echo "--start must be true|false" >&2
  exit 2
fi

if [[ "$ENABLE_ON_BOOT" != "true" && "$ENABLE_ON_BOOT" != "false" ]]; then
  echo "--enable must be true|false" >&2
  exit 2
fi

SERVICES="$(normalize_services_csv "$SERVICES")"

SSH_OPTS=(
  -o StrictHostKeyChecking=accept-new
  -o ServerAliveInterval=30
  -o ServerAliveCountMax=20
)

if [[ -n "$SSH_KEY" ]]; then
  SSH_OPTS+=(-i "$SSH_KEY")
fi

SSH_TARGET="${USER_NAME}@${HOST}"
BUNDLE="/tmp/ploy-ec2-deploy-$(date +%Y%m%d-%H%M%S).tar.gz"
REMOTE_BUNDLE="/tmp/ploy-ec2-deploy.tar.gz"

echo "==> Creating deploy bundle: ${BUNDLE}"
COPYFILE_DISABLE=1 tar czf "$BUNDLE" \
  --exclude='.git' \
  --exclude='._*' \
  --exclude='*/._*' \
  --exclude='target' \
  --exclude='data' \
  --exclude='results' \
  --exclude='ploy-frontend/node_modules' \
  Cargo.toml Cargo.lock src config migrations scripts deployment

echo "==> Uploading bundle to ${SSH_TARGET}:${REMOTE_BUNDLE}"
scp "${SSH_OPTS[@]}" "$BUNDLE" "$SSH_TARGET:$REMOTE_BUNDLE"
rm -f "$BUNDLE"

echo "==> Deploying on EC2 (${SSH_TARGET})"
ssh "${SSH_OPTS[@]}" "$SSH_TARGET" \
  "START_AFTER_DEPLOY='${START_AFTER_DEPLOY}' ENABLE_ON_BOOT='${ENABLE_ON_BOOT}' SERVICES='${SERVICES}' bash -s" <<'REMOTE_EOF'
set -euo pipefail

REMOTE_ROOT="/opt/ploy"
REMOTE_BUNDLE="/tmp/ploy-ec2-deploy.tar.gz"

if [[ ! -f "$REMOTE_BUNDLE" ]]; then
  echo "missing upload bundle: $REMOTE_BUNDLE" >&2
  exit 2
fi

if command -v apt-get >/dev/null 2>&1; then
  sudo apt-get update -qq
  sudo apt-get install -y \
    ca-certificates \
    curl \
    build-essential \
    pkg-config \
    libssl-dev \
    libssl3 \
    libpq-dev \
    libpq5
elif command -v dnf >/dev/null 2>&1; then
  sudo dnf install -y \
    ca-certificates \
    gcc \
    gcc-c++ \
    make \
    pkgconf-pkg-config \
    openssl-devel
  if ! command -v curl >/dev/null 2>&1; then
    sudo dnf install -y curl-minimal || sudo dnf install -y curl --allowerasing
  fi
  if ! command -v pg_config >/dev/null 2>&1; then
    sudo dnf install -y postgresql15-private-devel || sudo dnf install -y libpq-devel --allowerasing
  fi
elif command -v yum >/dev/null 2>&1; then
  sudo yum install -y \
    ca-certificates \
    gcc \
    gcc-c++ \
    make \
    pkgconfig \
    openssl-devel
  if ! command -v curl >/dev/null 2>&1; then
    sudo yum install -y curl-minimal || sudo yum install -y curl
  fi
  if ! command -v pg_config >/dev/null 2>&1; then
    sudo yum install -y postgresql15-private-devel || sudo yum install -y libpq-devel
  fi
else
  echo "no supported package manager found (apt-get/dnf/yum)" >&2
  exit 2
fi

if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi
source "$HOME/.cargo/env"

sudo mkdir -p "$REMOTE_ROOT"
sudo chown -R "$USER":"$USER" "$REMOTE_ROOT"
tar xzf "$REMOTE_BUNDLE" -C "$REMOTE_ROOT"
rm -f "$REMOTE_BUNDLE"
find "$REMOTE_ROOT" -type f -name '._*' -delete 2>/dev/null || true

cd "$REMOTE_ROOT"
# Build only the main binary. The workspace may contain experimental bins
# that are not required for production services.
#
# NOTE: This host is intentionally small; release LTO + single codegen unit
# can OOM and stall deploys. Prefer faster/lower-memory release settings.
CARGO_PROFILE_RELEASE_LTO=off \
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
  CARGO_BUILD_JOBS=2 \
  cargo build --release --bin ploy

if ! id -u ploy >/dev/null 2>&1; then
  sudo useradd --system --home "$REMOTE_ROOT" --shell /usr/sbin/nologin --no-create-home ploy
fi

sudo mkdir -p "$REMOTE_ROOT"/{bin,config,env,data,logs}

# Install runtime binary to a stable path. This decouples services from the
# (large) Cargo target directory, which can be cleaned to keep the disk healthy.
sudo install -o ploy -g ploy -m 0755 "$REMOTE_ROOT/target/release/ploy" "$REMOTE_ROOT/bin/ploy"

# Keep disk usage bounded on small hosts.
sudo rm -rf "$REMOTE_ROOT/target"
sudo chown -R ploy:ploy "$REMOTE_ROOT"

# Install service units
for unit in \
  "$REMOTE_ROOT/deployment/ploy.service" \
  "$REMOTE_ROOT/deployment/ploy@.service" \
  "$REMOTE_ROOT/deployment/ploy-sports-pm.service" \
  "$REMOTE_ROOT/deployment/ploy-crypto-dryrun.service" \
  "$REMOTE_ROOT/deployment/ploy-maintenance.service" \
  "$REMOTE_ROOT/deployment/ploy-maintenance.timer"
do
  if [[ -f "$unit" ]]; then
    sudo install -m 0644 "$unit" "/etc/systemd/system/$(basename "$unit")"
  fi
done

# Install workload config templates (do not overwrite local edits)
if [[ -f "$REMOTE_ROOT/deployment/config/sports_pm.toml" && ! -f "$REMOTE_ROOT/config/sports_pm.toml" ]]; then
  sudo cp "$REMOTE_ROOT/deployment/config/sports_pm.toml" "$REMOTE_ROOT/config/sports_pm.toml"
fi
if [[ -f "$REMOTE_ROOT/deployment/config/crypto_dry_run.toml" && ! -f "$REMOTE_ROOT/config/crypto_dry_run.toml" ]]; then
  sudo cp "$REMOTE_ROOT/deployment/config/crypto_dry_run.toml" "$REMOTE_ROOT/config/crypto_dry_run.toml"
fi

# Install env templates (do not overwrite local edits)
if [[ -f "$REMOTE_ROOT/deployment/env.example" && ! -f "$REMOTE_ROOT/.env" ]]; then
  sudo cp "$REMOTE_ROOT/deployment/env.example" "$REMOTE_ROOT/.env"
fi
if [[ -f "$REMOTE_ROOT/deployment/env.sports-pm.example" && ! -f "$REMOTE_ROOT/env/sports-pm.env" ]]; then
  sudo cp "$REMOTE_ROOT/deployment/env.sports-pm.example" "$REMOTE_ROOT/env/sports-pm.env"
fi
if [[ -f "$REMOTE_ROOT/deployment/env.crypto-dryrun.example" && ! -f "$REMOTE_ROOT/env/crypto-dryrun.env" ]]; then
  sudo cp "$REMOTE_ROOT/deployment/env.crypto-dryrun.example" "$REMOTE_ROOT/env/crypto-dryrun.env"
fi

# Keep crypto/sports DB target aligned by default (avoid split persistence DBs).
if [[ -f "$REMOTE_ROOT/env/sports-pm.env" && -f "$REMOTE_ROOT/env/crypto-dryrun.env" ]]; then
  sports_db_line="$(sudo grep -E '^PLOY_DATABASE__URL=' "$REMOTE_ROOT/env/sports-pm.env" | tail -n 1 || true)"
  if [[ -n "$sports_db_line" ]] && ! sudo grep -qE '^PLOY_DATABASE__URL=' "$REMOTE_ROOT/env/crypto-dryrun.env"; then
    echo "" | sudo tee -a "$REMOTE_ROOT/env/crypto-dryrun.env" >/dev/null
    echo "# Auto-copied from sports-pm.env during deploy to keep shared DB" | sudo tee -a "$REMOTE_ROOT/env/crypto-dryrun.env" >/dev/null
    echo "$sports_db_line" | sudo tee -a "$REMOTE_ROOT/env/crypto-dryrun.env" >/dev/null
  fi
fi

sudo chmod 600 "$REMOTE_ROOT"/.env "$REMOTE_ROOT"/env/*.env 2>/dev/null || true
sudo chown ploy:ploy \
  "$REMOTE_ROOT"/config/*.toml \
  "$REMOTE_ROOT"/.env \
  "$REMOTE_ROOT"/env/*.env 2>/dev/null || true

sudo systemctl daemon-reload

IFS=',' read -r -a SERVICE_LIST <<<"${SERVICES:-}"
for svc in "${SERVICE_LIST[@]}"; do
  svc="$(printf '%s' "$svc" | xargs)"
  [[ -n "$svc" ]] || continue

  if [[ "${ENABLE_ON_BOOT}" == "true" ]]; then
    sudo systemctl enable "$svc"
  fi

  if [[ "${START_AFTER_DEPLOY}" == "true" ]]; then
    sudo systemctl restart "$svc"
    sudo systemctl --no-pager --full status "$svc" || true
  fi
done

if [[ "${START_AFTER_DEPLOY}" != "true" ]]; then
  echo "service start skipped (--start false)"
fi

echo "==> Remote deploy completed"
REMOTE_EOF

echo
echo "Deploy complete."
echo "Selected services: ${SERVICES}"
echo "Next steps on EC2:"
echo "  1) Edit /opt/ploy/.env and /opt/ploy/env/*.env"
echo "  2) Set /opt/ploy/env/sports-pm.env and /opt/ploy/env/crypto-dryrun.env PLOY_DATABASE__URL (same DB recommended)"
echo "  3) Seed NBA stats: /opt/ploy/bin/ploy --config /opt/ploy/config/sports_pm.toml strategy nba-seed-stats --season 2025-26"
echo "  4) Check logs:"
echo "     sudo journalctl -u ploy-sports-pm -f"
echo "     sudo journalctl -u ploy-crypto-dryrun -f"
