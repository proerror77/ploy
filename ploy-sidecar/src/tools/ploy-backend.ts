/**
 * Ploy Backend MCP Tools — Calls to the Rust REST API.
 *
 * These tools interact with the running Ploy backend for:
 * - Grok unified decision requests
 * - Order submission through the Coordinator
 * - Position and risk state queries
 * - System status
 */

import { tool, createSdkMcpServer } from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";
import { randomUUID } from "crypto";

const PLOY_API = process.env.PLOY_API_URL || "http://localhost:8081";
const PLOY_ADMIN_TOKEN = process.env.PLOY_API_ADMIN_TOKEN || process.env.PLOY_ADMIN_TOKEN;

async function ployFetch(path: string, options?: RequestInit) {
  const url = `${PLOY_API}${path}`;
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
  };
  if (process.env.PLOY_SIDECAR_AUTH_TOKEN) {
    headers["x-ploy-sidecar-token"] = process.env.PLOY_SIDECAR_AUTH_TOKEN;
  }
  if (process.env.PLOY_API_KEY) {
    headers["Authorization"] = `Bearer ${process.env.PLOY_API_KEY}`;
  }
  if (PLOY_ADMIN_TOKEN) {
    headers["x-ploy-admin-token"] = PLOY_ADMIN_TOKEN;
  }
  return fetch(url, { ...options, headers: { ...headers, ...options?.headers } });
}

async function callBackend(path: string, options?: RequestInit): Promise<any> {
  const resp = await ployFetch(path, options);
  if (!resp.ok) {
    const err = await resp.text();
    throw new Error(`Backend error (${resp.status}): ${err}`);
  }
  return resp.json();
}

