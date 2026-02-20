/**
 * Ploy Sidecar — Claude Agent SDK Commander
 *
 * Orchestrates NBA comeback trading research:
 * 1. ESPN scan → live games with comeback potential
 * 2. Polymarket search → find corresponding markets
 * 3. Risk metrics computation → RR ≥ 4x, EV ≥ 5%, Kelly
 * 4. X.com sentiment research → WebSearch for injury/momentum
 * 5. Grok Final Judge → Trade or Pass
 * 6. Order submission → through Rust Coordinator
 *
 * Architecture:
 *   Claude Commander (this)  →  research skills (ESPN, Polymarket, WebSearch)
 *                            →  Grok Final Judge (via Rust backend)
 *                            →  Order Executor (via Rust backend)
 */

import { query } from "@anthropic-ai/claude-agent-sdk";
import { espnServer } from "./tools/espn.js";
import { polymarketServer } from "./tools/polymarket.js";
import { ployBackendServer } from "./tools/ploy-backend.js";
import { tradingOutputSchema } from "./schemas/output.js";

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

const SYSTEM_PROMPT = `You are the Ploy NBA Comeback Trading Commander.

## Your Mission
Scan live NBA games for comeback trading opportunities on Polymarket.
Buy YES shares on trailing teams when the market underprices their comeback probability.

## Decision Framework
1. **Scan**: Use espn.scoreboard to find live games in Q3 or late Q3/early Q4
2. **Filter**: Only consider games where:
   - A team is trailing by 1-15 points
   - Quarter is 3 (ideal) or early Q4
   - At least 8 minutes of game time remaining
3. **Market lookup**: Use polymarket.search_markets to find the corresponding market
4. **Risk check**: Calculate reward-to-risk ratio = (1 - price) / price
   - ONLY proceed if RR ≥ 4.0x (price ≤ $0.20)
   - Calculate EV = estimated_win_prob - price (need EV ≥ 5%)
   - Calculate Kelly fraction = EV / (1 - price), cap at 25%
5. **X.com research**: Use WebSearch to check X.com/Twitter for:
   - Injury updates during the game
   - Momentum shifts (runs, key plays)
   - Betting sentiment
6. **Grok decision**: Submit research brief to ploy-backend.request_grok_decision
   - Grok is the FINAL JUDGE. Only trade if Grok says "trade"
   - If Grok is unavailable and you have strong statistical backing, note it but do NOT trade
7. **Order**: If Grok approves, use ploy-backend.submit_order with dry_run=${DRY_RUN}

## Risk Rules (NEVER violate)
- Maximum price: $0.20 (reward-to-risk ≥ 4x)
- Maximum order: $50
- Maximum 3 positions per session
- Always default to PASS when uncertain
- Parse failures → PASS (never trade on garbage)

## Scoring Comeback Probability
Historical NBA comeback rates by deficit at end of Q3:
- 1-3 pts: 35-45% (barely trailing, not a comeback scenario)
- 4-6 pts: 20-30% (moderate trail)
- 7-10 pts: 10-20% (significant trail — sweet spot for underpriced YES)
- 11-15 pts: 5-12% (deep trail — needs big discount)
- 16+ pts: <5% (too unlikely)

Adjust for: team strength, home/away, rest days, key player status.

## Output Format
Return structured JSON with scan_summary, opportunities[], and orders_submitted[].
`;

