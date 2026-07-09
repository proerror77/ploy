import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { execFile, spawn } from "node:child_process";
import { appendFile, chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
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
  branch_name?: string;
  commit_sha?: string;
  pull_request_url?: string;
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
  publish_pull_request?: boolean;
  branch_name?: string;
  commit_message?: string;
  pull_request_title?: string;
  pull_request_body?: string;
  base_branch?: string;
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
  const publishPullRequest = params.publish_pull_request === true;
  const branchName = publishPullRequest
    ? params.branch_name || `selfmod/${proposal.proposal_id.slice(-12)}`
    : undefined;
  const originalBranch = publishPullRequest ? await currentBranch(repoRoot) : undefined;
  let switchedBranch = false;
  let patchApplied = false;
  let committed = false;
  let pushedBranch = false;

  try {
    if (publishPullRequest) {
      if (process.env.PLOY_HARNESS_SELF_MOD_ALLOW_PR !== "true") {
        throw new Error("self-modification PR publishing is not enabled");
      }
      if (!branchName) throw new Error("self-modification PR branch name is missing");
      await run(repoRoot, "git", ["check-ref-format", "--branch", branchName]);
      await run(repoRoot, "git", ["switch", "-c", branchName]);
      switchedBranch = true;
    }
    await run(repoRoot, "git", ["apply", "--check", "-"], proposal.patch);
    await run(repoRoot, "git", ["apply", "-"], proposal.patch);
    patchApplied = true;
    await runShell(repoRoot, proposal.verification_command);
    if (publishPullRequest) {
      await run(repoRoot, "git", ["add", "-A"]);
      await run(repoRoot, "git", ["diff", "--cached", "--quiet"]).then(
        () => {
          throw new Error("self-modification patch produced no staged changes");
        },
        () => undefined
      );
      await run(repoRoot, "git", [
        "commit",
        "-m",
        params.commit_message || proposal.title,
      ]);
      committed = true;
      const commitSha = (await runOutput(repoRoot, "git", ["rev-parse", "HEAD"])).trim();
      await run(repoRoot, "git", ["push", "-u", "origin", branchName!]);
      pushedBranch = true;
      const prUrl = (
        await runOutput(repoRoot, "gh", [
          "pr",
          "create",
          "--base",
          params.base_branch || "main",
          "--head",
          branchName!,
          "--title",
          params.pull_request_title || proposal.title,
          "--body",
          params.pull_request_body ||
            [
              "Approval-gated harness self-modification.",
              "",
              `Proposal: ${proposal.proposal_id}`,
              `Verification: ${proposal.verification_command}`,
              "",
              "Deployment: do not deploy from this branch; merge to main and use the repo's existing deployment workflows.",
            ].join("\n"),
        ])
      ).trim();
      return recordApplied(proposal, {
        branch_name: branchName,
        commit_sha: commitSha,
        pull_request_url: prUrl,
      });
    }
  } catch (error) {
    if (patchApplied && !committed) {
      await run(repoRoot, "git", ["apply", "-R", "-"], proposal.patch).catch(() => undefined);
    }
    if (switchedBranch && originalBranch) {
      await run(repoRoot, "git", ["switch", originalBranch]).catch(() => undefined);
      await run(repoRoot, "git", ["branch", "-D", branchName!]).catch(() => undefined);
    }
    if (pushedBranch) {
      await run(repoRoot, "git", ["push", "origin", `:${branchName}`]).catch(() => undefined);
    }
    throw error;
  }

  return recordApplied(proposal, {});
}

