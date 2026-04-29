# Binary Option 3D Research Framework

## Goal

Turn 5-minute Polymarket crypto binaries into a research surface with three
explicit dimensions:

1. `time_remaining_secs`
2. current state relative to the settlement line
3. LOB / microstructure direction pressure

This framework exists to avoid collapsing the whole 5-minute lifecycle into one
pooled factor model.

## The Three Dimensions

### 1. Time

Time is the first state variable, not a side feature.

Use:

- `time_remaining_secs`
- `sigma_horizon`

Interpretation:

- Early in the event, time value is large.
- Near expiry, time value collapses and short-term direction dominates.

### 2. State Relative To Settlement

This is the option-state layer. It describes where spot sits relative to the
binary strike / price-to-beat right now.

Use:

- `signed_distance_to_beat`
- `distance_over_sigma`
- `model_prob_up`

Interpretation:

- This replaces any vague notion of “settlement state”.
- Final `settlement_up` is a label, not an input feature.

### 3. LOB Direction Pressure

This is the microstructure confirmation layer. It describes whether the market
is currently reinforcing or fighting the option-state signal.

Use:

- `drift_30s`
- `obi_10`
- `depth_imbalance`
- `cum_mprice_drift_5m`
- `pm_lag_secs`

Interpretation:

- Early, LOB should mostly filter.
- Late, LOB often becomes the primary directional signal.

## Labels

Keep the research split into two targets:

- `settlement_up`
- `future_up_ask_change_30s`

Do not merge these.

- `settlement_up` answers whether the state is informative about the final
  binary outcome.
- `future_up_ask_change_30s` answers whether the state is informative about the
  next repricing move in PM.

## Regime Design

Default time regimes:

| Regime | Window | Research role |
| --- | --- | --- |
| `early` | `181..300s` | time value / option-state dominates |
| `middle` | `61..180s` | transition zone; option-state + LOB interact |
| `late` | `6..60s` | direction confirmation and microstructure matter more |
| `expiry` | `0..5s` | almost pure endgame direction; time value mostly gone |

## What To Research In Each Regime

### Early

Primary questions:

- Is spot far enough from the settlement line after volatility normalization?
- Is PM underreacting to the current binary state?

Primary variables:

- `model_prob_up`
- `distance_over_sigma`

LOB role:

- Confirmation / filter only

### Middle

Primary questions:

- Is the option-state still useful?
- Has the order book started to reinforce or reverse that state?

Primary variables:

- `cum_mprice_drift_5m`
- `depth_imbalance`
- `obi_10`

LOB role:

- Co-equal with option-state

### Late

Primary questions:

- Is the market now clearly resolving in one direction?
- Is there still enough PM lag or time value left to monetize?

Primary variables:

- `drift_30s`
- `pm_lag_secs`
- `sigma_horizon`

LOB role:

- Primary directional signal

### Expiry

Primary questions:

- Is the last directional move strong enough to survive fees and noise?

Primary variables:

- `drift_30s`
- `sigma_horizon`
- `pm_lag_secs`

LOB role:

- Dominant

## Current Evidence

Fresh `BTC+ETH` regime summary from
[btc-eth-regime-summary.log](/Users/proerror/Documents/ploy/tmp/factor-research/btc-eth-regime-summary.log):

### `settlement_up`

- `early`: `model_prob_up`, `distance_over_sigma` dominate
- `middle`: `cum_mprice_drift_5m`, `depth_imbalance`, `obi_10`
- `late`: `drift_30s` is strongest
- `expiry`: `drift_30s` stays strongest; `sigma_horizon` remains relevant

### `future_up_ask_change_30s`

- `early`: `distance_over_sigma`, `model_prob_up` dominate
- `middle`: `distance_over_sigma`, `model_prob_up` still dominate
- `late`: `obi_10` becomes a meaningful microstructure signal

