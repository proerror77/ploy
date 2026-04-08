/**
 * Diagnostics MCP Tools — evidence-backed platform/deployment diagnosis.
 *
 * These tools shell out to `ployctl system diagnose` and
 * `ployctl trading diagnose <deployment-id>` so the sidecar consumes the
 * canonical Rust diagnostics surface instead of inventing one in TypeScript.
 */

import { createSdkMcpServer, tool } from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";

import { runPloyctlCommand } from "./research.js";

const PLOY_API = process.env.PLOY_API_URL || "http://localhost:8081";

async function createProposalRequest(body: {
  action_kind: "pause_deployment" | "drain_deployment" | "reduce_max_exposure";
  target_deployment_id: string;
  rationale: string;
  evidence: string[];
  source_run_id?: string;
  proposed_max_gross_exposure?: string | number | null;
}): Promise<string> {
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

  const response = await fetch(`${PLOY_API}/api/proposals`, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`Proposal API error (${response.status}): ${text}`);
  }
  return text;
}

export const diagnosticsServer = createSdkMcpServer({
  name: "diagnostics",
  version: "1.0.0",
  tools: [
    tool(
      "diagnose_platform",
      "Run `ployctl system diagnose` and return an evidence-backed platform diagnostics report. Read-only.",
      {},
      async () => {
        try {
          const output = await runPloyctlCommand(["system", "diagnose"]);
          return {
            content: [{ type: "text" as const, text: output }],
          };
        } catch (error: any) {
          return {
            content: [{ type: "text" as const, text: error.message }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "diagnose_deployment",
      "Run `ployctl trading diagnose <deployment-id>` and return an evidence-backed deployment diagnostics report. Read-only.",
      {
        deployment_id: z.string().describe("Deployment resource id to diagnose"),
      },
      async ({ deployment_id }) => {
        try {
          const output = await runPloyctlCommand(["trading", "diagnose", deployment_id]);
          return {
            content: [{ type: "text" as const, text: output }],
          };
        } catch (error: any) {
          return {
            content: [{ type: "text" as const, text: error.message }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "create_safety_proposal",
      "Create an operator-approved safety proposal such as pausing a deployment, draining it, or reducing max exposure. This does not execute the action directly.",
      {
        action_kind: z.enum(["pause_deployment", "drain_deployment", "reduce_max_exposure"]),
        target_deployment_id: z.string().describe("Deployment resource id the proposal targets"),
        rationale: z.string().describe("Short operator-facing reason for the proposal"),
        evidence: z
          .array(z.string())
          .min(1)
          .describe("Concrete evidence that explains why the proposal exists"),
        source_run_id: z.string().optional().describe("Optional sidecar run id"),
        proposed_max_gross_exposure: z
          .union([z.string(), z.number()])
          .optional()
          .describe("Required when action_kind is reduce_max_exposure"),
      },
      async ({
        action_kind,
        target_deployment_id,
        rationale,
        evidence,
        source_run_id,
        proposed_max_gross_exposure,
      }) => {
        try {
          const output = await createProposalRequest({
            action_kind,
            target_deployment_id,
            rationale,
            evidence,
            source_run_id,
            proposed_max_gross_exposure: proposed_max_gross_exposure ?? null,
          });
          return {
            content: [{ type: "text" as const, text: output }],
          };
        } catch (error: any) {
          return {
            content: [{ type: "text" as const, text: error.message }],
            isError: true,
          };
        }
      }
    ),
  ],
});
