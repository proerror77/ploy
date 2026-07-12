"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { JsonRpcProvider, Wallet, getAddress, parseUnits } = require("ethers");
const { ChainId, OrderBuilder, Side } = require("@predictdotfun/sdk");

const PLAN_TTL_MS = 10 * 60 * 1000;
const OFFICIAL_NETWORKS = Object.freeze({
  56: "https://api.predict.fun",
  97: "https://api-testnet.predict.fun",
});

function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function sha256(value) {
  return crypto.createHash("sha256").update(canonical(value)).digest("hex");
}

function normalizeAddress(value, field) {
  try {
    return getAddress(value);
  } catch {
    throw new Error(`${field} must be a valid EVM address`);
  }
}

function normalizeContext(context) {
  const chainId = Number(context.chainId);
  if (!OFFICIAL_NETWORKS[chainId]) throw new Error("chainId must be 56 or 97");
  if (!new Set(["EOA", "PREDICT_ACCOUNT"]).has(context.accountType)) {
    throw new Error("accountType must be EOA or PREDICT_ACCOUNT");
  }
  if (!/^[0-9a-f]{40}$/.test(context.releaseSha)) {
    throw new Error("releaseSha must be a lowercase 40-character commit SHA");
  }
  return {
    account: normalizeAddress(context.account, "account"),
    accountType: context.accountType,
    chainId,
    releaseSha: context.releaseSha,
  };
}

function positiveDecimal(value, field) {
  const text = String(value);
  if (!/^(?:0|[1-9]\d*)(?:\.\d+)?$/.test(text) || Number(text) <= 0) {
    throw new Error(`${field} must be a positive decimal`);
  }
  return text;
}

function price(value, precision) {
  const text = positiveDecimal(value, "limitPrice");
  if (Number(text) >= 1) throw new Error("limitPrice must be below 1");
  if ((text.split(".")[1] || "").length > precision) {
    throw new Error(`limitPrice exceeds market decimal precision ${precision}`);
  }
  return text;
}

function planEnvelope(kind, payload, context, now) {
  const normalized = normalizeContext(context);
  const unsigned = {
    version: 1,
    kind,
    ...normalized,
    generatedAt: new Date(now).toISOString(),
    expiresAt: new Date(now + PLAN_TTL_MS).toISOString(),
    ...payload,
  };
  return { ...unsigned, sha256: sha256(unsigned) };
}

function buildOrderPlan(market, input, context, now = Date.now()) {
  if (Number(input.marketId) !== Number(market.id)) throw new Error("marketId mismatch");
  const outcome = (market.outcomes || []).find((candidate) => String(candidate.onChainId) === String(input.tokenId));
  if (!outcome) throw new Error("tokenId is not an outcome of the market");
  const side = String(input.side).toUpperCase();
  if (!new Set(["BUY", "SELL"]).has(side)) throw new Error("side must be BUY or SELL");
  const marketRoute = {
    id: Number(market.id),
    conditionId: String(market.conditionId).toLowerCase(),
    decimalPrecision: Number(market.decimalPrecision),
    feeRateBps: Number(market.feeRateBps),
    isNegRisk: Boolean(market.isNegRisk),
    isYieldBearing: Boolean(market.isYieldBearing),
  };
  if (!/^0x[0-9a-f]{64}$/.test(marketRoute.conditionId)) throw new Error("conditionId must be bytes32");
  if (!Number.isInteger(marketRoute.feeRateBps) || marketRoute.feeRateBps < 0) throw new Error("invalid feeRateBps");
  return planEnvelope("predict_fun_limit_order", {
    market: marketRoute,
    order: {
      tokenId: String(outcome.onChainId),
      outcomeIndexSet: Number(outcome.indexSet),
      side,
      quantity: positiveDecimal(input.quantity, "quantity"),
      limitPrice: price(input.limitPrice, marketRoute.decimalPrecision),
      feeRateBps: marketRoute.feeRateBps,
    },
  }, context, now);
}

