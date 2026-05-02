# PM5D Factor Stability Probe

Generated from snapshot-backed Factor Walk-Forward V2 run `25243415961`.

## Question

Can the current candidate factors prove stable profitability from train into
validation before any PM5D dry-run restoration?

## Inputs

- Workflow: `factor-walk-forward-v2.yml`
- Run: `25243415961`
- Source: `snapshot`
- Snapshot run: `25204438461`
- Snapshot hash: `fb338e1f202c3bda`
- Snapshot window: `2026-04-24T00:00:00Z -> 2026-05-02T00:00:00Z`
- Train window: `2026-04-24 -> 2026-04-29`
- Validation window: `2026-04-29 -> 2026-05-02`
- Symbols: `BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,BNBUSDT`
- Stake: `15`
- Train/test shape: `train_window_days=5`, `test_window_days=3`, `step_days=1`
- Minimum observations: `80`
- Factor filter:
  `side_model_prob,side_distance_over_sigma,cex_continuation_edge_gate,exit_bid_change_30s,entry_ask_change_30s,pm_reprice_speed_30s,obi_persistence_30s_side,cex_continuation_score_side`

## Data Health

- Source observations: `129839`
- V2 rows: `259678`
- Executable PnL rows: `50371`
- Entry fill rate: `19.40%`
- Exit fill rate: `17.97%`
- Full-depth entry fill rate: `46.84%`
- Full-depth exit fill rate: `37.43%`
- Snapshot data audit status: `critical`

## Raw Walk-Forward Read

Before applying the liquidity gate, the strongest validation PnL factors were:

| Factor | Validation PnL | Positive Window Ratio | Avg Fill Rate | Symbol Positive |
| --- | ---: | ---: | ---: | ---: |
| `exit_bid_change_30s` | `8914.4190` | `1.0000` | `0.6145` | `0.6667` |
| `cex_continuation_edge_gate` | `4416.7277` | `1.0000` | `0.6793` | `0.5000` |
| `entry_ask_change_30s` | `2390.5643` | `1.0000` | `0.7229` | `0.3333` |
| `pm_reprice_speed_30s` | `2390.5643` | `1.0000` | `0.7229` | `0.3333` |

The raw alpha-side factors were negative on validation:

| Factor | Direction | Train PnL | Validation PnL | Symbol Positive |
| --- | ---: | ---: | ---: | ---: |
| `side_model_prob` | `-1` | `88694.4939` | `-4832.1955` | `0.3333` |
| `side_distance_over_sigma` | `-1` | `88667.0470` | `-4867.1008` | `0.3333` |

This means raw single-factor results should not be promoted directly. They are
sensitive to execution and liquidity context.

## Liquidity-Gated Read

After applying the liquidity gate, the same train-to-validation probe changes
materially:

| Factor | Decision | Validation PnL | Positive Window Ratio | Fill Rate | Symbol Positive | Time Bucket Positive |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `side_distance_over_sigma` | `watchlist` | `5997.9890` | `1.0000` | `1.0000` | `1.0000` | `1.0000` |
| `side_model_prob` | `watchlist` | `5990.4034` | `1.0000` | `1.0000` | `1.0000` | `1.0000` |
| `cex_continuation_edge_gate` | `watchlist` | `353.9439` | `1.0000` | `1.0000` | `0.5000` | `0.7500` |
| `entry_ask_change_30s` | `watchlist` | `58.3147` | `1.0000` | `1.0000` | `0.6667` | `0.5000` |
| `pm_reprice_speed_30s` | `watchlist` | `58.3147` | `1.0000` | `1.0000` | `0.6667` | `0.5000` |
| `exit_bid_change_30s` | `reject` | `-39.6352` | `0.0000` | `1.0000` | `0.5000` | `0.5000` |
| `cex_continuation_score_side` | `reject` | `-416.6829` | `0.0000` | `1.0000` | `0.1667` | `0.5000` |
| `obi_persistence_30s_side` | `reject` | `-1349.0392` | `0.0000` | `1.0000` | `0.1667` | `0.0000` |

All positive rows are still `watchlist`, not `candidate`, because the run has
only one train-to-validation window. The workflow reason is
`too_few_windows_positive_pnl`.

## Interpretation

- The `80` gate is not per-symbol. It is a cross-symbol sample-power floor for
  the selected factor/hypothesis in a split.
- Per-symbol stability is checked separately through symbol-positive rates.
- Raw factor profitability is not stable enough to trust by itself.
- Liquidity-gated alpha factors are much more promising in this exact
  `5d train -> 3d validation` probe: `side_model_prob` and
  `side_distance_over_sigma` become strongly positive with `100%` fill,
  symbol-positive, and time-bucket-positive rates.
- This still does not authorize dry-run restoration because there is only one
  validation window and the source snapshot data audit remains `critical`.

## Decision

`continue-factor-stability-research`

Do not restore PM5D dry-run/live from this evidence. The next useful step is a
multi-window liquidity-gated factor stability run on longer or fresher snapshot
coverage, then a strict matrix/runtime-parity check if the same factors remain
positive across windows.
