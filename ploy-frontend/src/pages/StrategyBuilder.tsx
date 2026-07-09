import { type ReactNode, useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  Activity,
  AlertTriangle,
  ArrowRight,
  Bot,
  Brain,
  CheckCircle2,
  CircleDot,
  Clipboard,
  Code2,
  Database,
  FileCheck2,
  GitBranch,
  GitPullRequest,
  LockKeyhole,
  PlayCircle,
  RefreshCw,
  ShieldCheck,
  Sparkles,
  TerminalSquare,
  TimerReset,
  Wand2,
  XCircle,
} from 'lucide-react';

import { Badge } from '@/components/ui/Badge';
import { Button } from '@/components/ui/Button';
import {
  autonomyLabels,
  buildAgentRunCreateRequest,
  buildAgentSteps,
  buildAutomationPacket,
  buildResearchIssue,
  buildRunContract,
  capabilityMap,
  compactSymbols,
  dataSurfaces,
  defaultObjective,
  familyLabels,
  gateOrder,
  targetGateIndex,
  targetLabels,
  type AgentStep,
  type ArtifactKey,
  type AutonomyMode,
  type BuilderForm,
  type EvidenceTarget,
  type StepState,
  type StrategyFamily,
  type ToolCapability,
} from '@/lib/agenticStrategyBuilder';
import { api } from '@/services/api';
import type { AgentRunRecord, JsonValue } from '@/types';
import { cn } from '@/lib/utils';

function copyText(value: string) {
  if (!navigator.clipboard) return;
  void navigator.clipboard.writeText(value);
}

function statusClass(state: StepState) {
  if (state === 'active') return 'border-[#2563eb] bg-[#eff6ff] text-[#1d4ed8]';
  if (state === 'ready') return 'border-[#0f9f6e] bg-[#ecfdf5] text-[#047857]';
  if (state === 'blocked') return 'border-[#f43f5e] bg-[#fff1f2] text-[#be123c]';
  return 'border-[#d6b253] bg-[#fffbeb] text-[#92400e]';
}

function statusIcon(state: StepState) {
  if (state === 'active') return <CircleDot className="h-4 w-4" />;
  if (state === 'ready') return <CheckCircle2 className="h-4 w-4" />;
  if (state === 'blocked') return <LockKeyhole className="h-4 w-4" />;
  return <TimerReset className="h-4 w-4" />;
}

function capabilityVariant(status: ToolCapability['status']) {
  if (status === 'wired') return 'success' as const;
  if (status === 'approval') return 'warning' as const;
  return 'secondary' as const;
}

function runStatusVariant(status: string) {
  const normalized = status.toLowerCase();
  if (normalized.includes('success') || normalized.includes('succeeded')) return 'success' as const;
  if (normalized.includes('fail') || normalized.includes('error') || normalized.includes('blocked')) {
    return 'destructive' as const;
  }
  if (
    normalized.includes('retry') ||
    normalized.includes('partial') ||
    normalized.includes('start') ||
    normalized.includes('running')
  ) {
    return 'warning' as const;
  }
  return 'secondary' as const;
}

function contractCheckVariant(status: string) {
  const normalized = status.toLowerCase();
  if (normalized === 'passed') return 'success' as const;
  if (normalized === 'blocked') return 'destructive' as const;
  return 'warning' as const;
}

