---
title: Polymarket V2 Migration + SDK Optimization
date: 2026-04-20
deadline: 2026-04-28 ~11:00 UTC (cutover)
status: approved
---

# Polymarket V2 Migration + SDK Optimization

## Context

Polymarket CLOB V2 goes live on **April 28, 2026 (~11:00 UTC)**. All open orders will be wiped during the ~1 hour downtime window. This spec covers the mandatory V2 migration changes plus previously identified SDK optimizations.

Canary wallet validation already passed (SAFE ✅ PROXY ✅ for USDC.e → pUSD wrap).

---

## Group 1: SDK Layer (blocks Groups 2 & 3)

**Files:** `vendor/polymarket-client-sdk/src/`

### 1a. Contract Addresses (`lib.rs`)

Replace hardcoded V1 exchange addresses in `CONFIG` and `NEG_RISK_CONFIG`:

| Market | V1 | V2 |
|--------|----|----|
| Standard (chain 137) | `0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E` | `0xE111180000d2663C0091e4f400237545B87B996B` |
| NegRisk (chain 137) | `0xC5d563A36AE78145C45a50134d48A1215220f80a` | `0xe2222d279d744050d28e00520010520000310F59` |

Collateral token address (`0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174` USDC.e) in `ContractConfig.collateral` will be updated to pUSD (`0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB`) in this same change.

### 1b. EIP-712 Domain Version (`clob/client.rs:64`)

```rust
// Before
const VERSION: Option<Cow<'static, str>> = Some(Cow::Borrowed("1"));
// After
const VERSION: Option<Cow<'static, str>> = Some(Cow::Borrowed("2"));
```

### 1c. Order Struct Field Changes (`clob/types/mod.rs`)

**Remove from `sol!` struct `Order`:**
- `address taker`
- `uint256 expiration`
- `uint256 nonce`
- `uint256 feeRateBps`

**Add to `sol!` struct `Order`:**
- `uint64 timestamp` — milliseconds since Unix epoch
- `bytes32 metadata` — default zero
- `address builder` — default zero address

Update `OrderWithSignature` serialization helper to match. Remove the four deleted fields; add `timestamp`, `metadata`, `builder`.

### 1d. Order Builder (`clob/order_builder.rs`)

**Remove from `OrderBuilder` struct:**
- `nonce: Option<u64>`
- `expiration: Option<DateTime<Utc>>`
- `taker: Option<Address>`

**Remove builder methods:** `.nonce()`, `.expiration()`, `.taker()`

**Remove from `build()` logic:**
- `fee_rate_bps` API call (fees now determined by protocol at match time)
- `nonce`, `expiration`, `taker` defaulting logic
- GTD expiration validation (GTD order type may need re-evaluation)

**Add to `build()` logic:**
- `timestamp: now_ms()` — `SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64`
- `metadata: B256::ZERO`
- `builder: Address::ZERO`

Remove `fee_rate_bps()` cache method from `Client` if it's only used by the order builder.

---

## Group 2: Connectivity Layer (after Group 1)

**Files:** `crates/ploy-connectivity/src/lib.rs`, `Cargo.toml`

### 2a. Enable Heartbeats Feature

```toml
# Cargo.toml
polymarket-client-sdk = { path = "../../vendor/polymarket-client-sdk", features = ["clob", "heartbeats"] }
```

### 2b. Cache Authenticated Client

Current problem: `submit()`, `cancel()`, `reconcile_fills()` each create a new `Client` and call `authenticate()` — this is one signing operation + one API round-trip per order operation.

**Solution:** Introduce a `ClientPool` or `OnceLock<Client<Authenticated<K>>>` that lazily initializes on first use and reuses across calls. Since `Client<Authenticated<K>>` is `Clone` (or `Arc`-wrapped), share it across the three methods.

```rust
// Sketch
struct PolymarketConnector {
    config: ConnectorConfig,
    client: Arc<OnceLock<Client<Authenticated<LocalSigner>>>>,
}
```

On first call, authenticate and store. On subsequent calls, reuse. If the client returns an auth error (401), invalidate and re-authenticate once.

### 2c. Start Heartbeats for GTC Orders

After client initialization, call `Client::start_heartbeats()` so GTC orders maintain book priority.

---

## Group 3: Market-Data + Claimer Layer (after Group 1)

**Files:** `crates/ploy-market-data/Cargo.toml`, `crates/ploy-claimer/src/lib.rs`, `crates/ploy-market-data/src/collector.rs`

### 3a. Enable Heartbeats in market-data

```toml
# ploy-market-data/Cargo.toml
polymarket-client-sdk = { path = "../../vendor/polymarket-client-sdk", features = ["clob", "gamma", "rtds", "tracing", "ws", "heartbeats"] }
```

### 3b. Update Collateral Token in Claimer (`ploy-claimer/src/lib.rs:41`)

Replace `USDC_E_POLYGON` with pUSD address. The pUSD contract address on Polygon is confirmed from V2 migration docs. Update the constant name and all references.

```rust
// Before
pub(crate) const USDC_E_POLYGON: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
// After — pUSD (Polymarket USD) on Polygon (proxy)
pub(crate) const PUSD_POLYGON: &str = "0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB";
// Collateral Onramp — call wrap() to convert USDC.e → pUSD
pub(crate) const COLLATERAL_ONRAMP: &str = "0x93070a847efEf7F70739046A929D47a521F5B8ee";
```

Also update `relayer.rs` and any other files referencing `USDC_E_POLYGON`.

### 3c. Replace Raw reqwest in collector.rs

`collector.rs:119` uses a bare `reqwest::Client` to call `https://gamma-api.polymarket.com/markets`. Replace with the SDK `GammaClient` for type safety and consistent error handling.

---

## Execution Order

```
Group 1 (SDK layer)
    ↓ compile check passes
Group 2 (connectivity)  ←→  Group 3 (market-data + claimer)  [parallel]
    ↓
Full workspace compile + cargo test
```

## Definition of Done

- `cargo build --workspace` passes with no errors
- `cargo test --workspace` passes
- EIP-712 domain version is `"2"` in signing path
- V2 contract addresses are in `CONFIG` and `NEG_RISK_CONFIG`
- Order struct has no `nonce/feeRateBps/taker/expiration` fields
- `ploy-connectivity` reuses authenticated client across operations
- Heartbeats enabled in `ploy-connectivity` and `ploy-market-data`
- `ploy-claimer` references pUSD, not USDC.e