async function recordApplied(
  proposal: SelfModificationProposal,
  updates: Partial<SelfModificationProposal>
): Promise<SelfModificationProposal> {
  const applied: SelfModificationProposal = {
    ...proposal,
    ...updates,
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

async function currentBranch(cwd: string): Promise<string> {
  const branch = (await runOutput(cwd, "git", ["branch", "--show-current"])).trim();
  if (!branch) throw new Error("self-modification PR publishing requires a named git branch");
  return branch;
}

async function runOutput(cwd: string, command: string, args: string[]): Promise<string> {
  const { stdout } = await execFileAsync(command, args, {
    cwd,
    maxBuffer: 1024 * 1024 * 10,
  });
  return stdout;
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
  const originalAllowPr = process.env.PLOY_HARNESS_SELF_MOD_ALLOW_PR;
  const originalPathValue = process.env.PATH;
  const dir = await mkdtemp(join(tmpdir(), "ploy-self-mod-"));
  const logDir = await mkdtemp(join(tmpdir(), "ploy-self-mod-log-"));
  const binDir = await mkdtemp(join(tmpdir(), "ploy-self-mod-bin-"));
  const remoteDir = await mkdtemp(join(tmpdir(), "ploy-self-mod-origin-"));
  const logPath = join(logDir, "self-mod.jsonl");

  try {
    process.env.PLOY_HARNESS_SELF_MODIFICATIONS_FILE = logPath;
    process.env.PLOY_SELF_MOD_REPO_ROOT = dir;
    process.env.PLOY_HARNESS_SELF_MOD_APPROVAL_TOKEN = "approve";
    process.env.PLOY_HARNESS_SELF_MOD_ALLOW_PR = "true";
    process.env.PATH = `${binDir}${delimiter}${process.env.PATH || ""}`;
    await writeFile(
      join(binDir, "gh"),
      "#!/bin/sh\nprintf '%s\\n' 'https://github.example/ploy/pull/1'\n",
      "utf8"
    );
    await chmod(join(binDir, "gh"), 0o755);
    await run(dir, "git", ["init"]);
    await run(dir, "git", ["checkout", "-b", "main"]);
    await run(dir, "git", ["config", "user.email", "test@example.com"]);
    await run(dir, "git", ["config", "user.name", "Test"]);
    await writeFile(join(dir, "Cargo.toml"), "[workspace]\n", "utf8");
    await writeFile(join(dir, "README.md"), "before\n", "utf8");
    await run(dir, "git", ["add", "."]);
    await run(dir, "git", ["commit", "-m", "init"]);
    await mkdir(remoteDir, { recursive: true });
    await run(remoteDir, "git", ["init", "--bare"]);
    await run(dir, "git", ["remote", "add", "origin", remoteDir]);
    await run(dir, "git", ["push", "-u", "origin", "main"]);

    const proposal = await proposeSelfModification({
      title: "update readme",
      patch: "diff --git a/README.md b/README.md\nindex 229a18c..13d29f8 100644\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-before\n+after\n",
      verification_command: "grep -q after README.md",
    });
    const applied = await applyApprovedSelfModification({
      proposal_id: proposal.proposal_id,
      approval_token: "approve",
      publish_pull_request: true,
      branch_name: "selfmod/test",
    });
    assert.equal(applied.status, "applied");
    assert.equal(applied.branch_name, "selfmod/test");
    assert.match(applied.commit_sha ?? "", /^[0-9a-f]{40}$/);
    assert.equal(applied.pull_request_url, "https://github.example/ploy/pull/1");
    assert.equal(await readFile(join(dir, "README.md"), "utf8"), "after\n");
    assert.match(await readFile(logPath, "utf8"), /"status":"applied"/);
    assert.equal((await runOutput(dir, "git", ["branch", "--show-current"])).trim(), "selfmod/test");

    const failedProposal = await proposeSelfModification({
      title: "failing change",
      patch: "diff --git a/FAIL.md b/FAIL.md\nnew file mode 100644\nindex 0000000..257cc56\n--- /dev/null\n+++ b/FAIL.md\n@@ -0,0 +1 @@\n+fail\n",
      verification_command: "false",
    });
    await assert.rejects(
      applyApprovedSelfModification({
        proposal_id: failedProposal.proposal_id,
        approval_token: "approve",
        publish_pull_request: true,
        branch_name: "selfmod/fail",
      })
    );
    assert.equal((await runOutput(dir, "git", ["branch", "--show-current"])).trim(), "selfmod/test");
    assert.equal((await runOutput(dir, "git", ["branch", "--list", "selfmod/fail"])).trim(), "");
  } finally {
    restoreEnv("PLOY_HARNESS_SELF_MODIFICATIONS_FILE", originalPath);
    restoreEnv("PLOY_SELF_MOD_REPO_ROOT", originalRoot);
    restoreEnv("PLOY_HARNESS_SELF_MOD_APPROVAL_TOKEN", originalToken);
    restoreEnv("PLOY_HARNESS_SELF_MOD_ALLOW_PR", originalAllowPr);
    restoreEnv("PATH", originalPathValue);
    await rm(dir, { recursive: true, force: true });
    await rm(logDir, { recursive: true, force: true });
    await rm(binDir, { recursive: true, force: true });
    await rm(remoteDir, { recursive: true, force: true });
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
