# Polymarket V2 Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate the Polymarket trading system from CLOB V1 to V2 before the April 28 cutover — updating contract addresses, EIP-712 domain, order struct, order builder, client caching, heartbeats, and collateral token.

**Architecture:** Three groups executed in dependency order: Group 1 (SDK layer) must compile first, then Group 2 (connectivity) and Group 3 (market-data + claimer) can proceed in parallel. The SDK is a vendored crate at `vendor/polymarket-client-sdk/`. Downstream crates consume it via path dependencies.

**Tech Stack:** Rust, alloy (EIP-712 / Solidity types), polymarket-client-sdk (vendored), tokio, reqwest, sqlx

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `vendor/polymarket-client-sdk/src/lib.rs` | Modify | V2 exchange + pUSD collateral addresses |
| `vendor/polymarket-client-sdk/src/clob/client.rs` | Modify | EIP-712 domain version "1" → "2", remove `fee_rate_bps()` method + cache |
| `vendor/polymarket-client-sdk/src/clob/types/mod.rs` | Modify | V2 Order struct fields, OrderWithSignature, SignedOrder serialization |
| `vendor/polymarket-client-sdk/src/clob/order_builder.rs` | Modify | Remove nonce/expiration/taker, add timestamp/metadata/builder |
| `crates/ploy-connectivity/Cargo.toml` | Modify | Add `heartbeats` feature |
| `crates/ploy-connectivity/src/lib.rs` | Modify | Cache authenticated client, start heartbeats |
| `crates/ploy-market-data/Cargo.toml` | Modify | Add `heartbeats` feature |
| `crates/ploy-claimer/src/lib.rs` | Modify | USDC_E_POLYGON → PUSD_POLYGON constant |
| `crates/ploy-claimer/src/claim_flow.rs` | Modify | Update collateral address reference + error message |
| `crates/ploy-claimer/src/relayer/proxy_support.rs` | Modify | Update collateral address reference + error message |
| `crates/ploy-claimer/src/relayer/tests.rs` | Modify | Update test address reference |
| `crates/ploy-market-data/src/collector.rs` | Modify | Replace raw reqwest with SDK GammaClient |

---

## Task 1: Update V2 Contract Addresses and Collateral Token (SDK lib.rs)

**Files:**
- Modify: `vendor/polymarket-client-sdk/src/lib.rs:59-87`

- [ ] **Step 1: Update CONFIG exchange address for chain 137**

In `vendor/polymarket-client-sdk/src/lib.rs`, change the `CONFIG` phf_map entry for chain `137`:

```rust
// line 61: change exchange address
exchange: address!("0xE111180000d2663C0091e4f400237545B87B996B"),
// line 62: change collateral from USDC.e to pUSD
collateral: address!("0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB"),
```

Leave chain `80002` (Amoy testnet) unchanged — V2 testnet addresses are not yet published.

- [ ] **Step 2: Update NEG_RISK_CONFIG exchange address for chain 137**

In the same file, change the `NEG_RISK_CONFIG` phf_map entry for chain `137`:

```rust
// line 76: change exchange address
exchange: address!("0xe2222d279d744050d28e00520010520000310F59"),
// line 77: change collateral from USDC.e to pUSD
collateral: address!("0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB"),
```

Leave chain `80002` unchanged.

- [ ] **Step 3: Verify the SDK crate compiles**

Run: `cargo check -p polymarket-client-sdk`
Expected: compiles with no errors (addresses are compile-time constants via `address!` macro)

- [ ] **Step 4: Commit**

```bash
git add vendor/polymarket-client-sdk/src/lib.rs
git commit -m "feat(sdk): update V2 exchange addresses and pUSD collateral for Polygon mainnet"
```

---

## Task 2: Update EIP-712 Domain Version (SDK client.rs)

**Files:**
- Modify: `vendor/polymarket-client-sdk/src/clob/client.rs:64`

- [ ] **Step 1: Change VERSION constant from "1" to "2"**

In `vendor/polymarket-client-sdk/src/clob/client.rs` line 64:

```rust
// Before
const VERSION: Option<Cow<'static, str>> = Some(Cow::Borrowed("1"));
// After
const VERSION: Option<Cow<'static, str>> = Some(Cow::Borrowed("2"));
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p polymarket-client-sdk`
Expected: compiles with no errors

