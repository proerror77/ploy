# AWS EC2 Deployment Runbook

This runbook deploys two always-on workloads on one EC2 host:
- `ploy-sports-pm.service` (Sports PM / NBA comeback)
- `ploy-crypto-dryrun.service` (Crypto trading agents dry-run)

## 1) Deploy to EC2

Run from your local machine:

```bash
scripts/aws_ec2_deploy.sh \
  --host <EC2_PUBLIC_IP> \
  --key ~/.ssh/<your-key>.pem \
  --services ploy-sports-pm,ploy-crypto-dryrun
```

Optional:

```bash
scripts/aws_ec2_deploy.sh \
  --host <EC2_PUBLIC_IP> \
  --user ubuntu \
  --key ~/.ssh/<your-key>.pem \
  --start true \
  --enable true \
  --services ploy-sports-pm,ploy-crypto-dryrun
```

What it installs:
- systemd units: `ploy-sports-pm.service`, `ploy-crypto-dryrun.service`
- config files:
  - `/opt/ploy/config/sports_pm.toml`
  - `/opt/ploy/config/crypto_dry_run.toml`
- env files:
  - `/opt/ploy/.env`
  - `/opt/ploy/env/sports-pm.env`
  - `/opt/ploy/env/crypto-dryrun.env`

## 2) Configure Environment

On EC2:

```bash
sudoedit /opt/ploy/.env
sudoedit /opt/ploy/env/sports-pm.env
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
```

## 3) Sports PM DB Seed (Required)

Sports PM needs `nba_team_stats` data before signals are generated.

```bash
cd /opt/ploy
./target/release/ploy --config /opt/ploy/config/sports_pm.toml strategy nba-seed-stats --season 2025-26
```

## 4) Service Verification

```bash
sudo systemctl status ploy-sports-pm
sudo systemctl status ploy-crypto-dryrun
```

```bash
sudo journalctl -u ploy-sports-pm -f
sudo journalctl -u ploy-crypto-dryrun -f
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
