/**
 * Ploy Sidecar — Claude Agent SDK operator client
 *
 * Runs research and oversight loops while staying grounded in the
 * trading-platform control plane:
 * 1. Control-plane inspection -> system status, deployments, trading state
 * 2. Research tools -> replay, backtest, config compare
 * 3. External context -> ESPN, Polymarket, WebSearch when useful
 * 4. Oversight output -> alerts and operator recommendations only
 *
 * Architecture:
 *   Claude Sidecar (this)  ->  research skills (ESPN, Polymarket, WebSearch)
 *                          ->  ployd control plane (via Rust backend MCP)
 */

import { query } from "@anthropic-ai/claude-agent-sdk";
import { espnServer } from "./tools/espn.js";
import { polymarketServer } from "./tools/polymarket.js";
import { diagnosticsServer } from "./tools/diagnostics.js";
import { ployBackendServer } from "./tools/ploy-backend.js";
import { researchServer, runPloyResearchCommand } from "./tools/research.js";
import { researchOutputSchema } from "./schemas/output.js";
import { collectDiagnosticCandidates } from "./runtime/diagnostics.js";
import {
  buildRunRecord,
  newRunId,
  recordAgentRun,
  type AgentToolCallRecord,
} from "./runtime/run-recorder.js";

// -- Config ---------------------------------------------------------

function isMiniMaxAnthropicEndpoint(baseUrl: string | undefined): boolean {
  if (!baseUrl) return false;

  try {
    const parsed = new URL(baseUrl);
    const isMiniMaxHost =
      parsed.hostname.includes("minimax.io") || parsed.hostname.includes("minimaxi.com");
    return isMiniMaxHost && parsed.pathname.includes("/anthropic");
  } catch {
    return (
      baseUrl.includes("api.minimax.io/anthropic") ||
      baseUrl.includes("api.minimaxi.com/anthropic")
    );
  }
}

function applyMiniMaxCompatEnv(): string | null {
  if (!isMiniMaxAnthropicEndpoint(process.env.ANTHROPIC_BASE_URL)) {
    return null;
  }

  const minimaxModel = process.env.MINIMAX_ANTHROPIC_MODEL || "MiniMax-M2.5";
  const anthropicApiKey = process.env.ANTHROPIC_API_KEY?.trim();

  if (anthropicApiKey && !process.env.ANTHROPIC_CUSTOM_HEADERS) {
    process.env.ANTHROPIC_CUSTOM_HEADERS = `Authorization: Bearer ${anthropicApiKey}`;
  }

  if (!process.env.ANTHROPIC_DEFAULT_OPUS_MODEL) {
    process.env.ANTHROPIC_DEFAULT_OPUS_MODEL = minimaxModel;
  }
  if (!process.env.ANTHROPIC_DEFAULT_SONNET_MODEL) {
    process.env.ANTHROPIC_DEFAULT_SONNET_MODEL = minimaxModel;
  }
  if (!process.env.ANTHROPIC_DEFAULT_HAIKU_MODEL) {
    process.env.ANTHROPIC_DEFAULT_HAIKU_MODEL = minimaxModel;
  }

  return minimaxModel;
}

const minimaxCompatModel = applyMiniMaxCompatEnv();
const MODEL = process.env.SIDECAR_MODEL || "sonnet";
const POLL_INTERVAL = parseInt(process.env.SIDECAR_POLL_INTERVAL_SECS || "300", 10) * 1000;
const MAX_BUDGET = parseFloat(process.env.SIDECAR_MAX_BUDGET_USD || "1.00");
const DRY_RUN = process.env.SIDECAR_DRY_RUN !== "false";
const PLOY_API = process.env.PLOY_API_URL || "http://localhost:8081";

// -- System Prompt --------------------------------------------------

