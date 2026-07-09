import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { execFile, spawn } from "node:child_process";
import { appendFile, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

import { harnessSelfModificationsPath } from "./session-store.js";

const execFileAsync = promisify(execFile);

export type SelfModificationProposal = {
  kind: "self_modification_proposal";
  proposal_id: string;
  title: string;
  patch: string;
  verification_command: string;
  status: "proposed" | "applied";
  created_at: string;
  applied_at?: string;
};

export async function proposeSelfModification(params: {
  title: string;
  patch: string;
  verification_command?: string;
}): Promise<SelfModificationProposal> {
  const proposal: SelfModificationProposal = {
    kind: "self_modification_proposal",
    proposal_id: `selfmod-${randomUUID()}`,
    title: params.title,
    patch: params.patch,
    verification_command:
      params.verification_command ||
      "npm run build --prefix ploy-sidecar && npm run build --prefix ploy-frontend",
    status: "proposed",
    created_at: new Date().toISOString(),
  };
  await appendFile(await harnessSelfModificationsPath(), `${JSON.stringify(proposal)}\n`, "utf8");
  return proposal;
}

export async function applyApprovedSelfModification(params: {
  proposal_id: string;
  approval_token: string;
}): Promise<SelfModificationProposal> {
  const expectedToken = process.env.PLOY_HARNESS_SELF_MOD_APPROVAL_TOKEN;
  if (!expectedToken) {
    throw new Error("self-modification apply is not configured");
  }
  if (params.approval_token !== expectedToken) {
    throw new Error("invalid self-modification approval token");
  }

  const proposal = await readProposal(params.proposal_id);
  const repoRoot = await findRepoRoot();
  await ensureCleanWorktree(repoRoot);

  try {
    await run(repoRoot, "git", ["apply", "--check", "-"], proposal.patch);
    await run(repoRoot, "git", ["apply", "-"], proposal.patch);
    await runShell(repoRoot, proposal.verification_command);
  } catch (error) {
    await run(repoRoot, "git", ["apply", "-R", "-"], proposal.patch).catch(() => undefined);
    throw error;
  }

  const applied: SelfModificationProposal = {
    ...proposal,
    status: "applied",
    applied_at: new Date().toISOString(),
  };
  await appendFile(await harnessSelfModificationsPath(), `${JSON.stringify(applied)}\n`, "utf8");
  return applied;
}

async function readProposal(proposalId: string): Promise<SelfModificationProposal> {
  const body = await readFile(await harnessSelfModificationsPath(), "utf8");
  for (const line of body.split(/\r?\n/)) {
    if (!line.trim()) continue;
    const proposal = JSON.parse(line) as SelfModificationProposal;
    if (proposal.proposal_id === proposalId && proposal.status === "proposed") {
      return proposal;
    }
  }
  throw new Error(`self-modification proposal ${proposalId} was not found`);
}

async function findRepoRoot(): Promise<string> {
  if (process.env.PLOY_SELF_MOD_REPO_ROOT) return resolve(process.env.PLOY_SELF_MOD_REPO_ROOT);
  let current = resolve(process.cwd());
  for (;;) {
    try {
      await readFile(join(current, "Cargo.toml"), "utf8");
      await readFile(join(current, ".git", "HEAD"), "utf8");
      return current;
    } catch {
      const parent = dirname(current);
      if (parent === current) throw new Error("could not locate repo root");
      current = parent;
    }
  }
}

async function ensureCleanWorktree(cwd: string): Promise<void> {
  const { stdout } = await execFileAsync("git", ["status", "--porcelain"], { cwd });
  if (stdout.trim()) {
    throw new Error("self-modification requires a clean git worktree");
  }
}

async function run(cwd: string, command: string, args: string[], input?: string): Promise<void> {
  if (input === undefined) {
    await execFileAsync(command, args, {
      cwd,
      maxBuffer: 1024 * 1024 * 10,
    });
    return;
  }
  await new Promise<void>((resolvePromise, reject) => {
    const child = spawn(command, args, { cwd, stdio: ["pipe", "pipe", "pipe"] });
    let stderr = "";
    child.stderr.on("data", (chunk) => {
      stderr += String(chunk);
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) {
        resolvePromise();
      } else {
        reject(new Error(stderr || `${command} exited with ${code}`));
      }
    });
    child.stdin.end(input);
  });
}

async function runShell(cwd: string, command: string): Promise<void> {
  await execFileAsync("sh", ["-lc", command], {
    cwd,
    maxBuffer: 1024 * 1024 * 20,
  });
}

async function selfTest() {
  const originalPath = process.env.PLOY_HARNESS_SELF_MODIFICATIONS_FILE;
  const originalRoot = process.env.PLOY_SELF_MOD_REPO_ROOT;
  const originalToken = process.env.PLOY_HARNESS_SELF_MOD_APPROVAL_TOKEN;
  const dir = await mkdtemp(join(tmpdir(), "ploy-self-mod-"));
  const logDir = await mkdtemp(join(tmpdir(), "ploy-self-mod-log-"));
  const logPath = join(logDir, "self-mod.jsonl");

  try {
    process.env.PLOY_HARNESS_SELF_MODIFICATIONS_FILE = logPath;
    process.env.PLOY_SELF_MOD_REPO_ROOT = dir;
    process.env.PLOY_HARNESS_SELF_MOD_APPROVAL_TOKEN = "approve";
    await run(dir, "git", ["init"]);
    await run(dir, "git", ["config", "user.email", "test@example.com"]);
    await run(dir, "git", ["config", "user.name", "Test"]);
    await writeFile(join(dir, "Cargo.toml"), "[workspace]\n", "utf8");
    await writeFile(join(dir, "README.md"), "before\n", "utf8");
    await run(dir, "git", ["add", "."]);
    await run(dir, "git", ["commit", "-m", "init"]);

    const proposal = await proposeSelfModification({
      title: "update readme",
      patch: "diff --git a/README.md b/README.md\nindex 229a18c..13d29f8 100644\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-before\n+after\n",
      verification_command: "grep -q after README.md",
    });
    const applied = await applyApprovedSelfModification({
      proposal_id: proposal.proposal_id,
      approval_token: "approve",
    });
    assert.equal(applied.status, "applied");
    assert.equal(await readFile(join(dir, "README.md"), "utf8"), "after\n");
    assert.match(await readFile(logPath, "utf8"), /"status":"applied"/);
  } finally {
    restoreEnv("PLOY_HARNESS_SELF_MODIFICATIONS_FILE", originalPath);
    restoreEnv("PLOY_SELF_MOD_REPO_ROOT", originalRoot);
    restoreEnv("PLOY_HARNESS_SELF_MOD_APPROVAL_TOKEN", originalToken);
    await rm(dir, { recursive: true, force: true });
    await rm(logDir, { recursive: true, force: true });
  }
}

function restoreEnv(key: string, value: string | undefined) {
  if (value === undefined) {
    delete process.env[key];
  } else {
    process.env[key] = value;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await selfTest();
}
