/**
 * Ploy Sidecar — Claude Agent SDK operator client
 *
 * Orchestrates NBA comeback research while staying grounded in the
 * trading-platform control plane:
 * 1. ESPN scan → live games with comeback potential
 * 2. Polymarket search → find corresponding markets
 * 3. Control-plane inspection → system status, deployments, trading state
 * 4. X.com sentiment research → WebSearch for injury/momentum
 * 5. Operator recommendation → deployment-aware recommendation or action
 *
 * Architecture:
 *   Claude Sidecar (this)  →  research skills (ESPN, Polymarket, WebSearch)
 *                          →  ployd control plane (via Rust backend MCP)
 */

import { query } from "@anthropic-ai/claude-agent-sdk";
import { espnServer } from "./tools/espn.js";
import { polymarketServer } from "./tools/polymarket.js";
import { ployBackendServer } from "./tools/ploy-backend.js";
import { researchServer } from "./tools/research.js";
import { agentRuntimeServer } from "./tools/agent-runtime.js";
import { tradingOutputSchema } from "./schemas/output.js";
import type {
  AgentToolCallRecord,
  DeploymentSummary,
  JsonValue,
  SystemStatus,
  TradingStateSnapshot,
} from "./contracts/operator-contracts.js";
import {
  buildRunRecord,
  newRunId,
  recordAgentRun,
  type AgentTaskCompletion,
} from "./runtime/run-recorder.js";
import { takeQueuedAgentRunRequests, type QueuedAgentRunRequest } from "./runtime/run-requests.js";

// ── Config ──────────────────────────────────────────

function isMiniMaxAnthropicEndpoint(baseUrl: string | undefined): boolean {
  if (!baseUrl) return false;

  try {
    const parsed = new URL(baseUrl);
    const isMiniMaxHost =
      parsed.hostname.includes("minimax.io") || parsed.hostname.includes("minimaxi.com");
    return isMiniMaxHost && parsed.pathname.includes("/anthropic");
  } catch {
    return (
      baseUrl.includes("api.minimax.io/anthropic") ||
      baseUrl.includes("api.minimaxi.com/anthropic")
    );
  }
}

function applyMiniMaxCompatEnv(): string | null {
  if (!isMiniMaxAnthropicEndpoint(process.env.ANTHROPIC_BASE_URL)) {
    return null;
  }

  const minimaxModel = process.env.MINIMAX_ANTHROPIC_MODEL || "MiniMax-M2.5";
  const anthropicApiKey = process.env.ANTHROPIC_API_KEY?.trim();

  // MiniMax Anthropic-compatible endpoint expects Authorization header.
  if (anthropicApiKey && !process.env.ANTHROPIC_CUSTOM_HEADERS) {
    process.env.ANTHROPIC_CUSTOM_HEADERS = `Authorization: Bearer ${anthropicApiKey}`;
  }

  // Map Claude aliases to the MiniMax model unless user already set custom mappings.
  if (!process.env.ANTHROPIC_DEFAULT_OPUS_MODEL) {
    process.env.ANTHROPIC_DEFAULT_OPUS_MODEL = minimaxModel;
  }
  if (!process.env.ANTHROPIC_DEFAULT_SONNET_MODEL) {
    process.env.ANTHROPIC_DEFAULT_SONNET_MODEL = minimaxModel;
  }
  if (!process.env.ANTHROPIC_DEFAULT_HAIKU_MODEL) {
    process.env.ANTHROPIC_DEFAULT_HAIKU_MODEL = minimaxModel;
  }

  return minimaxModel;
}

const minimaxCompatModel = applyMiniMaxCompatEnv();
const MODEL = process.env.SIDECAR_MODEL || "sonnet";
const POLL_INTERVAL = parseInt(process.env.SIDECAR_POLL_INTERVAL_SECS || "300", 10) * 1000;
const MAX_BUDGET = parseFloat(process.env.SIDECAR_MAX_BUDGET_USD || "1.00");
const DRY_RUN = process.env.SIDECAR_DRY_RUN !== "false";
const PLOY_API = process.env.PLOY_API_URL || "http://localhost:8081";