- [ ] **Step 3: Commit**

```bash
git add vendor/polymarket-client-sdk/src/clob/client.rs
git commit -m "feat(sdk): bump EIP-712 domain version to 2 for V2 exchange"
```

---

## Task 3: Update Order Struct Fields (SDK types/mod.rs)

**Files:**
- Modify: `vendor/polymarket-client-sdk/src/clob/types/mod.rs:425-555`

- [ ] **Step 1: Replace V1 fields with V2 fields in the `sol! { struct Order }` block**

In `vendor/polymarket-client-sdk/src/clob/types/mod.rs`, replace the Order struct (lines 425-455):

```rust
sol! {
    /// Alloy solidity type representing an order in the context of the Polymarket exchange
    #[non_exhaustive]
    #[serde_as]
    #[derive(Serialize, Debug, Default, PartialEq)]
    struct Order {
        #[serde(serialize_with = "ser_salt")]
        uint256 salt;
        address maker;
        address signer;
        #[serde_as(as = "DisplayFromStr")]
        uint256 tokenId;
        #[serde_as(as = "DisplayFromStr")]
        uint256 makerAmount;
        #[serde_as(as = "DisplayFromStr")]
        uint256 takerAmount;
        uint64  timestamp;
        bytes32 metadata;
        address builder;
        uint8   side;
        uint8   signatureType;
    }
}
```

Removed: `taker`, `expiration`, `nonce`, `feeRateBps`.
Added: `timestamp` (u64), `metadata` (B256/bytes32), `builder` (Address).

- [ ] **Step 2: Update `OrderWithSignature` serialization helper**

Replace the `OrderWithSignature` struct (lines 488-518):

```rust
#[serde_as]
#[derive(Serialize)]
struct OrderWithSignature<'order> {
    #[serde(serialize_with = "ser_salt")]
    salt: &'order U256,
    maker: &'order alloy::primitives::Address,
    signer: &'order alloy::primitives::Address,
    #[serde_as(as = "DisplayFromStr")]
    #[serde(rename = "tokenId")]
    token_id: &'order U256,
    #[serde_as(as = "DisplayFromStr")]
    #[serde(rename = "makerAmount")]
    maker_amount: &'order U256,
    #[serde_as(as = "DisplayFromStr")]
    #[serde(rename = "takerAmount")]
    taker_amount: &'order U256,
    timestamp: u64,
    metadata: &'order alloy::primitives::B256,
    builder: &'order alloy::primitives::Address,
    /// Side serialized as "BUY"/"SELL" string (CLOB API requirement)
    side: Side,
    #[serde(rename = "signatureType")]
    signature_type: u8,
    /// Signature injected into the order object
    signature: String,
}
```

- [ ] **Step 3: Update `SignedOrder::serialize` implementation**

Replace the `OrderWithSignature` construction inside `impl Serialize for SignedOrder` (lines 530-543):

```rust
let order_with_sig = OrderWithSignature {
    salt: &self.order.salt,
    maker: &self.order.maker,
    signer: &self.order.signer,
    token_id: &self.order.tokenId,
    maker_amount: &self.order.makerAmount,
    taker_amount: &self.order.takerAmount,
    timestamp: self.order.timestamp,
    metadata: &self.order.metadata,
    builder: &self.order.builder,
    side,
    signature_type: self.order.signatureType,
    signature: self.signature.to_string(),
};
```

- [ ] **Step 4: Verify compilation**

Run: `cargo check -p polymarket-client-sdk`
Expected: compilation errors in `order_builder.rs` (expected — Task 4 fixes those)

- [ ] **Step 5: Commit**

```bash
git add vendor/polymarket-client-sdk/src/clob/types/mod.rs
git commit -m "feat(sdk): update Order struct to V2 fields (timestamp/metadata/builder)"
```

---

## Task 4: Update Order Builder — Remove V1 Fields, Add V2 Fields (SDK order_builder.rs)

**Files:**
- Modify: `vendor/polymarket-client-sdk/src/clob/order_builder.rs:37-469`

