import { createSdkMcpServer, tool } from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";

export const agentRuntimeServer = createSdkMcpServer({
  name: "agent-runtime",
  version: "1.0.0",
  tools: [
    tool(
      "complete_task",
      "Signal that the current agent run has reached success, partial completion, or a blocker.",
      {
        status: z.enum(["success", "partial", "blocked"]).default("success"),
        summary: z.string().describe("Concise operator-facing completion summary"),
        decision: z
          .enum(["continue", "pass", "trade", "monitor", "blocked"])
          .optional()
          .describe("Final operator decision, if one was reached"),
        grok_decision: z
          .enum(["trade", "pass", "not_queried"])
          .optional()
          .describe("Required for Grok Builder contracts"),
        evidence: z
          .array(z.string())
          .default([])
          .describe("Short evidence bullets that justify the completion status"),
        blockers: z
          .array(z.string())
          .default([])
          .describe("Concrete blockers when status is partial or blocked"),
        next_action: z.string().optional().describe("Single next action for the operator or retry loop"),
      },
      async ({ status, summary, decision, grok_decision, evidence, blockers, next_action }) => ({
        content: [
          {
            type: "text" as const,
            text: JSON.stringify({
              status,
              summary,
              decision,
              grok_decision,
              evidence,
              blockers,
              next_action,
            }),
          },
        ],
      })
    ),
  ],
});