function formatTime(value?: string | null) {
  if (!value) return '-';
  const time = Date.parse(value);
  if (!Number.isFinite(time)) return '-';
  return new Date(time).toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

function toolCount(run: AgentRunRecord) {
  return run.tool_calls.length;
}

type ContractCheckView = {
  name: string;
  status: string;
  detail: string;
};

type HarnessLearningView = {
  category: string;
  summary: string;
  suggestedChange: string;
  subagentProfile?: string;
};

function contractChecks(run: AgentRunRecord): ContractCheckView[] {
  const outputSummary = asJsonRecord(run.output_summary);
  const evaluation = asJsonRecord(outputSummary?.contract_evaluation);
  const checks = evaluation?.checks;
  if (!Array.isArray(checks)) return [];

  return checks
    .map((check) => {
      const record = asJsonRecord(check);
      if (!record) return null;
      return {
        name: asString(record.name, 'contract'),
        status: asString(record.status, 'unknown'),
        detail: asString(record.detail, ''),
      };
    })
    .filter((check): check is ContractCheckView => check !== null)
    .slice(0, 5);
}

function harnessLearning(run: AgentRunRecord): HarnessLearningView | null {
  const outputSummary = asJsonRecord(run.output_summary);
  const learning = asJsonRecord(outputSummary?.harness_learning);
  if (!learning) return null;
  return {
    category: asString(learning.category, 'harness'),
    summary: asString(learning.summary, ''),
    suggestedChange: asString(learning.suggested_change, ''),
    subagentProfile:
      typeof learning.subagent_profile === 'string' ? learning.subagent_profile : undefined,
  };
}

function asJsonRecord(value: JsonValue | undefined): Record<string, JsonValue> | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  return value;
}

function asString(value: JsonValue | undefined, fallback: string) {
  return typeof value === 'string' ? value : fallback;
}