export const ployBackendServer = createSdkMcpServer({
  name: "ploy-backend",
  version: "1.0.0",
  tools: [
    tool(
      "request_grok_decision",
      `Submit a research brief to Grok (via Rust backend) for a final trade decision.
Grok synthesizes ALL data (game state, stats, X.com sentiment, market) and returns Trade or Pass.
This is the FINAL JUDGE — only trade if Grok approves.`,
      {
        game_id: z.string().describe("ESPN game ID"),
        home_team: z.string().describe("Home team name"),
        away_team: z.string().describe("Away team name"),
        home_abbrev: z.string().describe("Home team abbreviation (e.g., BOS)"),
        away_abbrev: z.string().describe("Away team abbreviation (e.g., LAL)"),
        home_score: z.number().describe("Home team score"),
        away_score: z.number().describe("Away team score"),
        quarter: z.number().describe("Current quarter (1-4, 5+ for OT)"),
        clock: z.string().describe("Game clock (e.g., '4:30')"),
        trailing_team: z.string().describe("Name of the trailing team"),
        trailing_abbrev: z.string().describe("Abbreviation of trailing team"),
        deficit: z.number().describe("Point deficit (positive)"),
        // Statistical model data (from Claude's research)
        comeback_rate: z.number().optional().describe("Historical comeback rate (0.0-1.0)"),
        adjusted_win_prob: z.number().optional().describe("Estimated win probability (0.0-1.0)"),
        statistical_edge: z.number().optional().describe("Edge vs market price (0.0-1.0)"),
        // Market data
        market_slug: z.string().describe("Polymarket market slug"),
        token_id: z.string().describe("YES token ID for trailing team"),
        market_price: z.number().describe("Current YES price (0.0-1.0)"),
        best_bid: z.number().optional().describe("Best bid price"),
        best_ask: z.number().optional().describe("Best ask price"),
        // X.com intelligence (from Claude's research)
        momentum_narrative: z.string().optional().describe("Momentum summary from X.com"),
        sentiment_home: z.number().optional().describe("Home sentiment score (-1.0 to 1.0)"),
        sentiment_away: z.number().optional().describe("Away sentiment score (-1.0 to 1.0)"),
        injury_updates: z.string().optional().describe("Injury updates from X.com"),
      },
      async (args) => {
        try {
          const resp = await ployFetch("/api/sidecar/grok/decision", {
            method: "POST",
            body: JSON.stringify(args),
          });

          if (!resp.ok) {
            const err = await resp.text();
            return {
              content: [{ type: "text" as const, text: `Grok decision error (${resp.status}): ${err}` }],
              isError: true,
            };
          }

          const decision = await resp.json();
          return {
            content: [{ type: "text" as const, text: JSON.stringify(decision, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: `Backend unreachable: ${e.message}` }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "submit_order",
      `Submit a trade intent to the Ploy Coordinator ingress. Goes through deployment/risk gate before execution.
IMPORTANT: Always use dry_run=true unless explicitly configured for live trading.`,
      {
        deployment_id: z.string().default("openclaw.default").describe("Strategy deployment ID"),
        domain: z.enum(["crypto", "sports", "politics", "economics"]).default("sports"),
        strategy: z
          .string()
          .default("sidecar_nba")
          .describe("Strategy label (metadata only)"),
        market_slug: z.string().describe("Polymarket market slug"),
        token_id: z.string().describe("Token ID to trade"),
        side: z.enum(["YES", "NO"]).describe("Which outcome to buy"),
        shares: z.number().min(1).describe("Number of shares to buy"),
        price: z.number().min(0.01).max(0.99).describe("Limit price"),
        idempotency_key: z.string().optional().describe("Optional idempotency key"),
        dry_run: z.boolean().default(true).describe("Simulate only (default true)"),
        // Decision metadata for audit trail
        grok_decision_id: z.string().optional().describe("Grok decision request_id if applicable"),
        edge: z.number().optional().describe("Estimated edge"),
        confidence: z.number().optional().describe("Confidence level 0.0-1.0"),
        reasoning: z.string().optional().describe("Trade reasoning"),
      },
      async (args) => {
        try {
          const payload = {
            deployment_id: args.deployment_id,
            domain: args.domain,
            market_slug: args.market_slug,
            token_id: args.token_id,
            side: args.side === "YES" ? "UP" : "DOWN",
            order_side: "BUY",
            size: args.shares,
            price_limit: args.price,
            idempotency_key: args.idempotency_key || `sidecar-${randomUUID()}`,
            reason: args.reasoning,
            confidence: args.confidence,
            edge: args.edge,
            metadata: {
              source: "ploy-sidecar.mcp",
              strategy: args.strategy,
              decision_request_id: args.grok_decision_id || "",
            },
            dry_run: args.dry_run,
          };

          const resp = await ployFetch("/api/sidecar/intents", {
            method: "POST",
            body: JSON.stringify(payload),
          });

          if (!resp.ok) {
            const err = await resp.text();
            return {
              content: [{ type: "text" as const, text: `Order submission error (${resp.status}): ${err}` }],
              isError: true,
            };
          }

          const result = await resp.json();
          return {
            content: [{ type: "text" as const, text: JSON.stringify(result, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: `Backend unreachable: ${e.message}` }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "get_positions",
      "Get current open positions from the Ploy backend.",
      {},
      async () => {
        try {
          const resp = await ployFetch("/api/sidecar/positions");
          if (!resp.ok) {
            return {
              content: [{ type: "text" as const, text: `Positions error: ${resp.status}` }],
              isError: true,
            };
          }
          const positions = await resp.json();
          return {
            content: [{ type: "text" as const, text: JSON.stringify(positions, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: `Backend unreachable: ${e.message}` }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "get_system_status",
      "Get Ploy system health, uptime, and agent statuses.",
      {},
      async () => {
        try {
          const resp = await ployFetch("/api/system/status");
          if (!resp.ok) {
            return {
              content: [{ type: "text" as const, text: `Status error: ${resp.status}` }],
              isError: true,
            };
          }
          const status = await resp.json();
          return {
            content: [{ type: "text" as const, text: JSON.stringify(status, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: `Backend unreachable: ${e.message}` }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "system_control",
      "Control coordinator runtime. action: start|stop|restart|pause|resume|halt.",
      {
        action: z.enum(["start", "stop", "restart", "pause", "resume", "halt"]),
        domain: z.string().optional().describe("Optional domain scope (currently ignored by backend)"),
      },
      async (args) => {
        try {
          const body = args.domain ? JSON.stringify({ domain: args.domain }) : undefined;
          const result = await callBackend(`/api/system/${args.action}`, {
            method: "POST",
            body,
          });
          return {
            content: [{ type: "text" as const, text: JSON.stringify(result, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: e.message }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "get_config",
      "Fetch current strategy configuration from backend.",
      {},
      async () => {
        try {
          const config = await callBackend("/api/config");
          return {
            content: [{ type: "text" as const, text: JSON.stringify(config, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: e.message }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "update_config",
      "Patch strategy configuration. Backend requires full config; this tool merges with current config first.",
      {
        symbols: z.array(z.string()).optional(),
        min_move: z.number().optional(),
        max_entry: z.number().optional(),
        shares: z.number().int().positive().optional(),
        predictive: z.boolean().optional(),
        exit_edge_floor: z.number().nullable().optional(),
        exit_price_band: z.number().nullable().optional(),
        time_decay_exit_secs: z.number().int().nonnegative().nullable().optional(),
        liquidity_exit_spread_bps: z.number().int().nonnegative().nullable().optional(),
      },
      async (args) => {
        try {
          const current = await callBackend("/api/config");
          const payload = { ...current, ...args };
          const result = await callBackend("/api/config", {
            method: "PUT",
            body: JSON.stringify(payload),
          });
          return {
            content: [{ type: "text" as const, text: JSON.stringify(result, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: e.message }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "list_deployments",
      "List strategy deployment matrix entries.",
      {},
      async () => {
        try {
          const items = await callBackend("/api/deployments");
          return {
            content: [{ type: "text" as const, text: JSON.stringify(items, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: e.message }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "get_deployment",
      "Get one deployment by id.",
      {
        id: z.string(),
      },
      async (args) => {
        try {
          const item = await callBackend(`/api/deployments/${encodeURIComponent(args.id)}`);
          return {
            content: [{ type: "text" as const, text: JSON.stringify(item, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: e.message }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "upsert_deployments",
      "Bulk upsert deployment matrix. Provide deployments_json as a JSON array.",
      {
        deployments_json: z.string().describe("JSON array of StrategyDeployment objects"),
        replace: z.boolean().default(false),
      },
      async (args) => {
        try {
          const parsed = JSON.parse(args.deployments_json);
          if (!Array.isArray(parsed)) {
            throw new Error("deployments_json must be a JSON array");
          }
          const result = await callBackend("/api/deployments", {
            method: "PUT",
            body: JSON.stringify({
              deployments: parsed,
              replace: args.replace,
            }),
          });
          return {
            content: [{ type: "text" as const, text: JSON.stringify(result, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: e.message }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "set_deployment_enabled",
      "Enable/disable a deployment.",
      {
        id: z.string(),
        enabled: z.boolean(),
      },
      async (args) => {
        try {
          const suffix = args.enabled ? "enable" : "disable";
          const result = await callBackend(
            `/api/deployments/${encodeURIComponent(args.id)}/${suffix}`,
            {
              method: "POST",
            }
          );
          return {
            content: [{ type: "text" as const, text: JSON.stringify(result, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: e.message }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "delete_deployment",
      "Delete a deployment by id.",
      {
        id: z.string(),
      },
      async (args) => {
        try {
          const result = await callBackend(`/api/deployments/${encodeURIComponent(args.id)}`, {
            method: "DELETE",
          });
          return {
            content: [{ type: "text" as const, text: JSON.stringify(result, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: e.message }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "get_security_events",
      "Read security audit events from backend.",
      {
        limit: z.number().int().positive().max(500).optional(),
        severity: z.string().optional(),
        start_time: z.string().optional().describe("ISO timestamp"),
      },
      async (args) => {
        try {
          const params = new URLSearchParams();
          if (args.limit !== undefined) params.set("limit", String(args.limit));
          if (args.severity) params.set("severity", args.severity);
          if (args.start_time) params.set("start_time", args.start_time);
          const query = params.toString();
          const result = await callBackend(`/api/security/events${query ? `?${query}` : ""}`);
          return {
            content: [{ type: "text" as const, text: JSON.stringify(result, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: e.message }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "get_risk_state",
      "Get current risk state from coordinator.",
      {},
      async () => {
        try {
          const risk = await callBackend("/api/sidecar/risk");
          return {
            content: [{ type: "text" as const, text: JSON.stringify(risk, null, 2) }],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: e.message }],
            isError: true,
          };
        }
      }
    ),
  ],
});