Fresh DB-backed smoke verification from
[btc-time-ic-db-smoke.log](/Users/proerror/Documents/ploy/tmp/factor-research/btc-time-ic-db-smoke.log):

- scope `btc-5s-smoke-2026-04-15`
- persisted rows: `456`
- strongest persisted row:
  - factor: `distance_over_sigma`
  - label: `future_up_ask_change_30s`
  - bucket: `270..274s`
  - `abs_spearman = 0.9028`

## Design Conclusion

Do not build one global factor model.

Build a staged model:

- `early`: option-state model
- `middle`: option-state + LOB mixture
- `late`: LOB / drift-led directional model
- `expiry`: only trade the strongest late confirmation states

In short:

- early looks like an option
- late looks like an order book

## Immediate Next Step

Produce one table per regime with:

- primary target (`settlement` vs `30s repricing`)
- top 3 factors
- sign of relationship
- whether the factor is a driver or only a filter

That table should become the handoff artifact for the next modeling pass.

## Trading Frame

The research now supports a concrete three-layer trade design:

1. **Direction**
   - estimate whether `settlement_up` is sufficiently likely
2. **Confirmation**
   - require LOB / drift to agree with that directional view
3. **Worth-It Gate**
   - require payout structure to be good enough after fees

### Regime Table

| Regime | Time window | Direction driver | Confirmation layer | Reward/risk gate | Default posture |
| --- | --- | --- | --- | --- | --- |
| `early` | `181..300s` | `distance_over_sigma`, `model_prob_up`, `sigma_horizon` | `drift_30s` as filter only | loose | trade only when option-state is strong |
| `middle` | `61..180s` | `fair_prob_up_clean`, `vol_gap`, `cum_mprice_drift_5m` | `obi_10`, `depth_imbalance` | medium | trade when state and LOB do not conflict |
| `late` | `6..60s` | `fair_prob_up_clean`, `vol_gap` | `drift_30s`, `obi_10`, `depth_imbalance` | strict | trade only with clear LOB confirmation |
| `expiry` | `0..5s` | `fair_prob_up_clean` only if still decisive | `drift_30s` dominant | very strict | default `no-trade` unless everything aligns |

### Variable Roles

#### Direction Variables

Use these to estimate the side:

- `distance_over_sigma`
- `model_prob_up`
- `fair_prob_up_clean`
- `implied_sigma_horizon`
- `vol_gap`

Interpretation:

- High positive `fair_prob_up_clean` means the market is already implying UP.
- High negative `distance_over_sigma` historically aligned with stronger UP
  settlement probability in this sample.
- Positive `vol_gap` means implied binary-state volatility exceeds realized
  horizon volatility, which may matter most in `middle` / `late`.

#### Confirmation Variables

Use these to confirm the side:

- `drift_30s`
- `obi_10`
- `depth_imbalance`
- `cum_mprice_drift_5m`

Interpretation:

- Confirmation should not create a direction by itself in `early`.
- Confirmation can dominate timing in `late` / `expiry`.

#### Worth-It Variables

Use these only to decide whether to trade after direction is chosen:

- `reward_risk_up`
- `reward_risk_down`
- `model_edge_up`
- break-even probability from ask + fee

Interpretation:

- `reward_risk` is not a directional variable.
- High reward/risk can simply mean the market price is cheap, not that the
  trade has positive expectancy.

### No-Trade Conditions

Default to `no-trade` when any of the following is true:

- direction variables disagree strongly
- LOB confirmation is mixed in `late` / `expiry`
- `edge <= 0` after fee-adjusted break-even
- reward/risk is poor for the chosen side
- expiry is too near and only one layer is active

### Minimal Decision Skeleton

1. Choose regime from `time_remaining_secs`
2. Estimate direction from the regime's direction drivers
3. Check whether the regime's confirmation variables agree
4. Compute fee-adjusted edge and reward/risk
5. Trade only if all three layers pass

In short:

- direction first
- LOB second
- payout filter last
