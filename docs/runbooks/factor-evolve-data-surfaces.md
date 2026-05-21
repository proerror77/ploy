# FactorEvolve Data Surface Gates

This runbook names the current PM5D / FactorEvolve data surfaces and how they
can be used in research evidence.

## Current Surfaces

| Surface | Current status | Gate category | Notes |
| --- | --- | --- | --- |
| Binance aggTrade | present | `required_for_prediction` | External traded price and signed-flow context. |
| Binance partial LOB | present, not sequence-correct local book | `required_for_prediction` | Useful for diagnostic pressure/imbalance; not queue-position evidence. |
| Binance futures local book | not first-class | `missing_blocks_promotion` when claiming LOB execution quality | Requires snapshot plus diff-depth sequencing. |
| Polymarket quote ticks | present | `required_for_prediction` | Top-of-book PM state. |
| Polymarket full CLOB snapshots | present and archived to lake | `required_for_execution` | Required for executable sweep, capacity, and conservative fillability. |
| Official settlement | present for PM5D evidence | `required_for_execution` | Required for settlement-probability labels. |
| Dry-run/runtime fills | present | `required_for_execution` only for runtime parity | Observed execution behavior, not a substitute for official settlement labels. |
| Binance futures OI/funding/liquidation/basis | not first-class | `optional_context` until a hypothesis explicitly requires it | Missing surface must be stated in evidence. |
| OKX/Bybit LOB | not first-class | `optional_context` until multi-exchange hypothesis requires it | Missing surface must be stated in evidence. |

## Gate Categories

- `required_for_prediction`: a surface needed to compute the claimed signal.
- `required_for_execution`: a surface needed to prove executable entry, exit,
  fillability, settlement, or runtime parity.
- `optional_context`: a useful explanatory surface that is not required for the
  current hypothesis.
- `missing_blocks_promotion`: a missing surface that prevents dry-run or live
  promotion for the claimed strategy lane.

If evidence uses a missing surface as part of the edge mechanism, the result is
`diagnostic` or `revise`; it is not a promotion candidate.