- [ ] **Step 1: Remove V1 fields from `OrderBuilder` struct**

In `vendor/polymarket-client-sdk/src/clob/order_builder.rs`, remove these fields from the `OrderBuilder` struct (lines 48-50):

```rust
// DELETE these three lines:
pub(crate) nonce: Option<u64>,
pub(crate) expiration: Option<DateTime<Utc>>,
pub(crate) taker: Option<Address>,
```

Also remove the `chrono::{DateTime, Utc}` import if it becomes unused.

- [ ] **Step 2: Remove V1 builder methods**

Delete the `.nonce()`, `.expiration()`, and `.taker()` builder methods (lines 73-88):

```rust
// DELETE these three methods entirely:
pub fn nonce(mut self, nonce: u64) -> Self { ... }
pub fn expiration(mut self, expiration: DateTime<Utc>) -> Self { ... }
pub fn taker(mut self, taker: Address) -> Self { ... }
```

- [ ] **Step 3: Update limit order `build()` method (lines 124-259)**

Remove the V1 defaulting logic (lines 193-203):

```rust
// DELETE:
let nonce = self.nonce.unwrap_or(0);
let expiration = self.expiration.unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
let taker = self.taker.unwrap_or(Address::ZERO);
// ...
if !matches!(order_type, OrderType::GTD) && expiration > DateTime::<Utc>::UNIX_EPOCH { ... }
```

Remove the `fee_rate_bps` API call (line 149):

```rust
// DELETE:
let fee_rate = self.client.fee_rate_bps(token_id).await?;
```

Replace the Order construction (lines 234-249) with V2 fields:

```rust
use std::time::{SystemTime, UNIX_EPOCH};

let timestamp = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .expect("system clock before UNIX epoch")
    .as_millis() as u64;

let order = Order {
    salt: U256::from(salt),
    maker: self.funder.unwrap_or(self.signer),
    tokenId: token_id,
    makerAmount: U256::from(to_fixed_u128(maker_amount)),
    takerAmount: U256::from(to_fixed_u128(taker_amount)),
    side: side as u8,
    signer: self.signer,
    timestamp,
    metadata: alloy::primitives::B256::ZERO,
    builder: Address::ZERO,
    signatureType: self.signature_type as u8,
};
```

Also remove `feeRateBps: U256::from(fee_rate.base_fee)` from the construction.

- [ ] **Step 4: Update market order `build()` method (lines 348-469)**

Apply the same changes to the market order builder:

Remove V1 defaulting (lines 365-366):
```rust
// DELETE:
let nonce = self.nonce.unwrap_or(0);
let taker = self.taker.unwrap_or(Address::ZERO);
```

Remove fee_rate_bps call (line 386):
```rust
// DELETE:
let fee_rate = self.client.fee_rate_bps(token_id).await?;
```

Replace Order construction (lines 446-459):

```rust
let timestamp = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .expect("system clock before UNIX epoch")
    .as_millis() as u64;

let order = Order {
    salt: U256::from(salt),
    maker: self.funder.unwrap_or(self.signer),
    tokenId: token_id,
    makerAmount: U256::from(to_fixed_u128(maker_amount)),
    takerAmount: U256::from(to_fixed_u128(taker_amount)),
    side: side as u8,
    signer: self.signer,
    timestamp,
    metadata: alloy::primitives::B256::ZERO,
    builder: Address::ZERO,
    signatureType: self.signature_type as u8,
};
```

- [ ] **Step 5: Update any `OrderBuilder` construction sites that set removed fields**

Search for `.nonce(`, `.expiration(`, `.taker(` calls on `OrderBuilder` across the workspace. Remove them. These fields no longer exist.

Run: `rg '\.nonce\(|\.expiration\(|\.taker\(' --type rust`

- [ ] **Step 6: Remove `fee_rate_bps()` method from client.rs if only used by order builder**

In `vendor/polymarket-client-sdk/src/clob/client.rs`:
- The `fee_rate_bps()` method (lines 837-863) is called only from `order_builder.rs`.
- The `fee_rate_bps` DashMap cache field (line 403) and its `clear()` call (line 510) can be removed.
- The `set_fee_rate_bps()` method (line 574) can be removed.
- Remove the `FeeRateResponse` import if it becomes unused.

