# Polymarket Expansion Master Plan

## Goal

Upgrade the workspace so it can support Polymarket's expanded market surface across:

- Chainlink-backed high-frequency crypto markets
- Pyth-backed equities, FX, metals, and commodities
- sports market discovery and live-state ingestion
- unified CLI/operator visibility
- historical backtest and recorded replay flows that understand the new market families

## Working Assumptions

- Crypto 5m/15m markets remain the first-class live strategy target and must keep working throughout the refactor.
- Pyth-backed non-crypto markets are required for data capture, discovery, and backtest support in this project phase.
- Sports support is required for discovery, metadata normalization, live score/game-state ingestion, and backtest/research support.
- Sports-specific live order execution must be planned with stricter safeguards because PM sports books clear at game start; execution support can ship after the data and backtest foundations are in place.
- Existing research/backtest trust boundaries stay conservative: new sources do not become default truth until validated.
- Settlement data must remain explicitly backfillable and correctable from official PM sources; no phase may introduce a path where unresolved or stale settlement gaps become silently accepted as final truth.

## Current-State Constraints

- RTDS support in the vendored SDK only exposes typed helpers for `crypto_prices` and `crypto_prices_chainlink`.
- The live runner already depends on Chainlink RTDS to populate `price_to_beat` for crypto markets.
- Market discovery in the live runner is still tuned to short-dated crypto up/down windows and relies on hardcoded symbol inference from market questions.
- `ployctl` currently exposes control-plane status, trading state, and deployment operations, but no market-data or discovery commands.
- The sidecar Polymarket tool only performs title-based Gamma event search and point-in-time market snapshots.
- Historical database backtests already replay canonical `MarketUpdate` sequences and can fall back from `clob_quote_ticks` to `clob_orderbook_snapshots`, but they do not yet model the broader reference-price universe or sports state feeds.

## Architecture Direction

The refactor should converge on one shared model:

1. **Reference data layer**
   - A unified reference-price subsystem for Binance, Chainlink, and Pyth-originated feeds.
   - Shared caches plus persisted time series with source metadata and freshness semantics.

2. **Market discovery layer**
   - Family-specific discovery adapters:
     - short-dated crypto binary windows
     - sports event markets
     - Pyth-backed event markets when PM expands them into new categories
   - A normalized internal market descriptor with explicit market family, reference symbol, settlement source, lifecycle timestamps, and token metadata.

3. **Runtime/event layer**
   - `MarketUpdate` remains the runtime contract, but the event vocabulary needs to expand around market metadata and external state updates.
   - Live feeds, recorded replay, and historical DB loaders must all be able to produce the same canonical semantics.

4. **Operator/CLI layer**
   - `ployctl` exposes inspection and diagnostics for discovery state, reference feeds, and captured market families.
   - `ploy-sidecar` exposes richer search/discovery tools for sports, crypto, and reference-feed-aware lookup.

5. **Research/backtest layer**
   - Historical backtests gain explicit support for multiple reference-price sources and sports event-state inputs.
   - Replay mode remains the parity path for live-session reproduction.
   - Settlement completion/backfill remains a first-class data-quality requirement, not a best-effort cleanup step.

## Workstreams

### Workstream 1: Vendored Polymarket SDK RTDS Expansion

**Primary files**

- `vendor/polymarket-client-sdk/src/rtds/client.rs`
- `vendor/polymarket-client-sdk/src/rtds/types/request.rs`
- `vendor/polymarket-client-sdk/src/rtds/types/response.rs`
- `vendor/polymarket-client-sdk/examples/rtds_crypto_prices.rs`
- `vendor/polymarket-client-sdk/tests/websocket.rs`

**Objectives**

- Add typed `equity_prices` support for Pyth-backed feeds.
- Add response models for:
  - live equity price updates
  - historical subscribe snapshots
  - carried-forward closed-session prices
- Fix subscription serialization so `equity_prices` emits filters in the format the official RTDS docs require.
- Preserve `subscribe_raw` for forward compatibility, but stop depending on raw-topic parsing for first-class feed families that the repo actively uses.

**Deliverable**

A vendored SDK that can subscribe to Chainlink crypto and Pyth equity/commodity feeds without downstream custom protocol hacks.

### Workstream 2: Unified Reference-Price Feed Layer

**Primary files**

- `apps/ploy-runner/src/feeds.rs`
- `apps/ploy-runner/src/main.rs`
- `apps/ploy-runner/src/collector.rs`
- `crates/ploy-strategy-bundles/src/traits.rs`
- `config/default.toml`

**Objectives**