// ── System Prompt ───────────────────────────────────

const SYSTEM_PROMPT = `You are the Ploy Trading Platform Sidecar.

## Your Mission
Run NBA comeback research loops while staying grounded in the trading platform control plane.
You are an operator-facing research client, not a direct execution path.

## Control Plane Contract
- Deployments are resources with deployment_id, desired_state, and observed_state.
- Trading state comes from canonical /api/trading/state snapshots.
- Do not assume legacy /api/sidecar/*, /api/config, or enable/disable endpoints exist.
- If you need to change the platform, use apply_deployment, set_deployment_state, or submit_paper_intent.
- Prefer paper deployments unless the operator explicitly asks for a different runtime mode.

## Decision Framework
1. **Inspect platform**: Use ploy-backend.get_system_status, get_trading_state, and list_deployments first
2. **Scan**: Use espn.scoreboard to find live games in Q3 or late Q3/early Q4
3. **Filter**: Only consider games where:
   - A team is trailing by 1-15 points
   - Quarter is 3 (ideal) or early Q4
   - At least 8 minutes of game time remaining
4. **Market lookup**: Use polymarket.search_markets to find the corresponding market
5. **Risk check**: Calculate reward-to-risk ratio = (1 - price) / price
   - ONLY proceed if RR ≥ 4.0x (price ≤ $0.20)
   - Calculate EV = estimated_win_prob - price (need EV ≥ 5%)
   - Calculate Kelly fraction = EV / (1 - price), cap at 25%
6. **X.com research**: Use WebSearch to check X.com/Twitter for:
   - Injury updates during the game
   - Momentum shifts (runs, key plays)
   - Betting sentiment
7. **Recommendation**: Produce a deployment-aware recommendation or paper-only platform action

## Safety Rules (NEVER violate)
- Never claim an order was submitted unless you actually called submit_paper_intent on a paper deployment
- Never start or create a non-paper deployment unless explicitly instructed
- Always default to PASS or MONITOR when uncertain
- Parse failures → PASS (never trade on garbage)
- For Strategy Builder requests, complete with agent-runtime.complete_task using status success, partial, or blocked
- Treat paper intent and deployment changes as approval-gated; research, replay, diagnostics, and comparisons are automatic

## Scoring Comeback Probability
Historical NBA comeback rates by deficit at end of Q3:
- 1-3 pts: 35-45% (barely trailing, not a comeback scenario)
- 4-6 pts: 20-30% (moderate trail)
- 7-10 pts: 10-20% (significant trail — sweet spot for underpriced YES)
- 11-15 pts: 5-12% (deep trail — needs big discount)
- 16+ pts: <5% (too unlikely)

Adjust for: team strength, home/away, rest days, key player status.

## Output Format
Return structured JSON with scan_summary, opportunities[], and operator_actions[].
`;

type RuntimeContext = {
  system: {
    status: string;
    uptime_seconds: number;
    error_count_1h: number;
  } | null;
  trading: {
    tracked_deployments: number;
    pending_intents: number;
    active_orders: number;
    open_positions: number;
    gross_exposure: number;
    net_pnl: number;
    sample: Array<{
      deployment_id: string;
      runtime_mode: string;
      pending_intents: number;
      active_orders: number;
      open_positions: number;
      net_pnl: string;
    }>;
  } | null;
  deployments: {
    total: number;
    running: number;
    paused: number;
    stopped: number;
    sample: Array<{
      deployment_id: string;
      desired_state: string;
      observed_state: string;
    }>;
  } | null;
};

