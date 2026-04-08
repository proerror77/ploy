/**
 * Research MCP Tools — operator-safe replay/backtest/config-compare wrappers.
 *
 * These tools shell out to `ployctl research ...` so the sidecar stays aligned
 * with the canonical operator surface instead of inventing a parallel path.
 */

import { execFile } from "node:child_process";
import { access } from "node:fs/promises";
import { constants as fsConstants } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import { createSdkMcpServer, tool } from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";

const execFileAsync = promisify(execFile);
const moduleDir = dirname(fileURLToPath(import.meta.url));

async function resolvePloyctlBin(): Promise<string> {
  const candidates = [
    process.env.PLOYCTL_BIN,
    "ployctl",
    resolve(process.cwd(), "../target/debug/ployctl"),
    resolve(process.cwd(), "target/debug/ployctl"),
    resolve(moduleDir, "../../../target/debug/ployctl"),
  ].filter((candidate): candidate is string => Boolean(candidate));

  for (const candidate of candidates) {
    if (candidate === "ployctl") {
      // Check whether ployctl is actually on PATH before accepting it.
      // Without this check the bare name is returned immediately and all
      // local-build fallbacks below are never tried, causing ENOENT on
      // checkouts where ployctl is only available as a local debug binary.
      try {
        const { execFileSync } = await import("node:child_process");
        execFileSync("which", ["ployctl"], { stdio: "ignore" });
        return candidate;
      } catch {
        continue;
      }
    }

    try {
      await access(candidate, fsConstants.X_OK);
      return candidate;
    } catch {
      continue;
    }
  }

  throw new Error(
    "Unable to find ployctl binary. Set PLOYCTL_BIN or build ployctl locally first."
  );
}

export async function runPloyctlCommand(args: string[]): Promise<string> {
  const ployctlBin = await resolvePloyctlBin();

  try {
    const { stdout, stderr } = await execFileAsync(ployctlBin, args, {
      cwd: process.cwd(),
      env: process.env,
      maxBuffer: 1024 * 1024,
    });

    const output = [stdout, stderr]
      .map((chunk) => chunk.trim())
      .filter(Boolean)
      .join("\n");

    return output || "ployctl research completed with no output";
  } catch (error: any) {
    const stderr = typeof error?.stderr === "string" ? error.stderr.trim() : "";
    const stdout = typeof error?.stdout === "string" ? error.stdout.trim() : "";
    const status = error?.code ?? "unknown";
    const detail = [stderr, stdout, error?.message].filter(Boolean).join("\n");
    throw new Error(`ployctl failed (${status}): ${detail}`);
  }
}

export async function runPloyResearchCommand(args: string[]): Promise<string> {
  return runPloyctlCommand(["research", ...args]);
}

export const researchServer = createSdkMcpServer({
  name: "research",
  version: "1.0.0",
  tools: [
    tool(
      "replay_deployment",
      "Replay realized fills for one deployment via `ployctl research replay`. Read-only.",
      {
        deployment_id: z.string().describe("Deployment resource id to replay"),
      },
      async ({ deployment_id }) => {
        try {
          const output = await runPloyResearchCommand(["replay", deployment_id]);
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
      "run_backtest",
      "Run an operator-safe backtest through `ployctl research backtest`. Uses synthetic data unless db_url is provided.",
      {
        config_path: z
          .string()
          .optional()
          .describe("Unified strategy config TOML path. Defaults to the canonical PM5D config."),
        db_url: z
          .string()
          .optional()
          .describe("Optional Postgres URL for historical market data replay."),
        start_date: z
          .string()
          .optional()
          .describe("Optional inclusive start date in YYYY-MM-DD format."),
        end_date: z
          .string()
          .optional()
          .describe("Optional inclusive end date in YYYY-MM-DD format."),
      },
      async ({ config_path, db_url, start_date, end_date }) => {
        try {
          const args = ["backtest"];
          if (config_path) args.push("--config", config_path);
          if (db_url) args.push("--db-url", db_url);
          if (start_date) args.push("--start-date", start_date);
          if (end_date) args.push("--end-date", end_date);

          const output = await runPloyResearchCommand(args);
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
      "compare_configs",
      "Compare two strategy config TOML files through `ployctl research compare`. Read-only.",
      {
        left_path: z.string().describe("Left-hand config TOML path"),
        right_path: z.string().describe("Right-hand config TOML path"),
      },
      async ({ left_path, right_path }) => {
        try {
          const output = await runPloyResearchCommand(["compare", left_path, right_path]);
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
      "check_oversight",
      "Run deterministic oversight checks through `ployctl research oversight`. Read-only.",
      {},
      async () => {
        try {
          const output = await runPloyResearchCommand(["oversight"]);
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
