# Crypto 5m Repricing V1 Design

**Date:** 2026-03-07
**Scope:** Polymarket 5-minute crypto markets only
**Goal:** Build a baseline strategy framework that trades early repricing, not last-minute settlement guessing.

---

## Problem

The repo already has:

- `src/analysis/updown_backtest.rs` for 5m/15m event research.
- `src/strategy/directional_backtest.rs` for binary-option replay with fees and execution simulation.
- `src/strategy/backtest_feed.rs` for integrated historical replay from Binance and Polymarket tables.

None of those modules matches the requested semantics. The current directional engine is momentum-first,
holds to settlement by default, and does not model the specific "trade repricing between T-240s and T-75s,
then flatten by T-45s" workflow.

We need a separate strategy boundary so v1 can be backtested immediately and extended into a live trader
without reusing the wrong assumptions.

## Design Choice

Recommended approach: add a new dedicated module pair under `src/strategy/`:

- A reusable decision/core layer for 5-minute crypto repricing.
- A replay/backtest layer that consumes `HistoricalFeed`.

This keeps the domain language correct:

- fair probability is driven by short-window threshold crossing logic
- direction comes from Binance microstructure
- exits are time/risk driven, not settlement-driven

Alternatives considered:

1. Extend `directional_backtest.rs`
   Rejected because it would overload the existing momentum strategy with a different entry/exit model.

2. Keep the logic only in `analysis/updown_backtest.rs`
   Rejected because that module is research-oriented and not shaped for future live execution.

## V1 Boundaries

V1 trades only when all of these are true:

- event window is 5 minutes
- symbol is one of the configured crypto pairs
- entry time is between `T-240s` and `T-75s`
- spread and depth pass liquidity filters
- fair-gap exceeds an explicit cost buffer
- Binance L2 directional score confirms the side

V1 exits only by:

- partial/full take-profit on gap compression
- signal reversal
- time stop
- hard flat at `T-45s`

V1 will be live-ready, not full live-complete. It will define the same features, state, and decision outputs
that a future live runner can consume, but this turn will prioritize replay/backtest wiring and leave maker/GTD
order management as the next layer.

## Data Model

V1 will reuse `HistoricalFeed` and consume these update classes:

- `SpotTrade` for Binance price path and short-window realized vol
- `BinanceL2` for imbalance / depth features
- `PmQuote` for Polymarket best bid/ask on YES/NO tokens
- `EventState` for window start, end, and settlement metadata
- `LobSnapshot` for optional Polymarket ask-side depth filters

The new strategy-specific state will track:

- active 5-minute event metadata
- rolling Binance returns for `RV_30s`, `RV_120s`, `RV_300s`
- latest Binance L2 feature snapshot
- latest PM YES/NO quotes and optional depth
- open positions and their entry gap / exit timers

## Signal Model

V1 baseline model:

1. Estimate short-window volatility from realized returns over 30s / 120s / 300s.
2. Convert spot-vs-threshold distance into a work probability `p_fair`.
3. Build a direction score from Binance imbalance and near-term price pressure.
4. Compute side-specific fair-gap versus PM quotes.
5. Enter only when fair-gap clears a cost buffer and the direction score agrees.

The fair value model will stay intentionally simple in v1:

- no daily GARCH
- no full live aggTrade signing dependency
- no final-minute entry logic

That matches the requested baseline: fair-gap plus OFI/imbalance, on early repricing only.

## Execution Semantics

Replay will model:

- taker-fee drag via `FeeModel::crypto()`
- spread/slippage via `ExecutionSimulator`
- forced pre-expiry exits instead of settlement holds

The core decision layer will still emit execution intent metadata:

- desired side (`YES` / `NO`)
- urgency (`maker_candidate` / `taker_candidate`)
- target expiry for future GTD handling

That lets the live runner add post-only/GTD or IOC/FOK later without rewriting the strategy model.

## Files

New or updated files:

- `src/strategy/crypto_repricing.rs` or split submodule for shared config/state/decision logic
- `src/strategy/crypto_repricing_backtest.rs` for replay engine
- `src/strategy/mod.rs` for exports
- `src/analysis/mod.rs` only if analysis-only helpers are factored there
- `src/cli/strategy.rs` for CLI wiring
- `tasks/todo.md` and `docs/plans/...` for traceability

## Testing

Targeted tests will prove:

- entries are rejected outside `T-240s` to `T-75s`
- insufficient fair-gap after costs does not trade
- Binance L2 direction disagreement blocks entries
- positions are flattened by `T-45s`
- replay PnL reflects fee/slippage, not raw mark-to-market fantasy