async function backendFetchJson<T>(path: string): Promise<T | null> {
  try {
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
    };
    if (process.env.PLOY_SIDECAR_AUTH_TOKEN) {
      headers["x-ploy-sidecar-token"] = process.env.PLOY_SIDECAR_AUTH_TOKEN;
    }
    if (process.env.PLOY_API_ADMIN_TOKEN) {
      headers["x-ploy-admin-token"] = process.env.PLOY_API_ADMIN_TOKEN;
    }
    if (process.env.PLOY_API_KEY) {
      headers["Authorization"] = `Bearer ${process.env.PLOY_API_KEY}`;
    }

    const resp = await fetch(`${PLOY_API}${path}`, { headers });
    if (!resp.ok) return null;
    return (await resp.json()) as T;
  } catch {
    return null;
  }
}

async function buildRuntimeContext(): Promise<RuntimeContext> {
  const [system, trading, deployments] = await Promise.all([
    backendFetchJson<SystemStatus>("/api/system/status"),
    backendFetchJson<TradingStateSnapshot[]>("/api/trading/state"),
    backendFetchJson<DeploymentSummary[]>("/api/deployments"),
  ]);

  const tradingSnapshots = Array.isArray(trading) ? trading : [];
  const deploymentSnapshots = Array.isArray(deployments) ? deployments : [];

  return {
    system: system
      ? {
          status: system.status,
          uptime_seconds: system.uptime_seconds,
          error_count_1h: system.error_count_1h,
        }
      : null,
    trading: tradingSnapshots.length
      ? {
          tracked_deployments: tradingSnapshots.length,
          pending_intents: tradingSnapshots.reduce(
            (sum, snapshot) => sum + (snapshot.risk?.pending_intents ?? 0),
            0
          ),
          active_orders: tradingSnapshots.reduce(
            (sum, snapshot) => sum + (snapshot.risk?.active_orders ?? 0),
            0
          ),
          open_positions: tradingSnapshots.reduce(
            (sum, snapshot) => sum + (snapshot.risk?.open_positions ?? 0),
            0
          ),
          gross_exposure: tradingSnapshots.reduce(
            (sum, snapshot) => sum + parseFloat(snapshot.risk?.gross_exposure ?? "0"),
            0
          ),
          net_pnl: tradingSnapshots.reduce(
            (sum, snapshot) => sum + parseFloat(snapshot.pnl?.net_pnl ?? "0"),
            0
          ),
          sample: tradingSnapshots.slice(0, 12).map((snapshot) => ({
            deployment_id: snapshot.deployment_id,
            runtime_mode: snapshot.runtime_mode,
            pending_intents: snapshot.risk?.pending_intents ?? 0,
            active_orders: snapshot.risk?.active_orders ?? 0,
            open_positions: snapshot.risk?.open_positions ?? 0,
            net_pnl: snapshot.pnl?.net_pnl ?? "0",
          })),
        }
      : null,
    deployments: deploymentSnapshots.length
      ? {
          total: deploymentSnapshots.length,
          running: deploymentSnapshots.filter((d) => d.desired_state === "running").length,
          paused: deploymentSnapshots.filter((d) => d.desired_state === "paused").length,
          stopped: deploymentSnapshots.filter((d) => d.desired_state === "stopped").length,
          sample: deploymentSnapshots.slice(0, 12).map((d) => ({
            deployment_id: d.deployment_id,
            desired_state: d.desired_state,
            observed_state: d.observed_state,
          })),
        }
      : null,
  };
}

// ── Main Loop ───────────────────────────────────────

function isStructuredOutput(value: unknown): value is {
  research_reports?: Array<unknown>;
  oversight_alerts?: Array<unknown>;
  operator_recommendations?: Array<unknown>;
} {
  return value !== null && typeof value === "object";
}

