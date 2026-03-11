# Rollback

## When to Use

- A new release causes crashes, order failures, or unexpected behavior.
- Health checks fail after deployment.
- You need to revert to a known-good binary quickly.

## Procedure

### Option 1: GitHub Workflow (Preferred)

1. Go to Actions > "Rollback Production" workflow.
2. Click "Run workflow".
3. Select mode:
   - `latest-backup`: Restores the most recent backup binary on the host.
   - `specific-tag`: Restores a specific release (e.g., `v1.2.3`) from `/root/ploy/releases/`.
4. Enter a reason for the rollback.
5. Click "Run workflow".

### Option 2: Manual SSH

```bash
ssh root@<host>
sudo systemctl stop ploy
sudo cp /root/ploy/bin/ploy.bak /root/ploy/bin/ploy
sudo chmod +x /root/ploy/bin/ploy
sudo systemctl start ploy
```

## Post-rollback Checks

1. Verify the service is running:
   ```bash
   systemctl is-active ploy
   systemctl status ploy --no-pager
   ```

2. Check health endpoint:
   ```bash
   curl -fsS http://localhost:8081/health
   ```

3. Verify the correct binary version:
   ```bash
   cat /root/ploy/bin/.current_release
   ```

4. Monitor logs for the first few minutes:
   ```bash
   journalctl -u ploy -f --no-pager
   ```

5. Confirm no duplicate or stale orders from the failed release.