Check first: `rg 'fee_rate_bps' --type rust` — if no callers remain outside the SDK, remove it. If external crates use it, keep it.

- [ ] **Step 7: Verify full SDK compilation**

Run: `cargo check -p polymarket-client-sdk`
Expected: compiles with no errors

- [ ] **Step 8: Run SDK tests**

Run: `cargo test -p polymarket-client-sdk`
Expected: all tests pass (some tests may need updating if they construct `Order` directly)

- [ ] **Step 9: Commit**

```bash
git add vendor/polymarket-client-sdk/
git commit -m "feat(sdk): update OrderBuilder to V2 — remove nonce/expiration/taker/feeRateBps, add timestamp/metadata/builder"
```

---

## Task 5: Cache Authenticated Client in Connectivity (ploy-connectivity)

**Files:**
- Modify: `crates/ploy-connectivity/Cargo.toml:12`
- Modify: `crates/ploy-connectivity/src/lib.rs`

- [ ] **Step 1: Enable heartbeats feature in Cargo.toml**

In `crates/ploy-connectivity/Cargo.toml` line 12:

```toml
# Before
polymarket-client-sdk = { path = "../../vendor/polymarket-client-sdk", features = ["clob"] }
# After
polymarket-client-sdk = { path = "../../vendor/polymarket-client-sdk", features = ["clob", "heartbeats"] }
```

- [ ] **Step 2: Add `OnceLock` + `Arc` client caching to `PolymarketExecutionGateway`**

In `crates/ploy-connectivity/src/lib.rs`, restructure `PolymarketExecutionGateway` to cache the authenticated client:

```rust
use std::sync::{Arc, OnceLock};
use polymarket_client_sdk::auth::Authenticated;
use polymarket_client_sdk::clob::Client as ClobClient;

#[derive(Debug, Clone)]
pub struct PolymarketExecutionGateway {
    config: PolymarketExecutionConfig,
    client: Arc<OnceLock<ClobClient<Authenticated<LocalSigner>>>>,
}
```

Update `from_env()` and `new()`:

```rust
impl PolymarketExecutionGateway {
    pub fn from_env() -> Self {
        Self {
            config: PolymarketExecutionConfig::from_env(),
            client: Arc::new(OnceLock::new()),
        }
    }

    pub fn new(config: PolymarketExecutionConfig) -> Self {
        Self {
            config,
            client: Arc::new(OnceLock::new()),
        }
    }
}
```

- [ ] **Step 3: Extract a shared `get_or_init_client()` helper**

Add a private method that lazily authenticates once and reuses:

```rust
impl PolymarketExecutionGateway {
    fn get_or_init_client(
        &self,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<ClobClient<Authenticated<LocalSigner>>, ExecutionError> {
        if let Some(client) = self.client.get() {
            return Ok(client.clone());
        }

        let private_key = self
            .config
            .private_key
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .ok_or_else(|| {
                ExecutionError::Configuration(format!("{PRIVATE_KEY_VAR} is not configured"))
            })?;
        let funder = self
            .config
            .funder
            .as_deref()
            .map(Address::from_str)
            .transpose()
            .map_err(|err| ExecutionError::Configuration(format!("invalid POLY_FUNDER: {err}")))?;

        let client = runtime.block_on(async {
            let signer = LocalSigner::from_str(private_key)
                .map_err(|err| {
                    ExecutionError::Configuration(format!("invalid {PRIVATE_KEY_VAR}: {err}"))
                })?
                .with_chain_id(Some(POLYGON));

            let client = polymarket_client_sdk::clob::Client::new(
                &self.config.host,
                Config::builder()
                    .use_server_time(self.config.use_server_time)
                    .build(),
            )
            .map_err(|err| ExecutionError::Transport(format!("build client: {err}")))?;

            let mut auth = client.authentication_builder(&signer);
            auth = auth.signature_type(self.config.signature_type.into_sdk());
            if let Some(funder) = funder {
                auth = auth.funder(funder);
            }

            auth.authenticate()
                .await
                .map_err(|err| ExecutionError::Transport(format!("authenticate client: {err}")))
        })?;

        // OnceLock::set may fail if another thread raced — that's fine, use get()
        let _ = self.client.set(client);
        Ok(self.client.get().expect("just set").clone())
    }
}
```