const SYSTEM_PROMPT = `You are the Ploy Trading Platform Sidecar.

## Your Mission
Run research and oversight loops while staying grounded in the trading platform control plane.
You are an operator-facing research and monitoring client, not a strategy or execution path.

## Control Plane Contract
- Deployments are resources with deployment_id, desired_state, and observed_state.
- Trading state comes from canonical /api/trading/state snapshots.
- Do not assume legacy /api/sidecar/*, /api/config, or enable/disable endpoints exist.
- Use control-plane read APIs first: get_system_status, get_trading_state, list_deployments, get_deployment.
- Research tools are read-only/operator-safe: replay_deployment, run_backtest, and compare_configs.
- Do not submit intents or mutate deployment state from this sidecar loop.

## Decision Framework
1. Inspect platform health, deployment resources, and trading snapshots first.
2. Look for suspicious deployments, behavior drift, exposure growth, order buildup, or PnL deterioration.
3. Start from deterministic oversight_signals and oversight_playbook in the runtime context; do not ignore them without explanation.
4. Prefer the Rust-provided oversight_playbook before inventing a different next step.
5. Use replay_deployment, run_backtest, and compare_configs when they help explain current state.
5a. Use diagnose_platform or diagnose_deployment when you need evidence-backed root-cause context before making a recommendation.
6. Use ESPN, Polymarket, and WebSearch only as external diagnostic context, never as direct trade triggers.
7. Return oversight alerts and operator recommendations only. Recommendations may include replay, backtest, config compare, diagnostics, pause review, proposal creation, or human review.

## Safety Rules (NEVER violate)
- Never submit a trading intent from this loop.
- Never mutate deployment state from this loop.
- If evidence is strong enough to justify operator review, you may create a safety proposal. Proposal creation is allowed because it still requires explicit operator approval before any runtime action happens.
- Never present an external market scan as a direct trade recommendation.
- Default to MONITOR or HUMAN_FOLLOW_UP when evidence is incomplete.

## Output Format
Return structured JSON with summary, research_reports[], oversight_alerts[], and operator_recommendations[].
`;

type RuntimeContext = {
  system: {
    status: string;
    uptime_seconds: number;
    error_count_1h: number;
  } | null;
  trading: {
    tracked_deployments: number;
    pending_intents: number;
    active_orders: number;
    open_positions: number;
    gross_exposure: number;
    net_pnl: number;
    sample: Array<{
      deployment_id: string;
      runtime_mode: string;
      pending_intents: number;
      active_orders: number;
      open_positions: number;
      net_pnl: string;
    }>;
  } | null;
  deployments: {
    total: number;
    running: number;
    paused: number;
    stopped: number;
    sample: Array<{
      deployment_id: string;
      bundle_id: string;
      runtime_mode: string;
      desired_state: string;
      observed_state: string;
    }>;
  } | null;
  oversight_signals: OversightSignal[];
  oversight_playbook: OversightAction[];
  diagnostic_candidates: string[];
};

type TradingSnapshot = {
  deployment_id: string;
  runtime_mode: string;
  pnl?: {
    net_pnl?: string;
  };
  risk?: {
    pending_intents?: number;
    active_orders?: number;
    open_positions?: number;
    gross_exposure?: string;
  };
};

type DeploymentSummary = {
  deployment_id: string;
  bundle_id: string;
  runtime_mode: string;
  desired_state: string;
  observed_state: string;
};

type OversightSignal = {
  severity: "info" | "warning" | "critical";
  kind: string;
  deployment_id?: string;
  message: string;
  recommended_action: string;
  evidence: string[];
};

type OversightAction = {
  kind: string;
  target: string;
  rationale: string;
  operator_command: string;
  config_hint?: string | null;
  evidence: string[];
};

type OversightReport = {
  timestamp: string;
  platform_status: string;
  deployments_reviewed: number;
  signal_count: number;
  signals: OversightSignal[];
  recommended_actions: OversightAction[];
};

async function backendFetchJson<T>(path: string): Promise<T | null> {
  try {
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

    const resp = await fetch(`${PLOY_API}${path}`, { headers });
    if (!resp.ok) return null;
    return (await resp.json()) as T;
  } catch {
    return null;
  }
}

async function buildOversightReport(): Promise<OversightReport | null> {
  try {
    const output = await runPloyResearchCommand(["oversight"]);
    return JSON.parse(output) as OversightReport;
  } catch {
    return null;
  }
}