export function StrategyBuilder() {
  const queryClient = useQueryClient();
  const [form, setForm] = useState<BuilderForm>({
    objective: defaultObjective,
    family: 'pm5d',
    symbols: 'BTCUSDT,ETHUSDT',
    target: 'executable_replay',
    autonomy: 'research_until_blocked',
    budgetUsd: 1,
    maxTurns: 30,
  });
  const [artifact, setArtifact] = useState<ArtifactKey>('packet');

  const createRunMutation = useMutation({
    mutationFn: () => api.createAgentRun(buildAgentRunCreateRequest(form)),
    onSuccess: () => {
      setArtifact('packet');
      void queryClient.invalidateQueries({ queryKey: ['agent', 'runs'] });
    },
  });

  const agentRunsQuery = useQuery({
    queryKey: ['agent', 'runs'],
    queryFn: () => api.getAgentRuns(),
    retry: false,
    refetchInterval: 10000,
    refetchOnWindowFocus: false,
  });

  const agentSteps = useMemo(() => buildAgentSteps(form), [form]);
  const automationPacket = useMemo(() => buildAutomationPacket(form), [form]);
  const researchIssue = useMemo(() => buildResearchIssue(form), [form]);
  const runContract = useMemo(() => buildRunContract(form), [form]);
  const evidenceIndex = targetGateIndex(form.target);
  const recentRuns = (agentRunsQuery.data ?? []).slice(0, 4);

  const artifactValue =
    artifact === 'packet' ? automationPacket : artifact === 'issue' ? researchIssue : runContract;
  const canCreateRun = form.objective.trim().length > 0 && compactSymbols(form.symbols).length > 0;

  return (
    <div className="min-h-full bg-[#f4f7f2] text-[#111827]">
      <header className="border-b border-[#dbe5dc] bg-[#fbfdf9] px-5 py-4">
        <div className="flex flex-col gap-4 2xl:flex-row 2xl:items-center 2xl:justify-between">
          <div className="flex items-center gap-4">
            <div className="flex h-12 w-12 items-center justify-center rounded-md border border-[#b8d8c7] bg-[#e7f6ee] text-[#047857]">
              <Bot className="h-6 w-6" />
            </div>
            <div>
              <div className="text-xs font-semibold text-[#64748b]">Agentic Strategy OS</div>
              <h1 className="mt-1 text-2xl font-semibold tracking-normal">自主策略运行台</h1>
            </div>
          </div>
          <div className="grid gap-2 sm:grid-cols-4">
            <Metric label="自治模式" value={autonomyLabels[form.autonomy]} tone="green" />
            <Metric label="目标证据" value={targetLabels[form.target]} tone="blue" />
            <Metric label="Agent runs" value={`${agentRunsQuery.data?.length ?? 0}`} tone="ink" />
            <Metric label="Live" value="locked" tone="red" />
          </div>
        </div>
      </header>

      <main className="agentic-builder-grid p-5">
        <section className="min-w-0 space-y-5">
          <Panel
            title="Outcome command"
            icon={<Wand2 className="h-4 w-4" />}
            aside={<Badge variant="outline">one prompt</Badge>}
          >
            <div className="space-y-4">
              <Field label="目标" htmlFor="agentic-objective">
                <textarea
                  id="agentic-objective"
                  name="agentic_objective"
                  value={form.objective}
                  onChange={(event) => setForm({ ...form, objective: event.target.value })}
                  className="min-h-[190px] w-full resize-y rounded-md border border-[#cbd5d1] bg-white px-3 py-3 text-sm leading-6 outline-none focus:border-[#047857] focus:ring-2 focus:ring-[#bbf7d0]"
                />
              </Field>

              <div className="grid gap-3">
                <Field label="自治边界" htmlFor="agentic-autonomy">
                  <select
                    id="agentic-autonomy"
                    name="agentic_autonomy"
                    value={form.autonomy}
                    onChange={(event) =>
                      setForm({ ...form, autonomy: event.target.value as AutonomyMode })
                    }
                    className="h-10 w-full rounded-md border border-[#cbd5d1] bg-white px-3 text-sm outline-none focus:border-[#047857] focus:ring-2 focus:ring-[#bbf7d0]"
                  >
                    {Object.entries(autonomyLabels).map(([value, label]) => (
                      <option key={value} value={value}>
                        {label}
                      </option>
                    ))}
                  </select>
                </Field>

                <Field label="证据目标" htmlFor="agentic-target">
                  <select
                    id="agentic-target"
                    name="agentic_target"
                    value={form.target}
                    onChange={(event) =>
                      setForm({ ...form, target: event.target.value as EvidenceTarget })
                    }
                    className="h-10 w-full rounded-md border border-[#cbd5d1] bg-white px-3 text-sm outline-none focus:border-[#047857] focus:ring-2 focus:ring-[#bbf7d0]"
                  >
                    {Object.entries(targetLabels).map(([value, label]) => (
                      <option key={value} value={value}>
                        {label}
                      </option>
                    ))}
                  </select>
                </Field>
              </div>

              <div className="grid gap-3 sm:grid-cols-2">
                <Field label="策略族" htmlFor="agentic-family">
                  <select
                    id="agentic-family"
                    name="agentic_family"
                    value={form.family}
                    onChange={(event) =>
                      setForm({ ...form, family: event.target.value as StrategyFamily })
                    }
                    className="h-10 w-full rounded-md border border-[#cbd5d1] bg-white px-3 text-sm outline-none focus:border-[#047857] focus:ring-2 focus:ring-[#bbf7d0]"
                  >
                    {Object.entries(familyLabels).map(([value, label]) => (
                      <option key={value} value={value}>
                        {label}
                      </option>
                    ))}
                  </select>
                </Field>
                <Field label="标的" htmlFor="agentic-symbols">
                  <input
                    id="agentic-symbols"
                    name="agentic_symbols"
                    value={form.symbols}
                    onChange={(event) => setForm({ ...form, symbols: event.target.value })}
                    className="h-10 w-full rounded-md border border-[#cbd5d1] bg-white px-3 text-sm outline-none focus:border-[#047857] focus:ring-2 focus:ring-[#bbf7d0]"
                  />
                </Field>
              </div>

              <div className="grid grid-cols-2 gap-3">
                <NumberField
                  id="agentic-budget"
                  label="Budget"
                  suffix="USD"
                  value={form.budgetUsd}
                  min={0.1}
                  step={0.1}
                  onChange={(value) => setForm({ ...form, budgetUsd: value })}
                />
                <NumberField
                  id="agentic-turns"
                  label="Turns"
                  suffix="max"
                  value={form.maxTurns}
                  min={1}
                  step={1}
                  onChange={(value) => setForm({ ...form, maxTurns: value })}
                />
              </div>

              <Button
                type="button"
                className="h-10 w-full gap-2 bg-[#111827] text-white hover:bg-[#1f2937]"
                disabled={!canCreateRun || createRunMutation.isPending}
                onClick={() => createRunMutation.mutate()}
              >
                {createRunMutation.isPending ? (
                  <RefreshCw className="h-4 w-4 animate-spin" />
                ) : (
                  <Sparkles className="h-4 w-4" />
                )}
                {createRunMutation.isPending ? '提交 agent run' : '启动自动 agent run'}
              </Button>
              {createRunMutation.isError && (
                <div className="rounded-md border border-[#fecdd3] bg-[#fff1f2] px-3 py-2 text-xs text-[#be123c]">
                  {createRunMutation.error instanceof Error
                    ? createRunMutation.error.message
                    : 'agent run request failed'}
                </div>
              )}
              {createRunMutation.isSuccess && (
                <div className="rounded-md border border-[#b8d8c7] bg-[#e7f6ee] px-3 py-2 text-xs text-[#047857]">
                  queued {createRunMutation.data.run_id}
                </div>
              )}
            </div>
          </Panel>

          <Panel title="Action parity" icon={<GitBranch className="h-4 w-4" />}>
            <div className="space-y-2">
              {capabilityMap.map((item) => (
                <div
                  key={item.action}
                  className="grid grid-cols-[1fr_auto] gap-2 rounded-md border border-[#d9e3dd] bg-[#fbfdf9] px-3 py-2"
                >
                  <div className="min-w-0">
                    <div className="truncate text-sm font-medium">{item.action}</div>
                    <div className="mt-1 truncate text-xs text-[#64748b]">{item.tool}</div>
                  </div>
                  <Badge variant={capabilityVariant(item.status)}>{item.status}</Badge>
                </div>
              ))}
            </div>
          </Panel>
        </section>

        <section className="min-w-0 space-y-5">
          <Panel
            title="Autonomous loop"
            icon={<Brain className="h-4 w-4" />}
            aside={<Badge variant="warning">completion required</Badge>}
          >
            <div className="grid gap-3">
              {agentSteps.map((step, index) => (
                <AgentStepRow
                  key={step.id}
                  step={step}
                  terminal={index === agentSteps.length - 1}
                />
              ))}
            </div>
          </Panel>

          <div className="grid gap-5 xl:grid-cols-[1fr_300px]">
            <Panel title="Evidence gates" icon={<ShieldCheck className="h-4 w-4" />}>
              <div className="space-y-3">
                {gateOrder.map((gate, index) => {
                  const done = index <= evidenceIndex;
                  const hardBlocked = index >= 5;
                  return (
                    <div key={gate} className="flex items-center gap-3">
                      <div
                        className={cn(
                          'flex h-7 w-7 shrink-0 items-center justify-center rounded-md border text-xs font-semibold',
                          done
                            ? 'border-[#047857] bg-[#dcfce7] text-[#047857]'
                            : hardBlocked
                              ? 'border-[#f43f5e] bg-[#fff1f2] text-[#be123c]'
                              : 'border-[#d6b253] bg-[#fffbeb] text-[#92400e]'
                        )}
                      >
                        {done ? <CheckCircle2 className="h-4 w-4" /> : hardBlocked ? <LockKeyhole className="h-4 w-4" /> : index + 1}
                      </div>
                      <div className="min-w-0 flex-1">
                        <div className="text-sm font-medium">{gate}</div>
                        <div className="mt-1 h-1.5 overflow-hidden rounded-full bg-[#e5ece8]">
                          <div
                            className={cn(
                              'h-full rounded-full',
                              done ? 'bg-[#047857]' : hardBlocked ? 'bg-[#fb7185]' : 'bg-[#d6b253]'
                            )}
                            style={{ width: done ? '100%' : hardBlocked ? '18%' : '46%' }}
                          />
                        </div>
                      </div>
                    </div>
                  );
                })}
              </div>
            </Panel>

            <Panel title="Tool queue" icon={<TerminalSquare className="h-4 w-4" />}>
              <div className="space-y-2">
                {[
                  ['read', 'platform + context'],
                  ['scan', 'data surfaces'],
                  ['run', 'research workflow'],
                  ['verify', 'replay + parity'],
                  ['complete', 'complete_task'],
                ].map(([verb, object]) => (
                  <div
                    key={verb}
                    className="flex items-center justify-between rounded-md border border-[#d9e3dd] bg-white px-3 py-2"
                  >
                    <span className="text-sm font-semibold">{verb}</span>
                    <span className="text-xs text-[#64748b]">{object}</span>
                  </div>
                ))}
              </div>
            </Panel>
          </div>

          <Panel
            title="Artifacts"
            icon={<FileCheck2 className="h-4 w-4" />}
            aside={
              <Button
                type="button"
                size="sm"
                variant="outline"
                className="h-8 gap-2 whitespace-nowrap"
                onClick={() => copyText(artifactValue)}
              >
                <Clipboard className="h-4 w-4" />
                Copy
              </Button>
            }
          >
            <div className="mb-3 grid grid-cols-3 gap-2">
              <ArtifactTab
                active={artifact === 'packet'}
                label="Run packet"
                icon={<Activity className="h-4 w-4" />}
                onClick={() => setArtifact('packet')}
              />
              <ArtifactTab
                active={artifact === 'issue'}
                label="Issue"
                icon={<GitPullRequest className="h-4 w-4" />}
                onClick={() => setArtifact('issue')}
              />
              <ArtifactTab
                active={artifact === 'contract'}
                label="Contract"
                icon={<Code2 className="h-4 w-4" />}
                onClick={() => setArtifact('contract')}
              />
            </div>
            <pre className="max-h-[420px] overflow-auto rounded-md bg-[#101820] p-4 text-xs leading-5 text-[#f8fafc]">
              {artifactValue}
            </pre>
          </Panel>
        </section>

        <aside className="min-w-0 space-y-5">
          <Panel
            title="Run monitor"
            icon={<RefreshCw className="h-4 w-4" />}
            aside={
              agentRunsQuery.isError ? (
                <Badge variant="destructive">offline</Badge>
              ) : (
                <Badge variant="success">read-only</Badge>
              )
            }
          >
            {agentRunsQuery.isLoading ? (
              <EmptyState icon={<RefreshCw className="h-5 w-5" />} title="Loading runs" />
            ) : recentRuns.length === 0 ? (
              <EmptyState icon={<Bot className="h-5 w-5" />} title="No sidecar runs" />
            ) : (
              <div className="space-y-3">
                {recentRuns.map((run) => (
                  <div key={run.run_id} className="rounded-md border border-[#d9e3dd] bg-white p-3">
                    <div className="flex items-center justify-between gap-2">
                      <div className="min-w-0 truncate text-sm font-semibold">{run.cycle_kind}</div>
                      <Badge variant={runStatusVariant(run.status)}>{run.status}</Badge>
                    </div>
                    <div className="mt-2 grid grid-cols-2 gap-2 text-xs text-[#64748b]">
                      <span>{formatTime(run.finished_at || run.started_at)}</span>
                      <span className="text-right">{toolCount(run)} tools</span>
                      <span>{run.model}</span>
                      <span className="text-right">
                        {run.total_cost_usd == null ? 'cost pending' : `$${run.total_cost_usd.toFixed(4)}`}
                      </span>
                    </div>
                    {run.failure_reason && (
                      <div className="mt-2 rounded-md bg-[#fff1f2] px-2 py-1 text-xs text-[#be123c]">
                        {run.failure_reason}
                      </div>
                    )}
                    {contractChecks(run).length > 0 && (
                      <div className="mt-3 space-y-2">
                        {contractChecks(run).map((check) => (
                          <div
                            key={`${run.run_id}-${check.name}`}
                            className="rounded-md border border-[#e5ece8] bg-[#fbfdf9] px-2 py-2"
                          >
                            <div className="flex items-center justify-between gap-2">
                              <span className="min-w-0 truncate text-xs font-semibold">
                                {check.name}
                              </span>
                              <Badge variant={contractCheckVariant(check.status)}>
                                {check.status}
                              </Badge>
                            </div>
                            {check.detail && (
                              <div className="mt-1 line-clamp-2 text-xs leading-5 text-[#64748b]">
                                {check.detail}
                              </div>
                            )}
                          </div>
                        ))}
                      </div>
                    )}
                    {harnessLearning(run) && (
                      <div className="mt-3 rounded-md border border-[#bfdbfe] bg-[#eff6ff] px-2 py-2">
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-xs font-semibold text-[#1d4ed8]">
                            harness: {harnessLearning(run)?.category}
                          </span>
                          {harnessLearning(run)?.subagentProfile && (
                            <Badge variant="outline">{harnessLearning(run)?.subagentProfile}</Badge>
                          )}
                        </div>
                        <div className="mt-1 line-clamp-2 text-xs leading-5 text-[#1e3a8a]">
                          {harnessLearning(run)?.suggestedChange || harnessLearning(run)?.summary}
                        </div>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
          </Panel>

          <Panel title="Control boundary" icon={<LockKeyhole className="h-4 w-4" />}>
            <div className="space-y-3">
              <BoundaryRow
                icon={<PlayCircle className="h-4 w-4" />}
                title="Auto"
                detail="research, replay, diagnostics"
                tone="green"
              />
              <BoundaryRow
                icon={<AlertTriangle className="h-4 w-4" />}
                title="Confirm"
                detail="paper intent / dry-run handoff"
                tone="amber"
              />
              <BoundaryRow
                icon={<XCircle className="h-4 w-4" />}
                title="Locked"
                detail="live deployment and real orders"
                tone="red"
              />
            </div>
          </Panel>

          <Panel title="Shared workspace" icon={<Database className="h-4 w-4" />}>
            <div className="space-y-2 text-sm">
              {dataSurfaces[form.family].map((surface) => (
                <div
                  key={surface}
                  className="flex items-center gap-2 rounded-md border border-[#d9e3dd] bg-white px-3 py-2"
                >
                  <CheckCircle2 className="h-4 w-4 shrink-0 text-[#047857]" />
                  <span className="min-w-0 truncate">{surface}</span>
                </div>
              ))}
            </div>
          </Panel>
        </aside>
      </main>
    </div>
  );
}

function Metric({
  label,
  value,
  tone,
}: {
  label: string;
  value: string;
  tone: 'green' | 'blue' | 'ink' | 'red';
}) {
  const toneClasses = {
    green: 'border-[#b8d8c7] bg-[#e7f6ee] text-[#047857]',
    blue: 'border-[#bfdbfe] bg-[#eff6ff] text-[#1d4ed8]',
    ink: 'border-[#d9e3dd] bg-white text-[#111827]',
    red: 'border-[#fecdd3] bg-[#fff1f2] text-[#be123c]',
  };

  return (
    <div className={cn('rounded-md border px-3 py-2 text-right', toneClasses[tone])}>
      <div className="truncate text-sm font-semibold leading-none">{value}</div>
      <div className="mt-1 text-[11px] text-current opacity-70">{label}</div>
    </div>
  );
}

function Panel({
  title,
  icon,
  aside,
  children,
}: {
  title: string;
  icon: ReactNode;
  aside?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="rounded-md border border-[#d9e3dd] bg-[#fbfdf9] shadow-sm">
      <div className="flex min-h-[52px] items-center justify-between gap-3 border-b border-[#d9e3dd] px-4 py-3">
        <h2 className="flex min-w-0 items-center gap-2 text-sm font-semibold text-[#111827]">
          <span className="text-[#047857]">{icon}</span>
          <span className="truncate">{title}</span>
        </h2>
        {aside}
      </div>
      <div className="p-4">{children}</div>
    </section>
  );
}

function Field({ label, htmlFor, children }: { label: string; htmlFor: string; children: ReactNode }) {
  return (
    <div>
      <label htmlFor={htmlFor} className="mb-2 block text-xs font-semibold text-[#475569]">
        {label}
      </label>
      {children}
    </div>
  );
}

function NumberField({
  id,
  label,
  suffix,
  value,
  min,
  step,
  onChange,
}: {
  id: string;
  label: string;
  suffix: string;
  value: number;
  min: number;
  step: number;
  onChange: (value: number) => void;
}) {
  return (
    <Field label={label} htmlFor={id}>
      <div className="flex h-10 overflow-hidden rounded-md border border-[#cbd5d1] bg-white focus-within:border-[#047857] focus-within:ring-2 focus-within:ring-[#bbf7d0]">
        <input
          id={id}
          name={id.replace(/-/g, '_')}
          type="number"
          value={value}
          min={min}
          step={step}
          onChange={(event) => onChange(Number(event.target.value))}
          className="min-w-0 flex-1 bg-transparent px-3 text-sm outline-none"
        />
        <span className="flex items-center border-l border-[#d9e3dd] bg-[#f3f7f2] px-2 text-xs text-[#64748b]">
          {suffix}
        </span>
      </div>
    </Field>
  );
}

function AgentStepRow({ step, terminal }: { step: AgentStep; terminal: boolean }) {
  return (
    <div className="grid grid-cols-[minmax(0,1fr)_32px] items-stretch gap-3">
      <div className={cn('rounded-md border px-4 py-3', statusClass(step.state))}>
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <div className="flex items-center gap-2 text-sm font-semibold">
              {statusIcon(step.state)}
              <span>{step.title}</span>
            </div>
            <div className="mt-1 text-xs opacity-80">{step.owner}</div>
          </div>
          <span className="shrink-0 rounded-md bg-white/70 px-2 py-1 text-xs font-medium">
            {step.state}
          </span>
        </div>
        <div className="mt-3 text-sm">{step.detail}</div>
        <div className="mt-3 flex flex-wrap gap-2">
          {step.tools.map((tool) => (
            <span key={tool} className="rounded-md bg-white/70 px-2 py-1 text-[11px] font-medium">
              {tool}
            </span>
          ))}
        </div>
      </div>
      <div className="flex items-center justify-center text-[#94a3b8]">
        {terminal ? <CheckCircle2 className="h-5 w-5" /> : <ArrowRight className="h-5 w-5" />}
      </div>
    </div>
  );
}

function ArtifactTab({
  active,
  label,
  icon,
  onClick,
}: {
  active: boolean;
  label: string;
  icon: ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'flex h-9 items-center justify-center gap-2 rounded-md border px-2 text-sm font-medium',
        active
          ? 'border-[#111827] bg-[#111827] text-white'
          : 'border-[#d9e3dd] bg-white text-[#475569] hover:bg-[#f3f7f2]'
      )}
    >
      {icon}
      <span className="truncate">{label}</span>
    </button>
  );
}

function BoundaryRow({
  icon,
  title,
  detail,
  tone,
}: {
  icon: ReactNode;
  title: string;
  detail: string;
  tone: 'green' | 'amber' | 'red';
}) {
  const classes = {
    green: 'border-[#b8d8c7] bg-[#e7f6ee] text-[#047857]',
    amber: 'border-[#fde68a] bg-[#fffbeb] text-[#92400e]',
    red: 'border-[#fecdd3] bg-[#fff1f2] text-[#be123c]',
  };
  return (
    <div className={cn('rounded-md border px-3 py-3', classes[tone])}>
      <div className="flex items-center gap-2 text-sm font-semibold">
        {icon}
        {title}
      </div>
      <div className="mt-1 text-xs opacity-80">{detail}</div>
    </div>
  );
}

function EmptyState({ icon, title }: { icon: ReactNode; title: string }) {
  return (
    <div className="flex min-h-[110px] flex-col items-center justify-center rounded-md border border-dashed border-[#cbd5d1] bg-white text-center text-[#64748b]">
      {icon}
      <div className="mt-2 text-sm font-medium">{title}</div>
    </div>
  );
}