function buildRedeemPlan(positions, context, now = Date.now()) {
  const items = positions.flatMap((position) => {
    const market = position.market || {};
    const outcome = position.outcome || {};
    const resolution = market.resolution;
    if (!resolution || String(resolution.status).toUpperCase() !== "WON") return [];
    if (Number(outcome.indexSet) !== Number(resolution.indexSet)) return [];
    const amount = positiveDecimal(position.amount, "position.amount");
    const conditionId = String(market.conditionId).toLowerCase();
    if (!/^0x[0-9a-f]{64}$/.test(conditionId)) throw new Error("conditionId must be bytes32");
    return [{
      marketId: Number(market.id),
      conditionId,
      indexSet: Number(outcome.indexSet),
      tokenId: String(outcome.onChainId),
      amount,
      isNegRisk: Boolean(market.isNegRisk),
      isYieldBearing: Boolean(market.isYieldBearing),
    }];
  }).sort((a, b) => a.conditionId.localeCompare(b.conditionId) || a.indexSet - b.indexSet);
  return planEnvelope("predict_fun_redeem", { items }, context, now);
}

function validatePlan(plan, context, expectedHash, now = Date.now()) {
  const { sha256: embeddedHash, ...unsigned } = plan;
  const actualHash = sha256(unsigned);
  if (actualHash !== embeddedHash || actualHash !== expectedHash) throw new Error("plan hash mismatch");
  const normalized = normalizeContext(context);
  if (plan.account !== normalized.account) throw new Error("plan account mismatch");
  if (plan.accountType !== normalized.accountType) throw new Error("plan account type mismatch");
  if (plan.chainId !== normalized.chainId) throw new Error("plan chain mismatch");
  if (plan.releaseSha !== normalized.releaseSha) throw new Error("plan release mismatch");
  if (Date.parse(plan.generatedAt) > now + 30_000) throw new Error("plan generated in the future");
  if (Date.parse(plan.expiresAt) <= now || Date.parse(plan.expiresAt) - Date.parse(plan.generatedAt) > PLAN_TTL_MS) {
    throw new Error("plan expired or exceeds maximum TTL");
  }
  return plan;
}

function loadWalletSecret(env = process.env) {
  const direct = env.PREDICT_FUN_PRIVATE_KEY;
  const file = env.PREDICT_FUN_PRIVATE_KEY_FILE;
  if (Boolean(direct) === Boolean(file)) throw new Error("exactly one wallet secret source is required");
  if (direct) return direct.trim();
  const stat = fs.statSync(file);
  if (!stat.isFile() || (stat.mode & 0o077) !== 0 || (typeof process.getuid === "function" && stat.uid !== process.getuid())) {
    throw new Error("wallet secret file must be owned by the process user with mode 0600");
  }
  return fs.readFileSync(file, "utf8").trim();
}

function runtimeContext(env = process.env) {
  return normalizeContext({
    account: requireEnv("PLOY_LIVE_ACCOUNT_ID", env),
    accountType: requireEnv("PREDICT_FUN_ACCOUNT_TYPE", env).toUpperCase(),
    chainId: Number(requireEnv("PREDICT_FUN_CHAIN_ID", env)),
    releaseSha: requireEnv("PLOY_RELEASE_SHA", env),
  });
}