async function buildRuntimeContext(): Promise<RuntimeContext> {
  const [system, trading, deployments, oversightReport] = await Promise.all([
    backendFetchJson<{
      status: string;
      uptime_seconds: number;
      error_count_1h: number;
    }>("/api/system/status"),
    backendFetchJson<TradingSnapshot[]>("/api/trading/state"),
    backendFetchJson<DeploymentSummary[]>("/api/deployments"),
    buildOversightReport(),
  ]);

  const tradingSnapshots = Array.isArray(trading) ? trading : [];
  const deploymentSnapshots = Array.isArray(deployments) ? deployments : [];

  return {
    system: system
      ? {
          status: system.status,
          uptime_seconds: system.uptime_seconds,
          error_count_1h: system.error_count_1h,
        }
      : null,
    trading: tradingSnapshots.length
      ? {
          tracked_deployments: tradingSnapshots.length,
          pending_intents: tradingSnapshots.reduce(
            (sum, snapshot) => sum + (snapshot.risk?.pending_intents ?? 0),
            0
          ),
          active_orders: tradingSnapshots.reduce(
            (sum, snapshot) => sum + (snapshot.risk?.active_orders ?? 0),
            0
          ),
          open_positions: tradingSnapshots.reduce(
            (sum, snapshot) => sum + (snapshot.risk?.open_positions ?? 0),
            0
          ),
          gross_exposure: tradingSnapshots.reduce(
            (sum, snapshot) => sum + parseFloat(snapshot.risk?.gross_exposure ?? "0"),
            0
          ),
          net_pnl: tradingSnapshots.reduce(
            (sum, snapshot) => sum + parseFloat(snapshot.pnl?.net_pnl ?? "0"),
            0
          ),
          sample: tradingSnapshots.slice(0, 12).map((snapshot) => ({
            deployment_id: snapshot.deployment_id,
            runtime_mode: snapshot.runtime_mode,
            pending_intents: snapshot.risk?.pending_intents ?? 0,
            active_orders: snapshot.risk?.active_orders ?? 0,
            open_positions: snapshot.risk?.open_positions ?? 0,
            net_pnl: snapshot.pnl?.net_pnl ?? "0",
          })),
        }
      : null,
    deployments: deploymentSnapshots.length
      ? {
          total: deploymentSnapshots.length,
          running: deploymentSnapshots.filter((d) => d.desired_state === "running").length,
          paused: deploymentSnapshots.filter((d) => d.desired_state === "paused").length,
          stopped: deploymentSnapshots.filter((d) => d.desired_state === "stopped").length,
          sample: deploymentSnapshots.slice(0, 12).map((d) => ({
            deployment_id: d.deployment_id,
            bundle_id: d.bundle_id,
            runtime_mode: d.runtime_mode,
            desired_state: d.desired_state,
            observed_state: d.observed_state,
          })),
        }
      : null,
    oversight_signals: Array.isArray(oversightReport?.signals) ? oversightReport.signals : [],
    oversight_playbook: Array.isArray(oversightReport?.recommended_actions)
      ? oversightReport.recommended_actions
      : [],
    diagnostic_candidates: collectDiagnosticCandidates(
      Array.isArray(oversightReport?.signals) ? oversightReport.signals : []
    ),
  };
}

// -- Main Loop ------------------------------------------------------

