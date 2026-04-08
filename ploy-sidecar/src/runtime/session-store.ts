import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";

const baseDir = resolve(process.cwd(), "run", "sidecar");

export async function ensureSidecarRunDir(): Promise<string> {
  await mkdir(baseDir, { recursive: true });
  return baseDir;
}

export async function agentRunsLogPath(): Promise<string> {
  const runDir = await ensureSidecarRunDir();
  return resolve(runDir, "agent-runs.jsonl");
}