function requireEnv(name, env = process.env) {
  const value = env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function apiHeaders(context, env, jwt) {
  const headers = { accept: "application/json" };
  if (context.chainId === 56) headers["x-api-key"] = requireEnv("PREDICT_FUN_API_KEY", env);
  if (jwt) headers.authorization = `Bearer ${jwt}`;
  return headers;
}

async function apiRequest(context, env, pathname, options = {}, fetchImpl = fetch) {
  const url = new URL(pathname, OFFICIAL_NETWORKS[context.chainId]);
  const { jwt, headers, ...requestOptions } = options;
  const response = await fetchImpl(url, {
    ...requestOptions,
    redirect: "error",
    signal: AbortSignal.timeout(20_000),
    headers: { ...apiHeaders(context, env, jwt), ...(headers || {}) },
  });
  if (!response.ok) throw new Error(`Predict API ${pathname} failed: HTTP ${response.status}`);
  const payload = await response.json();
  if (!payload || payload.success !== true) throw new Error(`Predict API ${pathname} returned success=false`);
  return payload;
}

async function fetchMarket(marketId, context, env = process.env, fetchImpl = fetch) {
  return (await apiRequest(context, env, `/v1/markets/${Number(marketId)}`, {}, fetchImpl)).data;
}

async function fetchPositions(context, env = process.env, fetchImpl = fetch) {
  const all = [];
  let after;
  for (let page = 0; page < 100; page += 1) {
    const query = new URLSearchParams({ first: "100", isResolved: "true" });
    if (after) query.set("after", after);
    const payload = await apiRequest(context, env, `/v1/positions/${context.account}?${query}`, {}, fetchImpl);
    all.push(...payload.data);
    if (!payload.cursor || payload.cursor === after) return all;
    after = payload.cursor;
  }
  throw new Error("Predict positions exceeded the 10000-position safety limit");
}

async function makeWalletSession(context, env = process.env) {
  const provider = new JsonRpcProvider(requireEnv("PREDICT_FUN_RPC_URL", env), context.chainId, { staticNetwork: true });
  provider.pollingInterval = 300;
  const signer = new Wallet(loadWalletSecret(env), provider);
  const options = context.accountType === "PREDICT_ACCOUNT" ? { predictAccount: context.account } : undefined;
  const builder = await OrderBuilder.make(
    context.chainId === 56 ? ChainId.BnbMainnet : ChainId.BnbTestnet,
    signer,
    options,
  );
  const principal = context.accountType === "PREDICT_ACCOUNT" ? context.account : signer.address;
  if (normalizeAddress(principal, "wallet principal") !== context.account) {
    throw new Error("wallet signer does not match PLOY_LIVE_ACCOUNT_ID");
  }
  return { builder, signer };
}

async function authenticate(session, context, env = process.env, fetchImpl = fetch) {
  const challenge = await apiRequest(context, env, "/v1/auth/message", {}, fetchImpl);
  const message = challenge.data.message;
  const signature = context.accountType === "PREDICT_ACCOUNT"
    ? await session.builder.signPredictAccountMessage(message)
    : await session.signer.signMessage(message);
  const payload = await apiRequest(context, env, "/v1/auth", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ signer: context.account, signature, message }),
  }, fetchImpl);
  if (!payload.data || !payload.data.token) throw new Error("Predict auth response has no token");
  return payload.data.token;
}

function approvalScope(plan) {
  if (plan.kind === "predict_fun_limit_order") {
    return {
      operation: "TRADE",
      isNegRisk: plan.market.isNegRisk,
      isYieldBearing: plan.market.isYieldBearing,
      side: plan.order.side === "BUY" ? Side.BUY : Side.SELL,
    };
  }
  if (plan.kind === "predict_fun_redeem") {
    return plan.items.map((item) => ({
      operation: "REDEEM",
      isNegRisk: item.isNegRisk,
      isYieldBearing: item.isYieldBearing,
    }));
  }
  throw new Error(`unsupported plan kind ${plan.kind}`);
}

function dedupeSteps(steps) {
  return [...new Map(steps.map((step) => [step.id, step])).values()];
}

async function checkApprovals(plan, session) {
  const scope = approvalScope(plan);
  const scopes = Array.isArray(scope) ? scope : [scope];
  const steps = dedupeSteps(scopes.flatMap((scope) => session.builder.getApprovalSteps(scope)));
  return { steps, checks: steps.length === 0 ? [] : await session.builder.checkApprovals(steps) };
}

async function executeApprovals(plan, expectedHash, env = process.env, dependencies = {}) {
  if (env.PLOY_PREDICT_APPROVAL_WRITE_ENABLED !== "true") throw new Error("Predict approval writes are disabled");
  const context = runtimeContext(env);
  validatePlan(plan, context, expectedHash);
  const session = dependencies.session || await makeWalletSession(context, env);
  const scope = approvalScope(plan);
  const scopes = Array.isArray(scope) ? scope : [scope];
  const steps = dedupeSteps(scopes.flatMap((scope) => session.builder.getApprovalSteps(scope)));
  if (steps.length === 0) return { success: true, steps: [] };
  const report = await session.builder.runApprovals(steps, { stopOnError: true });
  if (!report.success) throw new Error("Predict scoped approval failed");
  return report;
}

