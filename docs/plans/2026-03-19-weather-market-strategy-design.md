# Weather Market Strategy Design Draft

Status: draft
Issue: #66
Issue URL: https://github.com/proerror77/ploy/issues/66
Reference: https://x.com/BiteyeCN/status/2034203186970701859

## Objective

Add a standalone canonical strategy that can observe and score Polymarket-style
weather contracts for configurable stations and contract definitions. The goal
is to reproduce the full Biteye-style decision flow, not just a single maximum
temperature forecast.

## Confirmed constraints

- The strategy must be generic for configurable airport/station inputs and
  weather-contract definitions.
- The first delivery is observe-only: emit model state, probabilities, target
  bucket views, and entry suggestions without live order submission.
- Prefer public or low-cost weather data sources over paid commercial feeds.
- The design target includes settlement normalization, multi-source forecast
  fusion, intraday correction, warming/cooling classification, and peak-time
  anomaly handling.

## Planned strategy flow

1. Settlement normalization

Map each contract to the exact reporting station, observation source, units,
rounding, and bucket definitions used at settlement. This isolates market
contracts from user-facing weather-app values and avoids city-vs-station
confusion.

2. Multi-source baseline forecast

Collect forecast snapshots from multiple public or low-cost forecast sources
and merge them into a base maximum-temperature estimate. The merge should be
weather-type aware rather than a fixed average so the runtime can shift weight
between sources when cloud cover, wind, or other conditions imply a different
error profile.

3. Intraday real-time correction

As station observations arrive, update the base forecast with an intraday
correction layer. This stage should estimate the remaining heating potential
for the day and increase the influence of realized observations as local time
approaches the typical daily peak window.

4. Warming/cooling regime classification

Compute a daily regime signal that answers whether today is likely to finish
warmer, flatter, or cooler than the prior day and expose both class and
confidence. This becomes a directional prior that can reinforce or suppress the
baseline forecast and the intraday correction output.

5. Peak-time anomaly detection

Identify cases where the expected daily peak timing deviates from the normal
diurnal pattern, such as late-day or night-time peaks during rain or warm-air
advection. This step exists to catch market situations where the crowd is still
pricing a normal sunny-day temperature curve.

6. Market-vs-model recommendation output

Convert the fused model view into contract-bucket probabilities and compare
them with current market pricing. The observe-only runtime should emit entry
suggestions only when model divergence, confidence, and risk filters all align.

## Repo integration target

- Add a new canonical strategy module under `src/strategy/weather_market/`.
- Register the strategy in `src/strategy/mod.rs` and
  `src/strategy/manager/factory.rs`.
- Add a default config template under `config/strategies/`.
- Expose dry-run-friendly state metrics through `StrategyStateInfo.metrics`.
- Use `StrategyAction::Alert` and `StrategyAction::LogEvent` for recommendation
  output until live execution is explicitly enabled.

## Validation target

- Offline evaluation for forecast accuracy, bucket accuracy, and regime-label
  quality using historical station data plus archived forecast snapshots.
- Focused dry-run verification in the canonical strategy runtime to ensure the
  strategy emits stable state, signals, and recommendation output.
- Explicit documentation of any phase split if offline training and runtime
  inference do not land in the same implementation slice.

## Open decisions

- Whether the first implementation PR should include repo-owned offline
  training and feature generation or land the online inference shell first.
- Which public forecast sources should be first-class in the generic adapter
  layer for cross-region support.
- How much contract-specific bucket logic belongs in config vs reusable helper
  code.
