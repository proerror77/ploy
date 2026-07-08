import assert from "node:assert/strict";
import { appendFile, mkdtemp, readFile, rename, rm, unlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import type { AgentRunCreateRequest } from "../contracts/operator-contracts.js";
import { agentRunInProgressRequestsPath, agentRunRequestsPath } from "./session-store.js";

export type QueuedAgentRunRequest = {
  run_id: string;
  created_at: string;
  request: AgentRunCreateRequest;
  attempt?: number;
  last_retry_reason?: string;
  last_retried_at?: string;
};

export type QueuedAgentRunBatch = {
  requests: QueuedAgentRunRequest[];
  acknowledge: () => Promise<void>;
};

export function queuedAgentRunAttempt(queued: QueuedAgentRunRequest): number {
  return Number.isInteger(queued.attempt) && (queued.attempt ?? 0) > 0 ? queued.attempt! : 0;
}

export function maxAgentRunRetries(): number {
  const parsed = Number.parseInt(process.env.SIDECAR_AGENT_RUN_MAX_RETRIES || "1", 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 0;
}

export async function requeueAgentRunRequest(
  queued: QueuedAgentRunRequest,
  reason: string
): Promise<QueuedAgentRunRequest | null> {
  const nextAttempt = queuedAgentRunAttempt(queued) + 1;
  if (nextAttempt > maxAgentRunRetries()) return null;

  const retry: QueuedAgentRunRequest = {
    ...queued,
    attempt: nextAttempt,
    last_retry_reason: reason,
    last_retried_at: new Date().toISOString(),
  };
  await appendFile(await agentRunRequestsPath(), `${JSON.stringify(retry)}\n`, "utf8");
  return retry;
}

export async function claimQueuedAgentRunRequests(): Promise<QueuedAgentRunBatch | null> {
  const path = await agentRunRequestsPath();
  const inProgressPath = await agentRunInProgressRequestsPath();

  const recovered = await readClaimedBatch(inProgressPath);
  if (recovered) return recovered;

  try {
    await rename(path, inProgressPath);
  } catch (error: any) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }

  return readClaimedBatch(inProgressPath);
}

async function readClaimedBatch(path: string): Promise<QueuedAgentRunBatch | null> {
  let body: string;
  try {
    body = await readFile(path, "utf8");
  } catch (error: any) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }

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

  return {
    requests,
    acknowledge: () =>
      unlink(path).catch((error: any) => {
        if (error?.code !== "ENOENT") throw error;
      }),
  };
}

async function selfTest() {
  const originalRequestsPath = process.env.PLOY_AGENT_RUN_REQUESTS_FILE;
  const originalInProgressPath = process.env.PLOY_AGENT_RUN_IN_PROGRESS_FILE;
  const originalMaxRetries = process.env.SIDECAR_AGENT_RUN_MAX_RETRIES;
  const dir = await mkdtemp(join(tmpdir(), "ploy-run-requests-"));

  try {
    process.env.PLOY_AGENT_RUN_REQUESTS_FILE = join(dir, "queue.jsonl");
    process.env.PLOY_AGENT_RUN_IN_PROGRESS_FILE = join(dir, "in-progress.jsonl");
    process.env.SIDECAR_AGENT_RUN_MAX_RETRIES = "1";

    const request: AgentRunCreateRequest = {
      autonomy_mode: "research_until_blocked",
      budget_usd: 1,
      max_turns: 3,
      objective: "self-test",
      run_contract: 'completion_signal = "required"',
      run_packet: "packet",
      strategy_profile: "self-test",
      symbols: ["TEST"],
      target_evidence: "diagnostic",
    };
    const queued: QueuedAgentRunRequest = {
      run_id: "run-self-test",
      created_at: "2026-07-08T00:00:00.000Z",
      request,
    };

    await appendFile(await agentRunRequestsPath(), `${JSON.stringify(queued)}\n`, "utf8");
    const claimed = await claimQueuedAgentRunRequests();
    assert.equal(claimed?.requests.length, 1);
    assert.equal(claimed?.requests[0]?.run_id, queued.run_id);

    const recovered = await claimQueuedAgentRunRequests();
    assert.equal(recovered?.requests.length, 1);
    assert.equal(recovered?.requests[0]?.run_id, queued.run_id);

    await claimed?.acknowledge();
    assert.equal(await claimQueuedAgentRunRequests(), null);

    const retry = await requeueAgentRunRequest(queued, "completion_signal missing");
    assert.equal(retry?.attempt, 1);
    assert.equal(await requeueAgentRunRequest(retry!, "still missing"), null);
    assert.match(await readFile(await agentRunRequestsPath(), "utf8"), /completion_signal missing/);
  } finally {
    if (originalRequestsPath === undefined) {
      delete process.env.PLOY_AGENT_RUN_REQUESTS_FILE;
    } else {
      process.env.PLOY_AGENT_RUN_REQUESTS_FILE = originalRequestsPath;
    }
    if (originalInProgressPath === undefined) {
      delete process.env.PLOY_AGENT_RUN_IN_PROGRESS_FILE;
    } else {
      process.env.PLOY_AGENT_RUN_IN_PROGRESS_FILE = originalInProgressPath;
    }
    if (originalMaxRetries === undefined) {
      delete process.env.SIDECAR_AGENT_RUN_MAX_RETRIES;
    } else {
      process.env.SIDECAR_AGENT_RUN_MAX_RETRIES = originalMaxRetries;
    }
    await rm(dir, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await selfTest();
}
