/**
 * Ploy Backend MCP Tools — Trading platform control-plane client.
 *
 * The sidecar is an operator-facing agent client. It inspects system state,
 * deployment resources, and trading snapshots through the ployd control plane.
 */

import { createSdkMcpServer, tool } from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";

const PLOY_API = process.env.PLOY_API_URL || "http://localhost:8081";
const PLOY_ADMIN_TOKEN = process.env.PLOY_API_ADMIN_TOKEN || process.env.PLOY_ADMIN_TOKEN;

type DesiredState = "running" | "paused" | "stopped";
type DeploymentState = "enabled" | "draining" | "disabled" | "archived";

type SystemStatusResponse = {
  status: string;
  uptime_seconds: number;
  version: string;
  strategy: string;
  last_trade_time: string | null;
  websocket_connected: boolean;
  database_connected: boolean;
  error_count_1h: number;
};

type DeploymentSummaryResponse = {
  deployment_id: string;
  deployment_state: DeploymentState;
  desired_state: DesiredState;
  observed_state: string;
};

type DeploymentRecordResponse = DeploymentSummaryResponse & {
  bundle_id: string;
  runtime_mode: string;
};

type TradingStateSnapshot = {
  deployment_id: string;
  runtime_mode: string;
  intents: unknown[];
  orders: unknown[];
  fills: unknown[];
  positions: unknown[];
  pnl: {
    realized_pnl: string;
    unrealized_pnl: string;
    total_fees: string;
    net_pnl: string;
  };
  risk: {
    pending_intents: number;
    active_orders: number;
    open_positions: number;
    gross_exposure: string;
  };
};

type PaperIntentResponse = {
  deployment_id: string;
  intent_id: string;
  order_id: string;
  state: string;
};

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

async function callBackend<T>(path: string, options?: RequestInit): Promise<T> {
  const resp = await ployFetch(path, options);
  if (!resp.ok) {
    const err = await resp.text();
    throw new Error(`Backend error (${resp.status}): ${err}`);
  }
  return (await resp.json()) as T;
}

export const ployBackendServer = createSdkMcpServer({
  name: "ploy-backend",
  version: "2.0.0",
  tools: [
    tool(
      "get_system_status",
      "Get trading platform daemon health, uptime, and version from the control plane.",
      {},
      async () => {
        try {
          const status = await callBackend<SystemStatusResponse>("/api/system/status");
          return {
            content: [{ type: "text" as const, text: JSON.stringify(status, null, 2) }],
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
      "get_trading_state",
      "Get the canonical trading-state snapshots across deployment resources.",
      {},
      async () => {
        try {
          const tradingState = await callBackend<TradingStateSnapshot[]>("/api/trading/state");
          return {
            content: [{ type: "text" as const, text: JSON.stringify(tradingState, null, 2) }],
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
      "List deployment resources from the ployd control plane.",
      {},
      async () => {
        try {
          const items = await callBackend<DeploymentSummaryResponse[]>("/api/deployments");
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
      "Get one deployment resource, including bundle_id and runtime_mode.",
      {
        id: z.string(),
      },
      async (args) => {
        try {
          const item = await callBackend<DeploymentRecordResponse>(
            `/api/deployments/${encodeURIComponent(args.id)}`
          );
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
      "apply_deployment",
      "Create or replace a deployment resource through the control plane. Prefer runtime_mode=paper unless explicitly told otherwise.",
      {
        deployment_id: z.string().describe("Deployment resource id"),
        bundle_id: z.string().describe("Strategy bundle id"),
        runtime_mode: z
          .string()
          .default("paper")
          .describe("Runtime mode, usually paper for sidecar-managed flows"),
        desired_state: z
          .enum(["running", "paused", "stopped"])
          .default("running")
          .describe("Desired lifecycle state"),
        deployment_state: z
          .enum(["enabled", "draining", "disabled", "archived"])
          .default("enabled")
          .describe("Operator lifecycle gate"),
      },
      async (args) => {
        try {
          const item = await callBackend<DeploymentRecordResponse>(
            `/api/deployments/${encodeURIComponent(args.deployment_id)}`,
            {
              method: "PUT",
              body: JSON.stringify(args),
            }
          );
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
      "set_deployment_state",
      "Pause, resume, or stop a deployment resource through the control plane.",
      {
        id: z.string(),
        desired_state: z.enum(["running", "paused", "stopped"]).optional(),
        deployment_state: z
          .enum(["enabled", "draining", "disabled", "archived"])
          .optional(),
      },
      async (args) => {
        try {
          const item = await callBackend<DeploymentRecordResponse>(
            `/api/deployments/${encodeURIComponent(args.id)}/control`,
            {
              method: "POST",
              body: JSON.stringify({
                desired_state: args.desired_state ?? null,
                deployment_state: args.deployment_state ?? null,
              }),
            }
          );
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
      "submit_paper_intent",
      "Submit a paper-only trading intent through the ployd control plane for a deployment resource.",
      {
        deployment_id: z.string().describe("Paper deployment resource id"),
        market_id: z.string().describe("Market identifier or slug"),
        token_id: z.string().describe("Outcome token id"),
        side: z.enum(["buy", "sell"]).default("buy"),
        quantity: z.number().positive().describe("Requested quantity"),
        limit_price: z.number().min(0).max(1).optional(),
        purpose: z
          .enum(["entry", "exit", "reduce", "hedge", "cancel"])
          .default("entry"),
      },
      async (args) => {
        try {
          const result = await callBackend<PaperIntentResponse>(
            `/api/deployments/${encodeURIComponent(args.deployment_id)}/intents`,
            {
              method: "POST",
              body: JSON.stringify({
                market_id: args.market_id,
                token_id: args.token_id,
                side: args.side,
                quantity: args.quantity,
                limit_price: args.limit_price ?? null,
                purpose: args.purpose,
              }),
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
  ],
});