function ledgerPaths(env = process.env) {
  const ledger = env.PLOY_PREDICT_OPS_LEDGER || "/opt/ploy/data/account-ops/predict-fun-ledger.jsonl";
  return { ledger, lock: env.PLOY_PREDICT_OPS_LOCK || `${ledger}.lock` };
}

function appendLedger(ledger, event) {
  fs.mkdirSync(path.dirname(ledger), { recursive: true, mode: 0o700 });
  const fd = fs.openSync(ledger, "a", 0o600);
  try {
    fs.writeSync(fd, `${JSON.stringify({ at: new Date().toISOString(), ...event })}\n`);
    fs.fsyncSync(fd);
  } finally {
    fs.closeSync(fd);
  }
}

function readLedger(ledger) {
  if (!fs.existsSync(ledger)) return [];
  return fs.readFileSync(ledger, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
}

function operationId(plan) {
  return sha256({ kind: plan.kind, account: plan.account, chainId: plan.chainId, planSha256: plan.sha256 });
}

function withWriteLock(plan, env, dependencies, work) {
  const { ledger, lock } = ledgerPaths(env);
  fs.mkdirSync(path.dirname(lock), { recursive: true, mode: 0o700 });
  fs.mkdirSync(lock, { mode: 0o700 });
  const record = dependencies.record || ((event) => appendLedger(ledger, event));
  let completed = false;
  return Promise.resolve(work(record, operationId(plan)))
    .then((result) => {
      completed = true;
      return result;
    })
    .finally(() => {
      if (completed && fs.existsSync(lock)) fs.rmdirSync(lock);
    });
}

function assertMarketUnchanged(plan, market) {
  const current = {
    id: Number(market.id),
    conditionId: String(market.conditionId).toLowerCase(),
    decimalPrecision: Number(market.decimalPrecision),
    feeRateBps: Number(market.feeRateBps),
    isNegRisk: Boolean(market.isNegRisk),
    isYieldBearing: Boolean(market.isYieldBearing),
  };
  if (canonical(current) !== canonical(plan.market)) throw new Error("market execution metadata changed after planning");
  if (!(market.outcomes || []).some((outcome) => String(outcome.onChainId) === plan.order.tokenId)) {
    throw new Error("planned token is no longer a market outcome");
  }
}

async function executeOrderPlan(plan, expectedHash, env = process.env, dependencies = {}) {
  if (env.PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED !== "true") throw new Error("Predict account-op writes are disabled");
  const context = runtimeContext(env);
  validatePlan(plan, context, expectedHash);
  const market = dependencies.market || await fetchMarket(plan.market.id, context, env);
  assertMarketUnchanged(plan, market);
  const session = dependencies.session || await makeWalletSession(context, env);
  const approvals = await checkApprovals(plan, session);
  if (approvals.checks.some((check) => !check.satisfied)) throw new Error("required scoped approvals are missing");
  const side = plan.order.side === "BUY" ? Side.BUY : Side.SELL;
  const amounts = session.builder.getLimitOrderAmounts({
    side,
    pricePerShareWei: parseUnits(plan.order.limitPrice, 18),
    quantityWei: parseUnits(plan.order.quantity, 18),
  });
  const order = session.builder.buildOrder("LIMIT", {
    side,
    tokenId: plan.order.tokenId,
    makerAmount: amounts.makerAmount,
    takerAmount: amounts.takerAmount,
    nonce: 0n,
    feeRateBps: plan.order.feeRateBps,
    expiresAt: new Date(plan.expiresAt),
  });
  const typedData = session.builder.buildTypedData(order, {
    isNegRisk: plan.market.isNegRisk,
    isYieldBearing: plan.market.isYieldBearing,
  });
  const signedOrder = await session.builder.signTypedDataOrder(typedData);
  const hash = session.builder.buildTypedDataHash(typedData);
  const jwt = dependencies.jwt || await authenticate(session, context, env);
  const submit = dependencies.submit || (async (body) => apiRequest(context, env, "/v1/orders", {
    method: "POST",
    jwt,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify(body),
  }));
  return withWriteLock(plan, env, dependencies, async (record, id) => {
    record({ operationId: id, state: "submitting", kind: plan.kind, orderHash: hash });
    try {
      const response = await submit({ data: {
        order: { ...signedOrder, hash },
        pricePerShare: amounts.pricePerShare.toString(),
        strategy: "LIMIT",
      } });
      record({ operationId: id, state: "submitted", kind: plan.kind, orderHash: hash, orderId: response.data.orderId });
      return response.data;
    } catch (error) {
      record({ operationId: id, state: "submission_unknown", kind: plan.kind, orderHash: hash, error: error.message });
      throw new Error(`Predict order submission outcome is unknown for ${id}; reconcile before retry`);
    }
  });
}

function positionAmountWei(amount) {
  const text = positiveDecimal(amount, "position.amount");
  return text.includes(".") ? parseUnits(text, 18) : BigInt(text);
}

async function executeRedeemPlan(plan, expectedHash, env = process.env, dependencies = {}) {
  if (env.PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED !== "true") throw new Error("Predict account-op writes are disabled");
  const context = runtimeContext(env);
  validatePlan(plan, context, expectedHash);
  const positions = dependencies.positions || await fetchPositions(context, env);
  const fresh = buildRedeemPlan(positions, context, Date.parse(plan.generatedAt));
  if (canonical(fresh.items) !== canonical(plan.items)) throw new Error("redeemable positions changed after planning");
  const session = dependencies.session || await makeWalletSession(context, env);
  const approvals = await checkApprovals(plan, session);
  if (approvals.checks.some((check) => !check.satisfied)) throw new Error("required scoped approvals are missing");
  return withWriteLock(plan, env, dependencies, async (record, id) => {
    const receipts = [];
    for (const item of plan.items) {
      record({ operationId: id, state: "submitting", kind: plan.kind, conditionId: item.conditionId, indexSet: item.indexSet });
      const options = {
        conditionId: item.conditionId,
        indexSet: item.indexSet,
        isNegRisk: item.isNegRisk,
        isYieldBearing: item.isYieldBearing,
      };
      if (item.isNegRisk) options.amount = positionAmountWei(item.amount);
      const result = await session.builder.redeemPositions(options);
      if (!result.success || !result.receipt || result.receipt.status !== 1) {
        record({ operationId: id, state: "ambiguous", kind: plan.kind, conditionId: item.conditionId });
        throw new Error(`Predict redemption is failed or ambiguous for ${item.conditionId}; reconcile before retry`);
      }
      const receipt = { conditionId: item.conditionId, transactionHash: result.receipt.hash };
      receipts.push(receipt);
      record({ operationId: id, state: "confirmed", kind: plan.kind, ...receipt });
    }
    return receipts;
  });
}

async function reconcileOrder(operationIdValue, env = process.env, dependencies = {}) {
  if (!operationIdValue) throw new Error("operation id is required");
  const context = runtimeContext(env);
  const { ledger, lock } = ledgerPaths(env);
  const history = dependencies.history || readLedger(ledger);
  const latest = history.filter((event) => event.operationId === operationIdValue).at(-1);
  if (!latest || latest.kind !== "predict_fun_limit_order" || latest.state !== "submission_unknown") {
    throw new Error("operation is not an unknown Predict order submission");
  }
  const session = dependencies.session || await makeWalletSession(context, env);
  const jwt = dependencies.jwt || await authenticate(session, context, env);
  const lookup = dependencies.lookup || (() => apiRequest(context, env, `/v1/orders/${latest.orderHash}`, { jwt }));
  const payload = await lookup();
  appendLedger(ledger, {
    operationId: operationIdValue,
    state: "submitted",
    kind: latest.kind,
    orderHash: latest.orderHash,
    orderId: payload.data.id,
    reconciled: true,
  });
  if (fs.existsSync(lock)) fs.rmdirSync(lock);
  return payload.data;
}

module.exports = {
  OFFICIAL_NETWORKS,
  buildOrderPlan,
  buildRedeemPlan,
  checkApprovals,
  executeApprovals,
  executeOrderPlan,
  executeRedeemPlan,
  fetchMarket,
  fetchPositions,
  loadWalletSecret,
  makeWalletSession,
  normalizeContext,
  runtimeContext,
  reconcileOrder,
  sha256,
  validatePlan,
};
