# Circuit Breaker

## States

- **Closed**: Normal operation. Failures are counted.
- **Open**: Tripped after threshold failures. All requests are rejected.
- **HalfOpen**: After cooldown, a limited number of requests are allowed through.
  Successes transition back to Closed; failures re-open the breaker.

## Inspect Current State

Via API:
```bash
curl -s http://localhost:8081/api/sidecar/risk | jq '.circuit_breaker'
```

Via logs:
```bash
journalctl -u ploy --no-pager | grep -i "circuit.breaker" | tail -20
```

## Manual Reset

If the circuit breaker is stuck in Open state after the underlying issue is resolved:

1. Verify the root cause is fixed (e.g., exchange API is back up).
2. Restart the service — the breaker resets to Closed on startup:
   ```bash
   sudo systemctl restart ploy
   ```
3. Monitor logs for the first few minutes to confirm requests succeed.

## Tuning

Circuit breaker parameters are configured in the coordinator bootstrap config:
- `failure_threshold`: Number of consecutive failures before opening.
- `cooldown_secs`: Seconds to wait in Open before transitioning to HalfOpen.
- `half_open_max_requests`: Requests allowed in HalfOpen before deciding.

## Validation

After reset, confirm:
```bash
journalctl -u ploy -n 30 --no-pager | grep -i "circuit"
curl -s http://localhost:8081/api/sidecar/risk | jq '.circuit_breaker.state'
```

Expected: state is `"Closed"` and requests are flowing normally.