async function runScanCycle(): Promise<void> {
  const timestamp = new Date().toISOString();
  const runId = newRunId();
  const startedAt = new Date().toISOString();
  console.log(`\n[${timestamp}] Starting scan cycle (model=${MODEL}, dry_run=${DRY_RUN})`);
  console.log(`  Run ID: ${runId}`);

  let sessionId: string | null = null;
  let totalCostUsd: number | null = null;
  let failureReason: string | null = null;
  let resultOutput: unknown = null;
  let runtimeContext: RuntimeContext = {
    system: null,
    trading: null,
    deployments: null,
    oversight_signals: [],
    oversight_playbook: [],
    diagnostic_candidates: [],
  };
  const toolCalls: AgentToolCallRecord[] = [];

  try {
    runtimeContext = await buildRuntimeContext();

    for await (const message of query({
      prompt: `Current time: ${timestamp}

Runtime context snapshot:
${JSON.stringify(runtimeContext, null, 2)}

Run one research-and-oversight cycle:
1. Inspect the platform state and deployment snapshots first
2. Start from the deterministic oversight_signals and oversight_playbook in the runtime context
3. Identify suspicious deployments, drift signals, or operational anomalies
4. Prefer the Rust-provided oversight_playbook before inventing a different next step
5. Use replay_deployment, run_backtest, compare_configs, diagnose_platform, and diagnose_deployment when they help explain the current state
6. If the evidence supports an operator-mediated safety action, use create_safety_proposal instead of suggesting a hidden mutation
7. Use ESPN, Polymarket, and WebSearch only as external context when relevant
8. Return alerts and operator recommendations only; do not propose direct trading actions

Return your structured analysis.`,
      options: {
        model: MODEL,
        systemPrompt: `${SYSTEM_PROMPT}

## Runtime Context (fresh snapshot for this cycle)
${JSON.stringify(runtimeContext, null, 2)}`,
        mcpServers: {
          espn: espnServer,
          polymarket: polymarketServer,
          diagnostics: diagnosticsServer,
          "ploy-backend": ployBackendServer,
          research: researchServer,
        },
        allowedTools: [
          "mcp__espn__*",
          "mcp__polymarket__*",
          "mcp__ploy-backend__get_system_status",
          "mcp__ploy-backend__get_trading_state",
          "mcp__ploy-backend__list_deployments",
          "mcp__ploy-backend__get_deployment",
          "mcp__diagnostics__*",
          "mcp__research__*",
          "WebSearch",
          "WebFetch",
        ],
        maxTurns: 30,
        maxBudgetUsd: MAX_BUDGET,
        permissionMode: "bypassPermissions",
        outputFormat: {
          type: "json_schema",
          schema: researchOutputSchema,
        },
      },
    })) {
      switch (message.type) {
        case "system":
          if (message.subtype === "init") {
            sessionId = message.session_id;
            console.log(`  Session: ${message.session_id}`);
            const mcpStatus = (message as any).mcp_servers;
            if (mcpStatus) {
              for (const s of mcpStatus) {
                console.log(`  MCP ${s.name}: ${s.status}`);
              }
            }
          }
          break;

        case "assistant":
          for (const block of message.message.content) {
            if ("name" in block) {
              console.log(`  Tool: ${block.name}`);
              toolCalls.push({ name: block.name, status: "called" });
            }
          }
          break;

        case "result":
          if (message.subtype === "success") {
            resultOutput = (message as any).structured_output;
            totalCostUsd = (message as any).total_cost_usd || 0;
            console.log(`  Completed. Cost: $${(totalCostUsd ?? 0).toFixed(4)}`);
          } else {
            failureReason = `query_result:${message.subtype}`;
            console.error(`  Scan failed: ${message.subtype}`);
          }
          break;
      }
    }

    if (resultOutput) {
      const output = resultOutput as {
        summary?: {
          platform_status?: string;
          deployments_reviewed?: number;
          research_tasks?: number;
        };
        research_reports?: Array<{ kind: string; subject: string; status: string }>;
        oversight_alerts?: Array<{ severity: string; kind: string; deployment_id?: string }>;
        operator_recommendations?: Array<{ kind: string; target: string }>;
      };

      console.log(`\n  Summary:`);
      console.log(`    Platform: ${output.summary?.platform_status || "unknown"}`);
      console.log(`    Deployments reviewed: ${output.summary?.deployments_reviewed || 0}`);
      console.log(`    Research tasks: ${output.summary?.research_tasks || 0}`);
      console.log(`    Deterministic signals: ${runtimeContext.oversight_signals.length}`);
      console.log(`    Playbook actions: ${runtimeContext.oversight_playbook.length}`);
      console.log(`    Diagnostic candidates: ${runtimeContext.diagnostic_candidates.length}`);
      console.log(`    Alerts: ${output.oversight_alerts?.length || 0}`);
      console.log(`    Recommendations: ${output.operator_recommendations?.length || 0}`);

      for (const signal of runtimeContext.oversight_signals) {
        console.log(
          `    = Signal: ${signal.severity} ${signal.kind} ${signal.deployment_id || "global"}`
        );
      }

      for (const action of runtimeContext.oversight_playbook) {
        console.log(`    = Playbook: ${action.kind} ${action.target}`);
        console.log(`      command: ${action.operator_command}`);
        if (action.config_hint) {
          console.log(`      config: ${action.config_hint}`);
        }
      }

      for (const deploymentId of runtimeContext.diagnostic_candidates) {
        console.log(`    = Diagnose candidate: ${deploymentId}`);
      }

      for (const report of output.research_reports || []) {
        console.log(`    -> Research: ${report.kind} ${report.subject} - ${report.status}`);
      }

      for (const alert of output.oversight_alerts || []) {
        console.log(`    ! Alert: ${alert.severity} ${alert.kind} ${alert.deployment_id || "global"}`);
      }

      for (const recommendation of output.operator_recommendations || []) {
        console.log(`    * Recommendation: ${recommendation.kind} ${recommendation.target}`);
      }
    }
  } catch (err) {
    failureReason = err instanceof Error ? err.message : String(err);
    console.error(`  Error in scan cycle:`, err);
  } finally {
    const finishedAt = new Date().toISOString();
    const record = buildRunRecord({
      runId,
      startedAt,
      finishedAt,
      sessionId,
      model: MODEL,
      runtimeContext,
      toolCalls,
      structuredOutput: (typeof resultOutput === "object" && resultOutput !== null
        ? (resultOutput as {
            research_reports?: Array<unknown>;
            oversight_alerts?: Array<unknown>;
            operator_recommendations?: Array<unknown>;
          })
        : null),
      totalCostUsd,
      failureReason,
    });

    try {
      await recordAgentRun(record);
      console.log(`  Trace saved: run/sidecar/agent-runs.jsonl`);
    } catch (recordErr) {
      console.error("  Failed to persist run trace:", recordErr);
    }
  }
}

