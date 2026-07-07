import { readFile, rename, unlink } from "node:fs/promises";
import type { AgentRunCreateRequest } from "../contracts/operator-contracts.js";
import { agentRunRequestsPath } from "./session-store.js";

export type QueuedAgentRunRequest = {
  run_id: string;
  created_at: string;
  request: AgentRunCreateRequest;
};

export async function takeQueuedAgentRunRequests(): Promise<QueuedAgentRunRequest[]> {
  const path = await agentRunRequestsPath();
  const drainPath = `${path}.${process.pid}.${Date.now()}.drain`;
  try {
    await rename(path, drainPath);
  } catch (error: any) {
    if (error?.code === "ENOENT") return [];
    throw error;
  }

  const body = await readFile(drainPath, "utf8");
  await unlink(drainPath).catch((error: any) => {
    if (error?.code !== "ENOENT") throw error;
  });

  const requests: QueuedAgentRunRequest[] = [];
  for (const [index, line] of body.split(/\r?\n/).entries()) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      requests.push(JSON.parse(trimmed) as QueuedAgentRunRequest);
    } catch (error) {
      console.warn(
        `Skipping malformed agent run request on line ${index + 1}: ${
          error instanceof Error ? error.message : String(error)
        }`
      );
    }
  }

  return requests;
}
