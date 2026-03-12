# Safe Restart

## When to Use

Use this procedure for planned restarts (config changes, minor updates).
For emergencies, see `emergency-stop.md` instead.

## Procedure

1. Check current state:
   ```bash
   systemctl status ploy
   journalctl -u ploy -n 20 --no-pager
   ```

2. Governance state is auto-persisted to the database (R-07 fix).
   No manual state export is needed before restart.

3. Stop the service:
   ```bash
   sudo systemctl stop ploy
   ```

4. Wait for graceful shutdown (up to 30s). Verify:
   ```bash
   systemctl is-active ploy  # should say "inactive"
   ```

5. Start the service:
   ```bash
   sudo systemctl start ploy
   ```

6. Verify:
   ```bash
   systemctl show ploy -p MemoryMax -p Restart -p OOMPolicy
   journalctl -u ploy -n 30 --no-pager
   curl -fsS http://localhost:8081/health
   ```

## Post-restart Checks

- Confirm positions are recovered from checkpoint (look for "recovery" in logs).
- Confirm governance policies are loaded (look for "governance" in logs).
- Confirm no duplicate orders were placed during restart window.