// -- Entry Point ----------------------------------------------------

async function main() {
  console.log("╔════════════════════════════════════════╗");
  console.log("║  Ploy Sidecar — Operator Client        ║");
  console.log("║  Research + Oversight Console          ║");
  console.log("╚════════════════════════════════════════╝");
  console.log(`  Model: ${MODEL}`);
  console.log(`  Dry run: ${DRY_RUN}`);
  console.log(`  Poll interval: ${POLL_INTERVAL / 1000}s`);
  console.log(`  Max budget/cycle: $${MAX_BUDGET}`);
  if (minimaxCompatModel) {
    console.log(`  MiniMax compat: enabled (alias -> ${minimaxCompatModel})`);
  }
  console.log("");

  let scanInProgress = false;
  let immediateRescanRequested = false;

  async function guardedScan() {
    if (scanInProgress) {
      immediateRescanRequested = true;
      return;
    }
    scanInProgress = true;
    try {
      await runScanCycle();
    } finally {
      scanInProgress = false;
      if (immediateRescanRequested) {
        immediateRescanRequested = false;
        setTimeout(() => guardedScan(), 5_000);
      }
    }
  }

  await guardedScan();
  setInterval(guardedScan, POLL_INTERVAL);

  // SSE listener for real-time critical signal detection
  startSSEListener(() => guardedScan());
}

function startSSEListener(onCriticalSignal: () => void): void {
  const sseEnabled = process.env.SIDECAR_SSE_ENABLED !== "false";
  if (!sseEnabled) return;

  const sseUrl = `${PLOY_API}/api/events/stream`;
  const sidecarToken = process.env.PLOY_SIDECAR_AUTH_TOKEN;

  function connect() {
    console.log("  SSE: connecting to event stream...");
    const headers: Record<string, string> = {};
    if (sidecarToken) {
      headers["Authorization"] = `Bearer ${sidecarToken}`;
    }

    fetch(sseUrl, { headers })
      .then(async (response) => {
        if (!response.ok || !response.body) {
          console.error(`  SSE: connection failed (${response.status}), retrying in 30s`);
          setTimeout(connect, 30_000);
          return;
        }
        console.log("  SSE: connected");
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        let buffer = "";

        // eslint-disable-next-line no-constant-condition
        while (true) {
          const { done, value } = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, { stream: true });

          const lines = buffer.split("\n");
          buffer = lines.pop() || "";

          for (const line of lines) {
            if (!line.startsWith("data: ")) continue;
            try {
              const event = JSON.parse(line.slice(6));
              const signals: OversightSignal[] = event.oversight?.signals || event.signals || [];
              if (signals.some((s) => s.severity === "critical")) {
                console.log("  SSE: critical signal detected, triggering immediate scan");
                onCriticalSignal();
              }
            } catch {
              // ignore parse errors (keep-alive comments, etc.)
            }
          }
        }
        console.log("  SSE: disconnected, reconnecting in 5s");
        setTimeout(connect, 5_000);
      })
      .catch((err: Error) => {
        console.error(`  SSE: error: ${err.message}, retrying in 30s`);
        setTimeout(connect, 30_000);
      });
  }

  connect();
}

main().catch((err) => {
  console.error("Fatal error:", err);
  process.exit(1);
});
