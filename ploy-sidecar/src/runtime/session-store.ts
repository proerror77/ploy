import { mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";

const DEFAULT_RUN_DIR = "run/sidecar";

export async function agentRunsLogPath(): Promise<string> {
  const logPath = resolve(process.env.PLOY_AGENT_RUNS_FILE || `${DEFAULT_RUN_DIR}/agent-runs.jsonl`);
  await mkdir(dirname(logPath), { recursive: true });
  return logPath;
}

export async function agentRunRequestsPath(): Promise<string> {
  const configured = process.env.PLOY_AGENT_RUN_REQUESTS_FILE;
  const requestPath = configured
    ? resolve(configured)
    : resolve(dirname(await agentRunsLogPath()), "agent-run-requests.jsonl");
  await mkdir(dirname(requestPath), { recursive: true });
  return requestPath;
}
