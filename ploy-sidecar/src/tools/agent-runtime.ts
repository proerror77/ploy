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
      },
      async ({ status, summary }) => ({
        content: [
          {
            type: "text" as const,
            text: JSON.stringify({ status, summary }),
          },
        ],
      })
    ),
  ],
});
