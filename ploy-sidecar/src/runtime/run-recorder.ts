import { appendFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import type {
  AgentRunRecord,
  AgentToolCallRecord,
  JsonValue,
} from "../contracts/operator-contracts.js";
import { agentRunsLogPath } from "./session-store.js";
import { evaluateRun } from "./evaluator.js";

type RuntimeContext = {
  system: { status: string } | null;
  deployments: {
    total: number;
    sample?: Array<{ deployment_id: string }>;
  } | null;
  oversight_signals?: Array<unknown>;
  oversight_playbook?: Array<unknown>;
  diagnostic_candidates?: string[];
};

type StructuredOutput = {
  research_reports?: Array<unknown>;
  oversight_alerts?: Array<unknown>;
  operator_recommendations?: Array<unknown>;
};

export type AgentTaskCompletion = {
  status: "success" | "partial" | "blocked";
  summary: string;
};

export function newRunId() {
  return randomUUID();
}

export async function recordAgentRun(record: AgentRunRecord): Promise<void> {
  const logPath = await agentRunsLogPath();
  await appendFile(logPath, `${JSON.stringify(record)}\n`, "utf8");
}

export function buildRunRecord(params: {
  runId: string;
  cycleKind: string;
  startedAt: string;
  finishedAt: string | null;
  sessionId: string | null;
  model: string;
  runtimeContext: RuntimeContext;
  toolCalls: AgentToolCallRecord[];
  structuredOutput: StructuredOutput | null;
  totalCostUsd: number | null;
  failureReason: string | null;
  completion: AgentTaskCompletion | null;
  request?: JsonValue;
}): AgentRunRecord {
  const evaluation = params.structuredOutput ? evaluateRun(params.structuredOutput) : null;
  return {
    run_id: params.runId,
    cycle_kind: params.cycleKind,
    status: runStatus(params),
    started_at: params.startedAt,
    finished_at: params.finishedAt,
    session_id: params.sessionId,
    model: params.model,
    platform_status: params.runtimeContext.system?.status ?? null,
    deployment_count: params.runtimeContext.deployments?.total ?? 0,
    oversight_signal_count: params.runtimeContext.oversight_signals?.length ?? 0,
    oversight_playbook_count: params.runtimeContext.oversight_playbook?.length ?? 0,
    total_cost_usd: params.totalCostUsd,
    tool_calls: params.toolCalls,
    research_reports: evaluation?.research_reports ?? 0,
    oversight_alerts: evaluation?.oversight_alerts ?? 0,
    operator_recommendations: evaluation?.operator_recommendations ?? 0,
    failure_reason: params.failureReason,
    runtime_context: {
      deployment_sample: summarizeDeploymentSample(params.runtimeContext),
      oversight_signal_summary: summarizeOversightSignals(params.runtimeContext),
      oversight_playbook_summary: summarizeOversightPlaybook(params.runtimeContext),
      diagnostic_candidates: summarizeDiagnosticCandidates(params.runtimeContext),
      request: params.request ?? null,
    },
    output_summary: params.structuredOutput || params.completion
      ? {
          task_completion: params.completion,
          research_report_summaries: params.structuredOutput
            ? summarizeResearchReports(params.structuredOutput)
            : [],
          oversight_alert_summaries: params.structuredOutput
            ? summarizeOversightAlerts(params.structuredOutput)
            : [],
          operator_recommendation_summaries: params.structuredOutput
            ? summarizeRecommendations(params.structuredOutput)
            : [],
        }
      : null,
    evaluation,
  };
}

function runStatus(params: {
  failureReason: string | null;
  completion: AgentTaskCompletion | null;
  finishedAt: string | null;
}) {
  if (params.failureReason) return "failed";
  if (params.completion?.status === "blocked") return "blocked";
  if (params.completion?.status === "partial") return "partial";
  return params.finishedAt ? "succeeded" : "started";
}

function summarizeDeploymentSample(runtimeContext: RuntimeContext) {
  const sample = runtimeContext.deployments?.sample;
  return Array.isArray(sample)
    ? sample
        .map((item) => item.deployment_id)
        .filter((item) => Boolean(item))
        .slice(0, 8)
    : [];
}

function summarizeOversightSignals(runtimeContext: RuntimeContext) {
  return (runtimeContext.oversight_signals ?? [])
    .map((signal) => {
      const candidate = signal as { severity?: string; kind?: string; deployment_id?: string };
      return [
        candidate.severity ?? "unknown",
        candidate.kind ?? "unknown",
        candidate.deployment_id ?? "platform",
      ].join(":");
    })
    .slice(0, 12);
}

function summarizeOversightPlaybook(runtimeContext: RuntimeContext) {
  return (runtimeContext.oversight_playbook ?? [])
    .map((action) => {
      const candidate = action as { kind?: string; target?: string };
      return [candidate.kind ?? "unknown", candidate.target ?? "platform"].join(":");
    })
    .slice(0, 12);
}

function summarizeDiagnosticCandidates(runtimeContext: RuntimeContext) {
  const candidates = runtimeContext.diagnostic_candidates;
  return Array.isArray(candidates) ? candidates.slice(0, 8) : [];
}

function summarizeResearchReports(output: StructuredOutput) {
  return (output.research_reports ?? [])
    .map((report) => {
      const candidate = report as { kind?: string; subject?: string; status?: string };
      return [
        candidate.kind ?? "unknown",
        candidate.subject ?? "unknown",
        candidate.status ?? "unknown",
      ].join(":");
    })
    .slice(0, 12);
}

function summarizeOversightAlerts(output: StructuredOutput) {
  return (output.oversight_alerts ?? [])
    .map((alert) => {
      const candidate = alert as { severity?: string; kind?: string; deployment_id?: string };
      return [
        candidate.severity ?? "unknown",
        candidate.kind ?? "unknown",
        candidate.deployment_id ?? "platform",
      ].join(":");
    })
    .slice(0, 12);
}

function summarizeRecommendations(output: StructuredOutput) {
  return (output.operator_recommendations ?? [])
    .map((recommendation) => {
      const candidate = recommendation as { kind?: string; target?: string };
      return [candidate.kind ?? "unknown", candidate.target ?? "platform"].join(":");
    })
    .slice(0, 12);
}