Note: The `heartbeats` feature auto-starts heartbeats during `authenticate()` (see SDK `client.rs:240-241`), so no explicit `start_heartbeats()` call is needed.

- [ ] **Step 4: Refactor `submit()` to use cached client**

Replace the client creation block in `submit()` (lines 331-360) with:

```rust
fn submit(&self, request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
    let limit_price = request.limit_price.ok_or_else(|| {
        ExecutionError::Validation(
            "live Polymarket execution currently requires a limit price".to_string(),
        )
    })?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| ExecutionError::Transport(format!("create tokio runtime: {err}")))?;

    let client = self.get_or_init_client(&runtime)?;

    runtime.block_on(async {
        let token_id = U256::from_str(&request.token_id).map_err(|err| {
            ExecutionError::Validation(format!("invalid token_id `{}`: {err}", request.token_id))
        })?;

        let side = polymarket_side(request.side);
        let order = match request.order_type {
            // ... existing order building logic unchanged ...
        }
        .map_err(|err| ExecutionError::Validation(format!("build limit order: {err}")))?;

        // Sign requires a signer — reconstruct from env
        let private_key = self
            .config
            .private_key
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .ok_or_else(|| {
                ExecutionError::Configuration(format!("{PRIVATE_KEY_VAR} is not configured"))
            })?;
        let signer = LocalSigner::from_str(private_key)
            .map_err(|err| {
                ExecutionError::Configuration(format!("invalid {PRIVATE_KEY_VAR}: {err}"))
            })?
            .with_chain_id(Some(POLYGON));

        let signed_order = client
            .sign(&signer, order)
            .await
            .map_err(|err| ExecutionError::Transport(format!("sign order: {err}")))?;

        let response = client
            .post_order(signed_order)
            .await
            .map_err(|err| ExecutionError::Transport(format!("submit order: {err}")))?;

        if response.success {
            Ok(ExecutionOutcome::Acknowledged {
                venue_order_id: response.order_id,
            })
        } else {
            Ok(ExecutionOutcome::Rejected {
                reason: response.error_msg.unwrap_or_else(|| {
                    format!("venue rejected order with status {}", response.status)
                }),
            })
        }
    })
}
```

- [ ] **Step 5: Refactor `reconcile_fills()` to use cached client**

Same pattern — replace the client creation block (lines 445-474) with `self.get_or_init_client(&runtime)?`.

- [ ] **Step 6: Refactor `cancel()` to use cached client**

Same pattern — replace the client creation block (lines 525-554) with `self.get_or_init_client(&runtime)?`.

- [ ] **Step 7: Verify compilation**

Run: `cargo check -p ploy-connectivity`
Expected: compiles with no errors

- [ ] **Step 8: Commit**

```bash
git add crates/ploy-connectivity/
git commit -m "feat(connectivity): cache authenticated client and enable heartbeats for V2"
```

---

## Task 6: Enable Heartbeats in Market-Data (ploy-market-data)

**Files:**
- Modify: `crates/ploy-market-data/Cargo.toml:14`

- [ ] **Step 1: Add heartbeats feature**

In `crates/ploy-market-data/Cargo.toml` line 14:

```toml
# Before
polymarket-client-sdk = { path = "../../vendor/polymarket-client-sdk", features = ["clob", "gamma", "rtds", "tracing", "ws"] }
# After
polymarket-client-sdk = { path = "../../vendor/polymarket-client-sdk", features = ["clob", "gamma", "rtds", "tracing", "ws", "heartbeats"] }
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p ploy-market-data`
Expected: compiles with no errors

- [ ] **Step 3: Commit**

```bash
git add crates/ploy-market-data/Cargo.toml
git commit -m "feat(market-data): enable heartbeats feature for V2"
```

---

## Task 7: Update Collateral Token in Claimer (ploy-claimer)

**Files:**
- Modify: `crates/ploy-claimer/src/lib.rs:41`
- Modify: `crates/ploy-claimer/src/claim_flow.rs:310`
- Modify: `crates/ploy-claimer/src/relayer/proxy_support.rs:157`
- Modify: `crates/ploy-claimer/src/relayer/tests.rs:142`