- Replace the narrow `ChainlinkPriceCache` concept with a generalized reference-price registry keyed by:
  - normalized symbol
  - source
  - timestamp/freshness
  - asset class
- Keep crypto runtime behavior intact while adding:
  - Pyth `equity_prices` capture
  - symbol normalization for `AAPL`, `XAUUSD`, `WTI`, `EURUSD`, etc.
- Decide whether `MarketUpdate::SpotPrice` stays generic enough or whether a new event type is needed for non-crypto reference ticks.
- Add config for subscribing to non-crypto reference symbols by family.

**Deliverable**

One feed subsystem that can power runtime discovery, backtest loading, and operator diagnostics across crypto and non-crypto reference markets.

### Workstream 3: Market Discovery Refactor

**Primary files**

- `apps/ploy-runner/src/scanner.rs`
- `apps/ploy-runner/src/main.rs`
- `ploy-sidecar/src/tools/polymarket.ts`
- `vendor/polymarket-client-sdk/src/gamma/client.rs`
- `config/default.toml`

**Objectives**

- Split discovery into explicit adapters instead of one crypto-only scanner.
- Introduce a normalized market descriptor with fields for:
  - market family
  - exchange/league
  - PM event ID / market ID / slug
  - up/down or yes/no semantics
  - reference symbol and settlement source
  - start/end timestamps
  - lifecycle rules
- Use Gamma sports endpoints (`/sports`, `/teams`, `/sports/market-types`) where applicable instead of title-only heuristics.
- Make sidecar search return richer typed results and not depend solely on `events?title=...`.

**Deliverable**

Discovery that can reliably find and describe crypto, sports, and future PM market families without brittle question-string matching.

### Workstream 4: Sports Live-State Ingestion

**Primary files**

- `apps/ploy-runner/src/` new `sports.rs` or `sports_feed.rs`
- `apps/ploy-runner/src/main.rs`
- `crates/ploy-strategy-bundles/src/traits.rs`
- `crates/ploy-strategy-bundles/src/feed/recorded.rs`
- new persistence migrations for sports state if needed

**Objectives**

- Add a client for the PM Sports WebSocket.
- Normalize live game state into canonical runtime updates:
  - score
  - period
  - status
  - elapsed
  - finished timestamp
- Attach sports state to discovered PM sports markets through slug/team mapping.
- Record sports state in replay logs so live sessions can be reproduced exactly.
- Plan, but do not prematurely enable, sports execution paths that ignore the game-start orderbook reset behavior.

**Deliverable**

The platform can ingest live sports state and use it in research, diagnostics, recorded replay, and later strategy logic without enabling sports live execution yet.

### Workstream 5: Historical Database And Replay Backtest Upgrade

**Primary files**

- `crates/ploy-strategy-bundles/src/feed/database.rs`
- `crates/ploy-strategy-bundles/src/feed/recorded.rs`
- `crates/ploy-strategy-bundles/src/feed/historical.rs`
- `crates/ploy-strategy-bundles/src/config.rs`
- `crates/ploy-research/src/backtesting.rs`
- `crates/ploy-research/src/replay.rs`
- `crates/ploy-strategy-bundles/tests/backtest_integration.rs`

**Objectives**

- Extend the historical loader to understand new persisted sources:
  - Pyth/equity reference prices
  - sports state timelines
  - richer market metadata
- Define source-ranking rules for non-crypto markets analogous to the current PM quote trust model.
- Keep replay as the gold-standard parity path for live captures.
- Make backtests asset-class aware:
  - crypto binary windows
  - sports event markets
  - future non-crypto PM contracts that settle against Pyth feeds
- Ensure historical resolution semantics stay official and explicit.

**Deliverable**

The backtest system can replay and evaluate strategies across the expanded PM market surface without relying on crypto-only assumptions, while still anchoring settlement repair on official PM settlement rows and keeping additive sources behind explicit loader flags until trust cutover.

### Workstream 6: CLI And Operator Surface Upgrade

**Primary files**

- `apps/ployctl/src/main.rs`
- `apps/ployctl/src/client.rs`
- `apps/ployctl/src/` new `markets.rs`
- `apps/ployctl/src/` new `feeds.rs`
- `ploy-sidecar/src/index.ts`
- `ploy-sidecar/src/tools/polymarket.ts`

**Objectives**

- Add `ployctl` commands for:
  - market discovery inspection
  - reference feed health/status
  - capture/source freshness
  - targeted market snapshot inspection by family
- Keep `ployctl` aligned with the control-plane model rather than turning it into an ad hoc raw API shell.
- Upgrade sidecar Polymarket tools to expose:
  - family-aware market search
  - sports lookup by team/league/slug
  - reference-feed-aware snapshots

