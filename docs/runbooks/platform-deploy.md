# Platform Deploy Runbook

## Default Release Path

The workspace-default deploy path is:

- GitHub Actions workflow: `.github/workflows/release-platform.yml`
- Long-running systemd unit: `deployment/ployd.service`
- Host install helper: `scripts/install-platform-service.sh`

This path deploys exactly two binaries:

- `/opt/ploy/bin/ployd`
- `/opt/ploy/bin/ployctl`

`ployd` is the only default long-running process. `ployctl` is an operator
client used for post-deploy verification and remote inspection.

## CI Bundle Contents

The release bundle contains:

- `bin/ployd`
- `bin/ployctl`
- `deployment/ployd.service`
- `scripts/install-platform-service.sh`
- `data/state/deployments.json.sample`

## Host Installation Flow

After the bundle lands on the host, the deploy workflow:

1. Installs `ployd` and `ployctl` into `/opt/ploy/bin`
2. Installs `deployment/ployd.service` into `/opt/ploy/deployment`
3. Installs `scripts/install-platform-service.sh` into `/opt/ploy/scripts`
4. Seeds `/opt/ploy/data/state/deployments.json` if missing
5. Runs `install-platform-service.sh`
6. Restarts `ployd`
7. Verifies:
   - `systemctl status ployd`
   - `/opt/ploy/bin/ployctl system status`

## Required Host Paths

The install script ensures:

- `/opt/ploy/.env`
- `/opt/ploy/data/state/deployments.json`
- `/opt/ploy/run/platform/system-status.json`
- `/opt/ploy/run/platform/deployments.json`

## Post-Deploy Checks

```bash
sudo systemctl status ployd --no-pager
sudo journalctl -u ployd -n 200 --no-pager
/opt/ploy/bin/ployctl system status
/opt/ploy/bin/ployctl deployments list
```

## Legacy Workflows

Older workflows that still build or deploy the single-binary `ploy` path are
legacy only. They are not the default release path for the trading-platform
workspace.