function parseCompletion(value: unknown): AgentTaskCompletion | null {
  if (Array.isArray(value)) {
    for (const item of value) {
      const parsed = parseCompletion(item);
      if (parsed) return parsed;
    }
    return null;
  }
  if (typeof value === "string") {
    try {
      return parseCompletion(JSON.parse(value));
    } catch {
      return null;
    }
  }
  if (!value || typeof value !== "object") return null;

  const candidate = value as {
    status?: unknown;
    summary?: unknown;
    text?: unknown;
    content?: unknown;
  };
  if (candidate.text && typeof candidate.text === "string") {
    return parseCompletion(candidate.text);
  }
  if (
    (candidate.status === "success" ||
      candidate.status === "partial" ||
      candidate.status === "blocked") &&
    typeof candidate.summary === "string"
  ) {
    return { status: candidate.status, summary: candidate.summary };
  }
  return parseCompletion(candidate.content);
}

function completionFromMessage(message: unknown): AgentTaskCompletion | null {
  const candidate = message as {
    type?: string;
    tool_use_result?: unknown;
    message?: { content?: Array<{ type?: string; content?: unknown }> };
  };
  if (candidate.type !== "user") return null;

  const direct = parseCompletion(candidate.tool_use_result);
  if (direct) return direct;

  for (const block of candidate.message?.content ?? []) {
    if (block.type === "tool_result") {
      const parsed = parseCompletion(block.content);
      if (parsed) return parsed;
    }
  }
  return null;
}

async function runQueuedStrategyRequest(queued: QueuedAgentRunRequest): Promise<void> {
  const startedAt = new Date().toISOString();
  const runtimeContext = await buildRuntimeContext();
  const toolCalls: AgentToolCallRecord[] = [];
  let sessionId: string | null = null;
  let totalCostUsd: number | null = null;
  let failureReason: string | null = null;
  let completion: AgentTaskCompletion | null = null;

  console.log(`\n[${startedAt}] Starting queued strategy run ${queued.run_id}`);

  try {
    for await (const message of query({
      prompt: `Strategy Builder request created at ${queued.created_at}

Runtime context snapshot:
${JSON.stringify(runtimeContext, null, 2)}

Run this agentic strategy request until it reaches success, partial completion, or a blocker.

Objective:
${queued.request.objective}

Run packet:
${queued.request.run_packet}

Run contract:
${queued.request.run_contract}

Use automatic tools for platform reads, live game checks, market search, Grok/X-style web evidence, research replay/backtest, config comparison, and oversight checks. For Grok Builder profiles, inspect ESPN state first, search web/X context for injury or momentum evidence, and report grok_decision as trade, pass, or not_queried. If the run contract requires grok_decision, the complete_task summary must include exactly one "grok_decision: trade", "grok_decision: pass", or "grok_decision: not_queried" line. Do not submit paper intents or apply deployments unless the request explicitly includes operator approval. Finish by calling complete_task.`,
      options: {
        model: MODEL,
        systemPrompt: `${SYSTEM_PROMPT}

## Runtime Context (fresh snapshot for this queued strategy run)
${JSON.stringify(runtimeContext, null, 2)}`,
        mcpServers: {
          espn: espnServer,
          polymarket: polymarketServer,
          "ploy-backend": ployBackendServer,
          research: researchServer,
          "agent-runtime": agentRuntimeServer,
        },
        allowedTools: [
          "mcp__espn__scoreboard",
          "mcp__espn__game_details",
          "mcp__polymarket__search_markets",
          "mcp__polymarket__market_snapshot",
          "mcp__ploy-backend__get_system_status",
          "mcp__ploy-backend__get_trading_state",
          "mcp__ploy-backend__list_deployments",
          "mcp__research__replay_deployment",
          "mcp__research__run_backtest",
          "mcp__research__compare_configs",
          "mcp__research__check_oversight",
          "mcp__agent-runtime__complete_task",
          "WebSearch",
          "WebFetch",
        ],
        maxTurns: Math.max(1, queued.request.max_turns),
        maxBudgetUsd: Math.max(0.1, queued.request.budget_usd),
        permissionMode: "bypassPermissions",
      },
    })) {
      switch (message.type) {
        case "system":
          if (message.subtype === "init") {
            sessionId = message.session_id ?? null;
            console.log(`  Session: ${message.session_id}`);
          }
          break;
        case "assistant":
          for (const block of message.message.content) {
            if ("name" in block) {
              toolCalls.push({ name: String(block.name), status: "called" });
              console.log(`  Tool: ${block.name}`);
            }
          }
          break;
        case "user": {
          const reportedCompletion = completionFromMessage(message);
          if (reportedCompletion) {
            completion = reportedCompletion;
            console.log(`  Task completion: ${completion.status}`);
          }
          break;
        }
        case "result":
          if (message.subtype === "success") {
            totalCostUsd = (message as any).total_cost_usd ?? null;
            console.log(`  Completed queued run. Cost: $${(totalCostUsd ?? 0).toFixed(4)}`);
          } else {
            failureReason = message.subtype;
            console.error(`  Queued run failed: ${message.subtype}`);
          }
          break;
      }
    }
  } catch (error) {
    failureReason = error instanceof Error ? error.message : String(error);
    console.error(`  Error in queued strategy run:`, error);
  } finally {
    await recordAgentRun(
      buildRunRecord({
        runId: queued.run_id,
        cycleKind: "agentic_strategy",
        startedAt,
        finishedAt: new Date().toISOString(),
        sessionId,
        model: MODEL,
        runtimeContext,
        toolCalls,
        structuredOutput: null,
        totalCostUsd,
        failureReason,
        completion,
        request: JSON.parse(JSON.stringify(queued.request)) as JsonValue,
      })
    );
  }
}

