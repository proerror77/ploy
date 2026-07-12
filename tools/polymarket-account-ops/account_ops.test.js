"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const {
  CHAIN_ID,
  CONTRACTS,
  buildPlan,
  executePlan,
  readLedger,
  reconcileTransaction,
  redeemTransaction,
  validatePlan,
} = require("./account_ops");

const account = "0x1111111111111111111111111111111111111111";
const releaseSha = "a".repeat(40);
const now = Date.parse("2026-07-12T12:00:00Z");

function position(overrides = {}) {
  return {
    proxyWallet: account,
    asset: "123",
    conditionId: `0x${"2".repeat(64)}`,
    size: "10.5",
    redeemable: true,
    negativeRisk: false,
    outcome: "Yes",
    outcomeIndex: 0,
    ...overrides,
  };
}

test("plan groups a condition and binds account, chain, release, expiry, and hash", () => {
  const plan = buildPlan(
    [position(), position({ asset: "456", outcome: "No", outcomeIndex: 1 })],
    { account, releaseSha, walletType: "SAFE" },
    now,
  );
  assert.equal(plan.chainId, CHAIN_ID);
  assert.equal(plan.account.toLowerCase(), account);
  assert.equal(plan.items.length, 1);
  assert.equal(plan.items[0].target, CONTRACTS.standardAdapter);
  assert.equal(Date.parse(plan.expiresAt) - Date.parse(plan.generatedAt), 10 * 60 * 1000);
  assert.equal(validatePlan(plan, { account, releaseSha, walletType: "SAFE" }, plan.sha256, now), plan);
});

test("validation rejects stale plans and account drift", () => {
  const plan = buildPlan([position()], { account, releaseSha, walletType: "PROXY" }, now);
  assert.throws(
    () => validatePlan(plan, { account, releaseSha, walletType: "PROXY" }, plan.sha256, now + 10 * 60 * 1000),
    /expired/,
  );
  assert.throws(
    () => validatePlan(plan, { account: "0x3333333333333333333333333333333333333333", releaseSha, walletType: "PROXY" }, plan.sha256, now),
    /account mismatch/,
  );
});

test("redeem calldata always routes through the current collateral adapter", () => {
  const plan = buildPlan([position({ negativeRisk: true })], { account, releaseSha, walletType: "SAFE" }, now);
  const transaction = redeemTransaction(plan.items[0]);
  assert.equal(transaction.to, CONTRACTS.negRiskAdapter);
  assert.equal(transaction.value, "0");
  assert.ok(transaction.data.startsWith("0x"));
});

test("plan fails closed on wallet and route disagreement", () => {
  assert.throws(
    () => buildPlan([position({ proxyWallet: "0x3333333333333333333333333333333333333333" })], { account, releaseSha, walletType: "SAFE" }, now),
    /wallet does not match/,
  );
  assert.throws(
    () => buildPlan([position(), position({ negativeRisk: true })], { account, releaseSha, walletType: "SAFE" }, now),
    /negative-risk disagreement/,
  );
});

test("execute refuses to enter account operations while writes are disabled", async () => {
  await assert.rejects(() => executePlan({}, "ignored", {}), /writes are disabled/);
});

test("reconcile clears the lock only after the exact mined transaction disappears from Data API", async (t) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "ploy-redeem-reconcile-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const ledger = path.join(directory, "redeem-ledger.jsonl");
  const lock = `${ledger}.lock`;
  fs.mkdirSync(lock);
  fs.writeFileSync(ledger, `${JSON.stringify({
    operationId: "operation-1",
    conditionId: position().conditionId,
    state: "ambiguous",
    transactionId: "relay-1",
  })}\n`);
  const env = {
    PLOY_LIVE_ACCOUNT_ID: account,
    PLOY_RELEASE_SHA: releaseSha,
    POLY_WALLET_TYPE: "SAFE",
    PLOY_REDEEM_LEDGER: ledger,
    PLOY_REDEEM_LOCK: lock,
  };
  const result = await reconcileTransaction("relay-1", env, {
    relay: {
      client: { getTransaction: async () => [{ transactionID: "relay-1", transactionHash: "0xabc", state: "STATE_MINED" }] },
      publicClient: { readContract: async () => 125n },
    },
    fetchPositions: async () => [],
    verifyPositionRoutes: async (positions) => positions,
  });
  assert.equal(result.state, "reconciled");
  assert.equal(fs.existsSync(lock), false);
  assert.equal(readLedger(ledger).at(-1).balanceAfter, "125");
});

test("reconcile retains the lock while the relayer transaction is pending", async (t) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "ploy-redeem-pending-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const ledger = path.join(directory, "redeem-ledger.jsonl");
  const lock = `${ledger}.lock`;
  fs.mkdirSync(lock);
  fs.writeFileSync(ledger, `${JSON.stringify({
    operationId: "operation-2",
    conditionId: position().conditionId,
    state: "submitted",
    transactionId: "relay-2",
  })}\n`);
  const env = {
    PLOY_LIVE_ACCOUNT_ID: account,
    PLOY_RELEASE_SHA: releaseSha,
    POLY_WALLET_TYPE: "SAFE",
    PLOY_REDEEM_LEDGER: ledger,
    PLOY_REDEEM_LOCK: lock,
  };
  await assert.rejects(
    () => reconcileTransaction("relay-2", env, {
      relay: { client: { getTransaction: async () => [{ transactionID: "relay-2", state: "STATE_EXECUTED" }] } },
    }),
    /still STATE_EXECUTED/,
  );
  assert.equal(fs.existsSync(lock), true);
  assert.equal(readLedger(ledger).at(-1).state, "ambiguous");
});