- [ ] **Step 1: Rename constant and update address in lib.rs**

In `crates/ploy-claimer/src/lib.rs` line 41:

```rust
// Before
pub(crate) const USDC_E_POLYGON: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
// After
pub(crate) const PUSD_POLYGON: &str = "0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB";
/// Collateral onramp contract — call wrap() to convert USDC.e → pUSD (not yet wired)
pub(crate) const COLLATERAL_ONRAMP: &str = "0x93070a847efEf7F70739046A929D47a521F5B8ee";
```

- [ ] **Step 2: Update claim_flow.rs reference**

In `crates/ploy-claimer/src/claim_flow.rs` line 310:

```rust
// Before
let collateral_addr: Address = USDC_E_POLYGON
    .parse()
    .map_err(|e| ClaimerError::Network(format!("Invalid USDC.e address: {}", e)))?;
// After
let collateral_addr: Address = PUSD_POLYGON
    .parse()
    .map_err(|e| ClaimerError::Network(format!("Invalid pUSD address: {}", e)))?;
```

- [ ] **Step 3: Update relayer/proxy_support.rs reference**

In `crates/ploy-claimer/src/relayer/proxy_support.rs` line 157:

```rust
// Before
let usdc_addr: EthersAddress = USDC_E_POLYGON.parse().map_err(|e| {
    crate::ClaimerError::Network(format!("Invalid USDC.e address: {}", e))
})?;
// After
let pusd_addr: EthersAddress = PUSD_POLYGON.parse().map_err(|e| {
    crate::ClaimerError::Network(format!("Invalid pUSD address: {}", e))
})?;
```

Also update the `Token::Address(usdc_addr)` reference below to `Token::Address(pusd_addr)`.

- [ ] **Step 4: Update relayer/tests.rs reference**

In `crates/ploy-claimer/src/relayer/tests.rs` line 142:

```rust
// Before
let usdc: EthersAddress = USDC_E_POLYGON.parse().expect("usdc");
// After
let pusd: EthersAddress = PUSD_POLYGON.parse().expect("pusd");
```

Update any subsequent references to `usdc` variable to `pusd` in the same test.

- [ ] **Step 5: Search for any remaining USDC_E_POLYGON references**

Run: `rg 'USDC_E_POLYGON' --type rust`
Expected: zero matches

- [ ] **Step 6: Verify compilation**

Run: `cargo check -p ploy-claimer`
Expected: compiles with no errors

- [ ] **Step 7: Run claimer tests**

Run: `cargo test -p ploy-claimer`
Expected: all tests pass

- [ ] **Step 8: Commit**

```bash
git add crates/ploy-claimer/
git commit -m "feat(claimer): update collateral token from USDC.e to pUSD for V2"
```

---

## Task 8: Replace Raw reqwest with SDK GammaClient in collector.rs

**Files:**
- Modify: `crates/ploy-market-data/src/collector.rs:91-264`

- [ ] **Step 1: Add GammaClient import and remove raw endpoint constant**

In `crates/ploy-market-data/src/collector.rs`:

Add import:
```rust
use polymarket_client_sdk::gamma::Client as GammaClient;
use polymarket_client_sdk::gamma::types::request::MarketByIdRequest;
```

Remove the constant (line 119):
```rust
// DELETE:
const POLYMARKET_GAMMA_MARKET_BY_ID_ENDPOINT: &str = "https://gamma-api.polymarket.com/markets";
```

- [ ] **Step 2: Replace `fetch_official_market_settlements` function**

Replace the function (lines 235-264) to use GammaClient instead of raw reqwest:

