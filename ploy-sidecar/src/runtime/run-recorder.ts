import { appendFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";

import { agentRunsLogPath } from "./session-store.js";
import { evaluateRun, type RunEvaluation } from "./evaluator.js";

type RuntimeContext = {
  system: { status: string } | null;
  deployments:
    | {
        total: number;
        sample?: Array<{ deployment_id: string }>;
      }
    | null;
  oversight_signals: Array<unknown>;
  oversight_playbook: Array<unknown>;
  diagnostic_candidates?: string[];
};

type StructuredOutput = {
  research_reports?: Array<unknown>;
  oversight_alerts?: Array<unknown>;
  operator_recommendations?: Array<unknown>;
};

export type AgentToolCallRecord = {
  name: string;
  status: string;
};

export type AgentRunStatus = "started" | "succeeded" | "failed";

export type AgentRunRecord = {
  run_id: string;
  cycle_kind: string;
  status: AgentRunStatus;
  started_at: string;
  finished_at: string | null;
  session_id: string | null;
  model: string;
  platform_status: string | null;
  deployment_count: number;
  oversight_signal_count: number;
  oversight_playbook_count: number;
  total_cost_usd: number | null;
  tool_calls: AgentToolCallRecord[];
  research_reports: number;
  oversight_alerts: number;
  operator_recommendations: number;
  failure_reason: string | null;
  runtime_context: {
    deployment_sample: string[];
    oversight_signal_summary: string[];
    oversight_playbook_summary: string[];
    diagnostic_candidates: string[];
  } | null;
  output_summary: {
    research_report_summaries: string[];
    oversight_alert_summaries: string[];
    operator_recommendation_summaries: string[];
  } | null;
  evaluation: RunEvaluation | null;
};

export function newRunId(): string {
  return randomUUID();
}

export async function recordAgentRun(record: AgentRunRecord): Promise<void> {
  const logPath = await agentRunsLogPath();
  await appendFile(logPath, `${JSON.stringify(record)}\n`, "utf8");
}

export function buildRunRecord(params: {
  runId: string;
  startedAt: string;
  finishedAt: string;
  sessionId: string | null;
  model: string;
  runtimeContext: RuntimeContext;
  toolCalls: AgentToolCallRecord[];
  structuredOutput: StructuredOutput | null;
  totalCostUsd: number | null;
  failureReason: string | null;
}): AgentRunRecord {
  const evaluation = params.structuredOutput ? evaluateRun(params.structuredOutput) : null;

  return {
    run_id: params.runId,
    cycle_kind: "research_oversight",
    status: params.failureReason ? "failed" : "succeeded",
    started_at: params.startedAt,
    finished_at: params.finishedAt,
    session_id: params.sessionId,
    model: params.model,
    platform_status: params.runtimeContext.system?.status ?? null,
    deployment_count: params.runtimeContext.deployments?.total ?? 0,
    oversight_signal_count: params.runtimeContext.oversight_signals.length,
    oversight_playbook_count: params.runtimeContext.oversight_playbook.length,
    total_cost_usd: params.totalCostUsd,
    tool_calls: params.toolCalls,
    research_reports: evaluation?.research_reports ?? 0,
    oversight_alerts: evaluation?.oversight_alerts ?? 0,
    operator_recommendations: evaluation?.operator_recommendations ?? 0,
    failure_reason: params.failureReason,
    runtime_context: {
      deployment_sample: params.runtimeContext.deployments?.total
        ? summarizeDeploymentSample(params.runtimeContext)
        : [],
      oversight_signal_summary: summarizeOversightSignals(params.runtimeContext),
      oversight_playbook_summary: summarizeOversightPlaybook(params.runtimeContext),
      diagnostic_candidates: summarizeDiagnosticCandidates(params.runtimeContext),
    },
    output_summary: params.structuredOutput
      ? {
          research_report_summaries: summarizeResearchReports(params.structuredOutput),
          oversight_alert_summaries: summarizeOversightAlerts(params.structuredOutput),
          operator_recommendation_summaries: summarizeRecommendations(params.structuredOutput),
        }
      : null,
    evaluation,
  };
}

function summarizeDeploymentSample(runtimeContext: RuntimeContext): string[] {
  const sample = (runtimeContext as RuntimeContext & {
    deployments?: { sample?: Array<{ deployment_id?: string }> };
  }).deployments?.sample;
  return Array.isArray(sample)
    ? sample
        .map((item) => item.deployment_id)
        .filter((item): item is string => Boolean(item))
        .slice(0, 8)
    : [];
}

function summarizeOversightSignals(runtimeContext: RuntimeContext): string[] {
  return runtimeContext.oversight_signals
    .map((signal) => {
      const candidate = signal as {
        severity?: string;
        kind?: string;
        deployment_id?: string;
      };
      return [
        candidate.severity ?? "unknown",
        candidate.kind ?? "unknown",
        candidate.deployment_id ?? "platform",
      ].join(":");
    })
    .slice(0, 12);
}

function summarizeOversightPlaybook(runtimeContext: RuntimeContext): string[] {
  return runtimeContext.oversight_playbook
    .map((action) => {
      const candidate = action as { kind?: string; target?: string };
      return [candidate.kind ?? "unknown", candidate.target ?? "platform"].join(":");
    })
    .slice(0, 12);
}

function summarizeDiagnosticCandidates(runtimeContext: RuntimeContext): string[] {
  const candidates = (runtimeContext as RuntimeContext & {
    diagnostic_candidates?: string[];
  }).diagnostic_candidates;
  return Array.isArray(candidates) ? candidates.slice(0, 8) : [];
}

function summarizeResearchReports(output: StructuredOutput): string[] {
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

function summarizeOversightAlerts(output: StructuredOutput): string[] {
  return (output.oversight_alerts ?? [])
    .map((alert) => {
      const candidate = alert as {
        severity?: string;
        kind?: string;
        deployment_id?: string;
      };
      return [
        candidate.severity ?? "unknown",
        candidate.kind ?? "unknown",
        candidate.deployment_id ?? "platform",
      ].join(":");
    })
    .slice(0, 12);
}

function summarizeRecommendations(output: StructuredOutput): string[] {
  return (output.operator_recommendations ?? [])
    .map((recommendation) => {
      const candidate = recommendation as { kind?: string; target?: string };
      return [candidate.kind ?? "unknown", candidate.target ?? "platform"].join(":");
    })
    .slice(0, 12);
}