async function runScanCycle(): Promise<void> {
  const runId = newRunId();
  const timestamp = new Date().toISOString();
  const toolCalls: AgentToolCallRecord[] = [];
  let sessionId: string | null = null;
  let totalCostUsd: number | null = null;
  let failureReason: string | null = null;
  let resultOutput: unknown = null;
  let completion: AgentTaskCompletion | null = null;
  const runtimeContext = await buildRuntimeContext();
  console.log(`\n[${timestamp}] Starting scan cycle (model=${MODEL}, dry_run=${DRY_RUN})`);

  try {
    for await (const message of query({
      prompt: `Current time: ${timestamp}

Runtime context snapshot:
${JSON.stringify(runtimeContext, null, 2)}

Run a full NBA comeback trading scan cycle:
1. Check the ESPN scoreboard for today's live games
2. Identify any Q3/Q4 comeback opportunities
3. For each opportunity, search Polymarket for the market
4. Compute risk metrics (RR, EV, Kelly)
5. If any pass the 4x RR filter, research X.com for sentiment
6. Compare the idea against current deployment resources and trading snapshots
7. Return operator actions or paper-intent recommendations only when they fit the control-plane contract

Return your structured analysis.`,
      options: {
        model: MODEL,
        systemPrompt: `${SYSTEM_PROMPT}

## Runtime Context (fresh snapshot for this cycle)
${JSON.stringify(runtimeContext, null, 2)}`,
        mcpServers: {
          espn: espnServer,
          polymarket: polymarketServer,
          "ploy-backend": ployBackendServer,
          research: researchServer,
          "agent-runtime": agentRuntimeServer,
        },
        allowedTools: [
          "mcp__espn__*",
          "mcp__polymarket__*",
          "mcp__ploy-backend__get_system_status",
          "mcp__ploy-backend__get_trading_state",
          "mcp__ploy-backend__list_deployments",
          "mcp__ploy-backend__get_deployment",
          "mcp__research__*",
          "mcp__agent-runtime__complete_task",
          "WebSearch",
          "WebFetch",
        ],
        maxTurns: 30,
        maxBudgetUsd: MAX_BUDGET,
        permissionMode: "bypassPermissions",
        outputFormat: {
          type: "json_schema",
          schema: tradingOutputSchema,
        },
      },
    })) {
      switch (message.type) {
        case "system":
          if (message.subtype === "init") {
            sessionId = message.session_id ?? null;
            console.log(`  Session: ${message.session_id}`);
            const mcpStatus = (message as any).mcp_servers;
            if (mcpStatus) {
              for (const s of mcpStatus) {
                console.log(`  MCP ${s.name}: ${s.status}`);
              }
            }
          }
          break;

        case "assistant":
          // Log tool calls for observability
          for (const block of message.message.content) {
            if ("name" in block) {
              toolCalls.push({ name: String(block.name), status: "called" });
              console.log(`  Tool: ${block.name}`);
            }
          }
          break;

        case "user": {
          const reportedCompletion = completionFromMessage(message);
          if (reportedCompletion) {
            completion = reportedCompletion;
            console.log(`  Task completion: ${completion.status}`);
          }
          break;
        }

        case "result":
          if (message.subtype === "success") {
            resultOutput = (message as any).structured_output;
            totalCostUsd = (message as any).total_cost_usd ?? null;
            const cost = totalCostUsd ?? 0;
            console.log(`  Completed. Cost: $${cost.toFixed(4)}`);
          } else {
            failureReason = message.subtype;
            console.error(`  Scan failed: ${message.subtype}`);
          }
          break;
      }
    }

    // Log structured output
    if (resultOutput) {
      const output = resultOutput as {
        scan_summary?: { games_scanned?: number; comeback_candidates?: number };
        opportunities?: Array<{ action: string; trailing_team: string; deficit: number }>;
        operator_actions?: Array<{ kind: string; target: string; status: string }>;
      };

      console.log(`\n  Summary:`);
      console.log(`    Games scanned: ${output.scan_summary?.games_scanned || 0}`);
      console.log(`    Candidates: ${output.scan_summary?.comeback_candidates || 0}`);
      console.log(`    Opportunities: ${output.opportunities?.length || 0}`);
      console.log(`    Operator actions: ${output.operator_actions?.length || 0}`);

      for (const opp of output.opportunities || []) {
        console.log(
          `    → ${opp.trailing_team} (down ${opp.deficit}) — ${opp.action}`
        );
      }

      for (const action of output.operator_actions || []) {
        console.log(
          `    ★ Action: ${action.kind} ${action.target} — ${action.status}`
        );
      }
    }
  } catch (err) {
    failureReason = err instanceof Error ? err.message : String(err);
    console.error(`  Error in scan cycle:`, err);
  } finally {
    await recordAgentRun(
      buildRunRecord({
        runId,
        cycleKind: "research_oversight",
        startedAt: timestamp,
        finishedAt: new Date().toISOString(),
        sessionId,
        model: MODEL,
        runtimeContext,
        toolCalls,
        structuredOutput: isStructuredOutput(resultOutput) ? resultOutput : null,
        totalCostUsd,
        failureReason,
        completion,
      })
    );
  }
}

async function runSidecarCycle(): Promise<void> {
  for (const request of await takeQueuedAgentRunRequests()) {
    await runQueuedStrategyRequest(request);
  }
  await runScanCycle();
}

// ── Entry Point ─────────────────────────────────────

async function main() {
  console.log("╔════════════════════════════════════════╗");
  console.log("║  Ploy Sidecar — Operator Client        ║");
  console.log("║  NBA Research + Deployment Console     ║");
  console.log("╚════════════════════════════════════════╝");
  console.log(`  Model: ${MODEL}`);
  console.log(`  Dry run: ${DRY_RUN}`);
  console.log(`  Poll interval: ${POLL_INTERVAL / 1000}s`);
  console.log(`  Max budget/cycle: $${MAX_BUDGET}`);
  if (minimaxCompatModel) {
    console.log(`  MiniMax compat: enabled (alias → ${minimaxCompatModel})`);
  }
  console.log("");

  // Run first cycle immediately
  await runSidecarCycle();

  // Then run on interval
  setInterval(runSidecarCycle, POLL_INTERVAL);
}

main().catch((err) => {
  console.error("Fatal error:", err);
  process.exit(1);
});