type RuntimeContext = {
  system: {
    status: string;
    uptime_seconds: number;
    error_count_1h: number;
  } | null;
  risk: {
    state: string;
    queue_depth: number;
    daily_pnl_usd: number;
    position_count: number;
    circuit_breaker_events: number;
  } | null;
  open_positions: Array<{
    market_slug: string;
    side: string;
    shares: number;
    status: string;
  }>;
  deployments: {
    total: number;
    enabled: number;
    sample: Array<{
      id: string;
      domain: string;
      strategy: string;
      enabled: boolean;
      timeframe: string;
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
  const [system, risk, positions, deployments] = await Promise.all([
    backendFetchJson<{
      status: string;
      uptime_seconds: number;
      error_count_1h: number;
    }>("/api/system/status"),
    backendFetchJson<{
      risk_state: string;
      queue_depth: number;
      daily_pnl_usd: number;
      positions: unknown[];
      circuit_breaker_events: unknown[];
    }>("/api/sidecar/risk"),
    backendFetchJson<
      Array<{ market_slug: string; side: string; shares: number; status: string }>
    >("/api/sidecar/positions"),
    backendFetchJson<
      Array<{
        id: string;
        domain: string;
        strategy: string;
        enabled: boolean;
        timeframe: string;
      }>
    >("/api/deployments"),
  ]);

  return {
    system: system
      ? {
          status: system.status,
          uptime_seconds: system.uptime_seconds,
          error_count_1h: system.error_count_1h,
        }
      : null,
    risk: risk
      ? {
          state: risk.risk_state,
          queue_depth: risk.queue_depth,
          daily_pnl_usd: risk.daily_pnl_usd,
          position_count: Array.isArray(risk.positions) ? risk.positions.length : 0,
          circuit_breaker_events: Array.isArray(risk.circuit_breaker_events)
            ? risk.circuit_breaker_events.length
            : 0,
        }
      : null,
    open_positions: (positions ?? []).slice(0, 8).map((p) => ({
      market_slug: p.market_slug,
      side: p.side,
      shares: p.shares,
      status: p.status,
    })),
    deployments: deployments
      ? {
          total: deployments.length,
          enabled: deployments.filter((d) => d.enabled).length,
          sample: deployments.slice(0, 12).map((d) => ({
            id: d.id,
            domain: d.domain,
            strategy: d.strategy,
            enabled: d.enabled,
            timeframe: d.timeframe,
          })),
        }
      : null,
  };
}

// ── Main Loop ───────────────────────────────────────

async function runScanCycle(): Promise<void> {
  const timestamp = new Date().toISOString();
  console.log(`\n[${timestamp}] Starting scan cycle (model=${MODEL}, dry_run=${DRY_RUN})`);

  try {
    const runtimeContext = await buildRuntimeContext();
    let resultOutput: unknown = null;

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
6. Submit to Grok for final decision if warranted
7. Execute orders if Grok approves

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
        },
        allowedTools: [
          "mcp__espn__*",
          "mcp__polymarket__*",
          "mcp__ploy-backend__*",
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
              console.log(`  Tool: ${block.name}`);
            }
          }
          break;

        case "result":
          if (message.subtype === "success") {
            resultOutput = (message as any).structured_output;
            const cost = (message as any).total_cost_usd || 0;
            console.log(`  Completed. Cost: $${cost.toFixed(4)}`);
          } else {
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
        orders_submitted?: Array<{ market_slug: string; status: string }>;
      };

      console.log(`\n  Summary:`);
      console.log(`    Games scanned: ${output.scan_summary?.games_scanned || 0}`);
      console.log(`    Candidates: ${output.scan_summary?.comeback_candidates || 0}`);
      console.log(`    Opportunities: ${output.opportunities?.length || 0}`);

      for (const opp of output.opportunities || []) {
        console.log(
          `    → ${opp.trailing_team} (down ${opp.deficit}) — ${opp.action}`
        );
      }

      for (const order of output.orders_submitted || []) {
        console.log(
          `    ★ Order: ${order.market_slug} — ${order.status}`
        );
      }
    }
  } catch (err) {
    console.error(`  Error in scan cycle:`, err);
  }
}

// ── Entry Point ─────────────────────────────────────

async function main() {
  console.log("╔════════════════════════════════════════╗");
  console.log("║  Ploy Sidecar — Claude Commander       ║");
  console.log("║  NBA Comeback Trading Agent             ║");
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
  await runScanCycle();

  // Then run on interval
  setInterval(runScanCycle, POLL_INTERVAL);
}

main().catch((err) => {
  console.error("Fatal error:", err);
  process.exit(1);
});
