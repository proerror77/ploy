"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const {
  buildOrderPlan,
  buildRedeemPlan,
  executeOrderPlan,
  executeRedeemPlan,
  loadWalletSecret,
  validatePlan,
} = require("./account_ops");

const MARKET = {
  id: 42,
  conditionId: `0x${"ab".repeat(32)}`,
  decimalPrecision: 3,
  feeRateBps: 200,
  isNegRisk: true,
  isYieldBearing: true,
  outcomes: [
    { name: "Yes", indexSet: 1, onChainId: "111" },
    { name: "No", indexSet: 2, onChainId: "222" },
  ],
};
const CONTEXT = {
  account: "0x1111111111111111111111111111111111111111",
  accountType: "PREDICT_ACCOUNT",
  chainId: 56,
  releaseSha: "a".repeat(40),
};

test("order plan binds official market route, account, expiry, and exact hash", () => {
  const plan = buildOrderPlan(MARKET, {
    marketId: 42,
    tokenId: "111",
    side: "BUY",
    quantity: "10",
    limitPrice: "0.425",
  }, CONTEXT, 1_700_000_000_000);

  assert.equal(plan.kind, "predict_fun_limit_order");
  assert.equal(plan.market.isNegRisk, true);
  assert.equal(plan.order.feeRateBps, 200);
  assert.equal(plan.expiresAt, "2023-11-14T22:23:20.000Z");
  assert.equal(validatePlan(plan, CONTEXT, plan.sha256, 1_700_000_001_000), plan);
  assert.throws(() => validatePlan(plan, CONTEXT, "0".repeat(64)), /plan hash mismatch/);
});

test("order plan rejects a token or price not supported by the market", () => {
  assert.throws(() => buildOrderPlan(MARKET, {
    marketId: 42, tokenId: "999", side: "BUY", quantity: "1", limitPrice: "0.5",
  }, CONTEXT), /tokenId is not an outcome/);
  assert.throws(() => buildOrderPlan(MARKET, {
    marketId: 42, tokenId: "111", side: "BUY", quantity: "1", limitPrice: "0.4251",
  }, CONTEXT), /decimal precision/);
});

test("redeem plan keeps standard and neg-risk amount semantics distinct", () => {
  const positions = [{
    amount: "2500000000000000000",
    market: { ...MARKET, resolution: { indexSet: 1, status: "WON" } },
    outcome: MARKET.outcomes[0],
  }];
  const plan = buildRedeemPlan(positions, CONTEXT, 1_700_000_000_000);
  assert.equal(plan.items.length, 1);
  assert.equal(plan.items[0].amount, "2500000000000000000");
  assert.equal(plan.items[0].isNegRisk, true);
  assert.equal(plan.items[0].indexSet, 1);
});

test("wallet secret requires exactly one injected source and never accepts loose files", () => {
  assert.throws(() => loadWalletSecret({}), /exactly one/);
  assert.throws(() => loadWalletSecret({
    PREDICT_FUN_PRIVATE_KEY: `0x${"11".repeat(32)}`,
    PREDICT_FUN_PRIVATE_KEY_FILE: "/tmp/key",
  }), /exactly one/);
});

function runtimeEnv(tmp) {
  return {
    PLOY_LIVE_ACCOUNT_ID: CONTEXT.account,
    PREDICT_FUN_ACCOUNT_TYPE: CONTEXT.accountType,
    PREDICT_FUN_CHAIN_ID: String(CONTEXT.chainId),
    PLOY_RELEASE_SHA: CONTEXT.releaseSha,
    PLOY_PREDICT_OPS_LEDGER: path.join(tmp, "ledger.jsonl"),
  };
}

function fakeOrderSession() {
  return { builder: {
    getApprovalSteps: () => [{ id: "allowance" }],
    checkApprovals: async () => [{ satisfied: true }],
    getLimitOrderAmounts: () => ({ makerAmount: 425n, takerAmount: 1000n, pricePerShare: 425n }),
    buildOrder: (_strategy, order) => ({ ...order, salt: "1", maker: CONTEXT.account, signer: CONTEXT.account }),
    buildTypedData: (order) => ({ message: order }),
    signTypedDataOrder: async ({ message }) => ({ ...message, signature: "0xsig" }),
    buildTypedDataHash: () => `0x${"cd".repeat(32)}`,
  } };
}

test("order execution is write-gated and submits one official SDK payload", async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "predict-order-"));
  const plan = buildOrderPlan(MARKET, {
    marketId: 42, tokenId: "111", side: "BUY", quantity: "10", limitPrice: "0.425",
  }, CONTEXT);
  await assert.rejects(executeOrderPlan(plan, plan.sha256, runtimeEnv(tmp)), /writes are disabled/);
  let submitted;
  const events = [];
  const result = await executeOrderPlan(plan, plan.sha256, {
    ...runtimeEnv(tmp), PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED: "true",
  }, {
    market: MARKET,
    session: fakeOrderSession(),
    jwt: "jwt",
    record: (event) => events.push(event),
    submit: async (body) => { submitted = body; return { data: { orderId: "order-1", orderHash: body.data.order.hash } }; },
  });
  assert.equal(result.orderId, "order-1");
  assert.equal(submitted.data.strategy, "LIMIT");
  assert.equal(submitted.data.order.signature, "0xsig");
  assert.deepEqual(events.map((event) => event.state), ["submitting", "submitted"]);
});

test("redemption execution routes neg-risk amount through official SDK and records receipt", async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "predict-redeem-"));
  const positions = [{
    amount: "2500000000000000000",
    market: { ...MARKET, resolution: { indexSet: 1, status: "WON" } },
    outcome: MARKET.outcomes[0],
  }];
  const plan = buildRedeemPlan(positions, CONTEXT);
  let options;
  const events = [];
  const session = { builder: {
    getApprovalSteps: () => [{ id: "neg-risk-adapter" }],
    checkApprovals: async () => [{ satisfied: true }],
    redeemPositions: async (value) => { options = value; return { success: true, receipt: { status: 1, hash: "0xtx" } }; },
  } };
  const result = await executeRedeemPlan(plan, plan.sha256, {
    ...runtimeEnv(tmp), PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED: "true",
  }, { positions, session, record: (event) => events.push(event) });
  assert.equal(options.amount, 2500000000000000000n);
  assert.equal(options.isNegRisk, true);
  assert.equal(result[0].transactionHash, "0xtx");
  assert.deepEqual(events.map((event) => event.state), ["submitting", "confirmed"]);
});