```rust
async fn fetch_official_market_settlements(
    gamma: &GammaClient,
    market_id: &str,
) -> OfficialMarketSettlementStatus {
    let request = MarketByIdRequest::builder().id(market_id).build();
    let market = match gamma.market_by_id(&request).await {
        Ok(market) => market,
        Err(_) => return OfficialMarketSettlementStatus::Unknown,
    };

    if !market.closed.unwrap_or(false) {
        return OfficialMarketSettlementStatus::Open;
    }

    // Convert SDK Market fields to local settlement payload format
    let payload = OfficialMarketSettlementPayload {
        closed: market.closed,
        resolved_by: market.resolved_by,
        uma_resolution_status: market.uma_resolution_status,
        outcomes: market.outcomes.map(|v| serde_json::to_string(&v).unwrap_or_default()),
        outcome_prices: market.outcome_prices.map(|v| serde_json::to_string(&v).unwrap_or_default()),
        clob_token_ids: market.clob_token_ids.map(|v| {
            v.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",")
        }),
    };

    match parse_official_market_settlements(&payload) {
        Some(settlements) => OfficialMarketSettlementStatus::Closed(settlements),
        None => OfficialMarketSettlementStatus::Unknown,
    }
}
```

Note: The SDK `Market` type has `outcomes: Option<Vec<String>>` and `outcome_prices: Option<Vec<Decimal>>`, while the local `OfficialMarketSettlementPayload` expects `Option<String>` (JSON-encoded). We bridge by serializing back to JSON strings. This preserves the existing `parse_official_market_settlements` logic unchanged.

- [ ] **Step 3: Update the caller in `spawn_settlement_collector`**

Find where `fetch_official_market_settlements` is called (around line 648). The caller currently passes `&reqwest::Client`. Change it to pass a `&GammaClient` instead.

The `GammaClient` should be created once at the start of the settlement collector:

```rust
let gamma = GammaClient::default();
```

Then pass `&gamma` instead of `&http` to `fetch_official_market_settlements`.

If the `reqwest::Client` (`http`) is only used for this one call, remove it entirely. If it's used elsewhere in the collector, keep it.

- [ ] **Step 4: Verify compilation**

Run: `cargo check -p ploy-market-data`
Expected: compiles with no errors

- [ ] **Step 5: Run market-data tests**

Run: `cargo test -p ploy-market-data`
Expected: all tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/ploy-market-data/src/collector.rs
git commit -m "refactor(market-data): replace raw reqwest with SDK GammaClient for settlement checks"
```

---

## Task 9: Full Workspace Verification

**Files:** None (verification only)

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: compiles with no errors

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: all tests pass

- [ ] **Step 3: Verify Definition of Done checklist**

Manually verify each item:

1. EIP-712 domain version is `"2"`:
   Run: `rg 'Borrowed\("2"\)' vendor/polymarket-client-sdk/src/clob/client.rs`

2. V2 contract addresses in CONFIG and NEG_RISK_CONFIG:
   Run: `rg '0xE111180000d2663C0091e4f400237545B87B996B' vendor/polymarket-client-sdk/src/lib.rs`
   Run: `rg '0xe2222d279d744050d28e00520010520000310F59' vendor/polymarket-client-sdk/src/lib.rs`

3. Order struct has no V1 fields:
   Run: `rg 'nonce|feeRateBps|taker|expiration' vendor/polymarket-client-sdk/src/clob/types/mod.rs`
   Expected: no matches in the `sol! { struct Order }` block

4. Connectivity reuses authenticated client:
   Run: `rg 'OnceLock' crates/ploy-connectivity/src/lib.rs`

5. Heartbeats enabled:
   Run: `rg 'heartbeats' crates/ploy-connectivity/Cargo.toml crates/ploy-market-data/Cargo.toml`

6. Claimer references pUSD:
   Run: `rg 'PUSD_POLYGON' crates/ploy-claimer/`
   Run: `rg 'USDC_E_POLYGON' crates/ploy-claimer/` (should be zero)

- [ ] **Step 4: Commit any remaining fixes**

If any verification step fails, fix and commit atomically.

---

## Execution Order Summary

```
Task 1 (addresses) → Task 2 (EIP-712) → Task 3 (Order struct) → Task 4 (OrderBuilder)
    ↓ SDK compiles
Task 5 (connectivity client cache + heartbeats)  ←→  Task 6 (market-data heartbeats)
                                                  ←→  Task 7 (claimer pUSD)
                                                  ←→  Task 8 (collector GammaClient)
    ↓
Task 9 (full workspace verification)
```

Tasks 5-8 are independent of each other and can run in parallel after Tasks 1-4 complete.