**Deliverable**

Operators and agents can see what markets exist, what feeds are healthy, and what data the backtest/runtime can actually trust.

### Workstream 7: Schema, Persistence, And Migration

**Primary files**

- `migrations/`
- `crates/ploy-strategy-bundles/src/feed/database.rs`
- `apps/ploy-runner/src/collector.rs`
- `apps/ploy-runner/src/scanner.rs`

**Objectives**

- Decide whether to extend existing tables or add explicit new ones for:
  - non-crypto reference ticks
  - sports game-state events
  - normalized market metadata by family
- Avoid writing family-specific hacks into `pm_market_metadata` when the shape differs materially from short-dated crypto markets.
- Preserve compatibility with existing historical crypto data and trusted PM quote paths.

**Deliverable**

A migration path that supports the new market families without corrupting the assumptions of existing research tables.

## Recommended Phase Order

### Phase 1: Protocol And Data Foundations

- Workstream 1
- Workstream 2
- schema slice from Workstream 7

**Why first**

Everything else depends on being able to subscribe to and persist the new reference-price families correctly.

### Phase 2: Discovery And Metadata Normalization

- Workstream 3
- remaining schema slice from Workstream 7

**Why second**

CLI, runtime wiring, and backtest ingestion all need one normalized view of what a PM market is.

### Phase 3: Sports State Ingestion

- Workstream 4

**Why third**

Sports support is not just more markets; it introduces a second live-state channel and different lifecycle semantics.

### Phase 4: Historical/Replay Backtest Upgrade

- Workstream 5

**Why fourth**

Once the new sources and descriptors exist, the backtest system can ingest them without speculative schema churn.

### Phase 5: CLI And Sidecar Upgrade

- Workstream 6

**Why fifth**

CLI surfaces should reflect the real system model after the underlying discovery/feed contracts settle.

### Phase 6: Hardening And Strategy Enablement

- trust cutovers
- source-health metrics
- sports execution guardrails
- documentation and runbook refresh

## Verification Matrix

Each phase needs a proof target before moving on.

### Foundation verification

- Vendored SDK tests for `equity_prices` subscribe/unsubscribe and deserialization
- Focused `rtk cargo check` for `ploy-runner` and touched crates
- smoke collector run against a small Pyth symbol set

### Discovery verification

- unit tests for symbol/market-family normalization
- snapshot tests for sidecar search results
- Gamma integration checks for sports endpoints

### Sports verification

- recorded fixtures for Sports WebSocket messages
- replay parity tests proving sports state survives record/replay
- DB persistence checks for sports state capture

### Backtest verification

- integration tests for historical loader across:
  - crypto-only
  - non-crypto reference data
  - sports market + sports state
- regression tests that existing crypto backtests still pass unchanged

### CLI verification

- parser tests for new `ployctl` subcommands
- snapshot tests for human-readable output
- end-to-end tests against fixture-backed control-plane snapshots where possible

## Risks

- The vendored SDK may need broader RTDS abstractions than a one-topic patch if PM expands the protocol again.
- Sports markets can look tradable but have very different execution risk because orders are cancelled around start time.
- Non-crypto reference prices can produce stale carried-forward values outside market hours; the runtime and backtest path must not treat those as live ticks.
- Overloading crypto-oriented tables with sports/non-crypto semantics will make the historical loader brittle.

## Explicit Non-Goals For The First Execution Pass

- Shipping a production-ready sports market-making strategy before sports state ingestion and lifecycle safeguards are proven
- Treating newly captured Pyth or sports data as trusted historical truth without validation windows
- Replacing replay mode with the historical DB path; replay remains the canonical live-session parity tool

## Proposed Execution Split

After approval, execution should be split into these implementation plans:

1. **RTDS + reference-price foundation**
   - `docs/plans/2026-04-06-rtds-reference-foundation-implementation-plan.md`
2. **Discovery + metadata normalization**
   - `docs/plans/2026-04-06-market-discovery-normalization-implementation-plan.md`
3. **Sports live-state capture**
   - `docs/plans/2026-04-06-sports-data-capture-implementation-plan.md`
4. **Backtest/replay expansion**
   - `docs/plans/2026-04-06-backtest-replay-expansion-implementation-plan.md`
5. **CLI + sidecar operator surface**
   - `docs/plans/2026-04-06-cli-operator-surface-implementation-plan.md`
6. **Hardening, docs, and trust-cutover**
   - `docs/plans/2026-04-06-hardening-trust-cutover-implementation-plan.md`

This keeps each plan independently testable and avoids one giant cross-cutting patch set.
