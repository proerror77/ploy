# Staggered Arb Strategy: Core Idea, Entry/Exit, State Machine

## Core idea
- Trade prediction-market binary pairs (`UP`, `DOWN`) as a two-leg cycle.
- Buy Leg1 on the predicted side early in the event.
- Buy Leg2 on the opposite side when sum/edge conditions are met.
- Treat cycle as complete (`merge` close reason) once both legs are filled.
- Keep the strategy in strict single-cycle mode: do not open a new cycle while one is in-flight.

## State machine
1. `Idle`
2. `EntryGateEval`
3. `Leg1Submit` (live) / `Leg1Filled` (paper)
4. `Leg1Filled`
5. `Leg2GateEval`
6. `Leg2Submit` (live) / `Merged` or `ForcedComplete` (paper)
7. `Merged` or `ForcedComplete`
8. back to `Idle`

## Entry gates (Leg1)
- Balance pause gate (`balance_pause_until`) when recent balance failures happened.
- Active cycle lock (`has_active_cycle`).
- Time remaining gate (`min_time_remaining_secs`).
- Entry timing gate (`entry_after_start_max_secs`).
- Market quote presence (`up_ask`, `down_ask`) and min ask floor (`min_ask_price`).
- Pair sum gates:
- `current_sum >= min_entry_sum`
- `current_sum < max_initial_sum`
- Probability and directional conviction:
- valid `open_price` anchor (`s0`)
- `|p_hat - 0.5| >= direction_threshold`
- Price displacement gate from event open anchor.
- Greeks confirmation gates (if enabled): gamma/theta/delta/vega/d2/fair-value consistency.
- Binance L2 OBI confirmation gate (freshness + direction alignment).
- Leg1 price cap (`max_leg1_price`).
- Leg2 feasibility gate (`merge_target_sum - leg1_price > 0`).
- Cooldown gate (`cooldown_secs`).
- Concurrency and duplication gates:
- `max_concurrent_positions`
- no duplicate open event
- no duplicate pending Leg1 event
- Per-event limit gate (`max_trades_per_event`).
- Sizing / reserve gates:
- non-zero shares
- reserve guard (`min_balance_usd`)

## Exit gates (Leg2)
- Skip if Leg2 order is already pending.
- Final-minute no-trade gate (`no_trade_last_secs`).
- Opposite-side quote presence and min ask floor.
- Minimum delay gate (`min_leg2_delay_secs`).
- Close triggers:
- `current_sum <= merge_target_sum`
- fee-adjusted profit `>= min_profit_target`
- any fee-adjusted positive profit
- forced stop-loss (`max_leg1_loss`)
- forced timeout (`wait_deadline`)
- forced time safety (`time_remaining < min_time_remaining_secs`)

## Runtime observability (new)
The strategy state metrics now expose gate counters:
- Entry counters: `entry_gate_*`
- Leg2 counters: `leg2_gate_*`

Useful keys include:
- `entry_gate_entry_accepted`
- `entry_gate_active_cycle_lock`
- `entry_gate_entry_window_expired`
- `entry_gate_direction_strength_below_threshold`
- `entry_gate_obi_not_confirmed`
- `entry_gate_leg1_price_above_cap`
- `entry_gate_reserve_guard`
- `leg2_gate_final_minute_block`
- `leg2_gate_min_leg2_delay`
- `leg2_gate_leg2_order_pending`

These counters are cumulative since strategy start and are meant to identify the dominant bottleneck reducing trade count.
