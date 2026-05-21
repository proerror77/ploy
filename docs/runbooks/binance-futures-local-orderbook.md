# Binance Futures Local Orderbook Contract

PM5D factor research may use Binance L2/LOB as prediction context, but the
data surface has two distinct evidence levels:

- `binance_lob_ticks` and `scripts/binance_lob_collector.py` are partial-depth
  snapshot evidence. They are useful for pressure, imbalance, and short-term
  context diagnostics.
- Promotion-grade FactorEvolve LOB research requires a local futures book built
  from an exchange snapshot plus incremental diff-depth updates with strict
  update-id sequencing.

The local book contract is:

- initialize from a snapshot with `last_update_id`;
- accept the first diff only when
  `first_update_id <= last_update_id + 1 <= final_update_id`;
- accept later diffs only when
  `previous_final_update_id + 1 == first_update_id`;
- reject sequence gaps or out-of-order updates fail-closed.

No runtime strategy or research promotion gate should treat unsequenced partial
depth as queue-position, passive-fill, or full execution-quality evidence.
