# AWS EC2 Deployment Runbook

This runbook deploys always-on workloads on one EC2 host:
- `ploy-sports-pm.service` (Sports PM / NBA comeback)
- `ploy-crypto-collector.service` (Crypto event data collector: LOB + threshold + settlement)
- `ploy-crypto-dryrun.service` (Crypto trading agents dry-run)
- `ploy-orderbook-history.service` (Polymarket L2 orderbook-history backfill collector)
- `ploy-maintenance.timer` (DB + log retention)

## 1) Deploy to EC2

Run from your local machine:

```bash
scripts/aws_ec2_deploy.sh \
  --host <EC2_PUBLIC_IP> \
  --key ~/.ssh/<your-key>.pem \
  --services ploy-sports-pm,ploy-crypto-collector,ploy-orderbook-history,ploy-maintenance.timer
```

Optional:

```bash
scripts/aws_ec2_deploy.sh \
  --host <EC2_PUBLIC_IP> \
  --user ubuntu \
  --key ~/.ssh/<your-key>.pem \
  --start true \
  --enable true \
  --services ploy-sports-pm,ploy-crypto-collector,ploy-orderbook-history,ploy-maintenance.timer
```

What it installs:
- systemd units:
  - `ploy-sports-pm.service`
  - `ploy-crypto-collector.service`
  - `ploy-crypto-dryrun.service`
  - `ploy-orderbook-history.service`
  - `ploy-maintenance.service`
  - `ploy-maintenance.timer`
- config files:
  - `/opt/ploy/config/sports_pm.toml`
  - `/opt/ploy/config/crypto_dry_run.toml`
- env files:
  - `/opt/ploy/.env`
  - `/opt/ploy/env/sports-pm.env`
  - `/opt/ploy/env/crypto-collector.env`
  - `/opt/ploy/env/crypto-dryrun.env`

## 2) Configure Environment

On EC2:

```bash
sudoedit /opt/ploy/.env
sudoedit /opt/ploy/env/sports-pm.env
sudoedit /opt/ploy/env/crypto-collector.env
sudoedit /opt/ploy/env/crypto-dryrun.env
```

Minimum settings:

```env
# /opt/ploy/env/sports-pm.env
PLOY_DATABASE__URL=postgres://<user>:<pass>@<host>:5432/<db>
PLOY_DRY_RUN__ENABLED=true
```

```env
# /opt/ploy/env/crypto-dryrun.env
# Keep this equal to sports-pm.env to store PM/CLOB data in the same DB.
PLOY_DATABASE__URL=postgres://<user>:<pass>@<host>:5432/<db>
PLOY_DRY_RUN__ENABLED=true

# /opt/ploy/env/crypto-collector.env
# Collector should point to the same DB so settlement + quote data are unified.
PLOY_DATABASE__URL=postgres://<user>:<pass>@<host>:5432/<db>
PLOY_DRY_RUN__ENABLED=true
PM_SETTLEMENT_POLL_SECS=60
```

## 3) Sports PM DB Seed (Required)

Sports PM needs `nba_team_stats` data before signals are generated.

```bash
cd /opt/ploy
/opt/ploy/bin/ploy --config /opt/ploy/config/sports_pm.toml strategy nba-seed-stats --season 2025-26
```

## 4) Service Verification

```bash
sudo systemctl status ploy-sports-pm
sudo systemctl status ploy-crypto-collector
sudo systemctl status ploy-crypto-dryrun
sudo systemctl status ploy-orderbook-history
sudo systemctl status ploy-maintenance.timer
```

```bash
sudo journalctl -u ploy-sports-pm -f
sudo journalctl -u ploy-crypto-collector -f
sudo journalctl -u ploy-crypto-dryrun -f
sudo journalctl -u ploy-orderbook-history -f
sudo journalctl -u ploy-maintenance -n 200 --no-pager
```

## 5) OpenClaw Remote Control (SSH Forced Command)

Recommended `authorized_keys` entry:

```text
command="/opt/ploy/scripts/ssh_ployctl.sh",no-port-forwarding,no-agent-forwarding,no-X11-forwarding,no-pty ssh-ed25519 AAAA...
```

Remote control examples:

```bash
ssh ploy@<EC2_PUBLIC_IP> "svc-status sports"
ssh ploy@<EC2_PUBLIC_IP> "svc-start sports"
ssh ploy@<EC2_PUBLIC_IP> "svc-logs sports 200"

ssh ploy@<EC2_PUBLIC_IP> "svc-status crypto"
ssh ploy@<EC2_PUBLIC_IP> "svc-restart crypto"
ssh ploy@<EC2_PUBLIC_IP> "svc-logs crypto 200"
```

Note: `ssh_ployctl.sh` uses `sudo systemctl`/`sudo journalctl`; grant the `ploy` user passwordless sudo for these commands.

## 6) Safety Checklist

- keep both workloads in dry-run until full validation
- verify Sports PM can read DB and has seeded NBA team stats
- keep `PLOY_RPC_WRITE_ENABLED=false` unless explicitly required
- monitor systemd restarts and journal error rates
