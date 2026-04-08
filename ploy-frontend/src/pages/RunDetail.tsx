import { useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Link, useParams } from 'react-router-dom';
import { api } from '@/services/api';
import { Badge } from '@/components/ui/Badge';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { formatCurrency, formatTimestamp } from '@/lib/utils';

function statusVariant(status: string) {
  switch (status) {
    case 'succeeded':
      return 'success' as const;
    case 'failed':
      return 'destructive' as const;
    default:
      return 'warning' as const;
  }
}

export function RunDetail() {
  const { runId = '' } = useParams();

  const runQuery = useQuery({
    queryKey: ['agent-run', runId],
    queryFn: () => api.getAgentRun(runId),
    enabled: Boolean(runId),
    refetchInterval: 15000,
  });

  const proposalsQuery = useQuery({
    queryKey: ['proposals'],
    queryFn: () => api.getProposals(),
    refetchInterval: 15000,
  });

  const relatedProposals = useMemo(() => {
    const proposals = proposalsQuery.data ?? [];
    if (!runId) return [];
    return proposals.filter((proposal) => proposal.source_run_id === runId);
  }, [proposalsQuery.data, runId]);

  const run = runQuery.data;

  return (
    <div className="space-y-8 p-8">
      <div className="space-y-2">
        <Link to="/research-runs" className="text-sm text-primary hover:underline">
          ← Back to Research Runs
        </Link>
        <div>
          <h1 className="text-3xl font-bold">Run Replay Detail</h1>
          <p className="text-muted-foreground">
            Operator replay view for one sidecar run, including context, outputs, and follow-through.
          </p>
        </div>
      </div>

      {runQuery.isLoading ? (
        <Card>
          <CardContent className="pt-6 text-sm text-muted-foreground">
            Loading run detail...
          </CardContent>
        </Card>
      ) : runQuery.isError || !run ? (
        <Card>
          <CardContent className="pt-6 text-sm text-destructive">
            Unable to load run detail for {runId || 'unknown run'}.
          </CardContent>
        </Card>
      ) : (
        <>
          <Card>
            <CardHeader>
              <CardTitle className="text-lg">Run Summary</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="flex flex-wrap items-center gap-2">
                <div className="font-medium">{run.run_id}</div>
                <Badge variant={statusVariant(run.status)}>{run.status}</Badge>
                {run.evaluation?.usefulness && (
                  <Badge variant="outline">{run.evaluation.usefulness}</Badge>
                )}
              </div>
              <div className="grid grid-cols-1 gap-3 text-sm md:grid-cols-2 xl:grid-cols-4">
                <div className="rounded bg-muted px-3 py-2">cycle {run.cycle_kind}</div>
                <div className="rounded bg-muted px-3 py-2">model {run.model}</div>
                <div className="rounded bg-muted px-3 py-2">
                  cost {formatCurrency(run.total_cost_usd ?? 0)}
                </div>
                <div className="rounded bg-muted px-3 py-2">
                  platform {run.platform_status ?? 'unknown'}
                </div>
              </div>
              <div className="text-sm text-muted-foreground">
                started {formatTimestamp(run.started_at)}
                {run.finished_at ? ` · finished ${formatTimestamp(run.finished_at)}` : ''}
                {run.session_id ? ` · session ${run.session_id}` : ''}
              </div>
              {run.failure_reason && (
                <div className="rounded bg-destructive/10 px-3 py-2 text-sm text-destructive">
                  {run.failure_reason}
                </div>
              )}
            </CardContent>
          </Card>

          <div className="grid grid-cols-1 gap-6 xl:grid-cols-2">
            <Card>
              <CardHeader>
                <CardTitle className="text-lg">Runtime Context</CardTitle>
              </CardHeader>
              <CardContent className="space-y-4">
                <SummaryList
                  label="Deployment sample"
                  items={run.runtime_context?.deployment_sample ?? []}
                />
                <SummaryList
                  label="Oversight signals"
                  items={run.runtime_context?.oversight_signal_summary ?? []}
                />
                <SummaryList
                  label="Oversight playbook"
                  items={run.runtime_context?.oversight_playbook_summary ?? []}
                />
                <SummaryList
                  label="Diagnostic candidates"
                  items={run.runtime_context?.diagnostic_candidates ?? []}
                />
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle className="text-lg">Output Summary</CardTitle>
              </CardHeader>
              <CardContent className="space-y-4">
                <SummaryList
                  label="Research reports"
                  items={run.output_summary?.research_report_summaries ?? []}
                />
                <SummaryList
                  label="Oversight alerts"
                  items={run.output_summary?.oversight_alert_summaries ?? []}
                />
                <SummaryList
                  label="Operator recommendations"
                  items={run.output_summary?.operator_recommendation_summaries ?? []}
                />
              </CardContent>
            </Card>
          </div>

          <Card>
            <CardHeader>
              <CardTitle className="text-lg">Tool Timeline</CardTitle>
            </CardHeader>
            <CardContent>
              {run.tool_calls.length === 0 ? (
                <p className="text-sm text-muted-foreground">No tool calls recorded</p>
              ) : (
                <div className="space-y-3">
                  {run.tool_calls.map((tool, index) => (
                    <div
                      key={`${tool.name}-${index}`}
                      className="flex flex-wrap items-center justify-between gap-2 rounded border p-3 text-sm"
                    >
                      <div className="font-medium">{tool.name}</div>
                      <Badge variant={tool.status === 'succeeded' ? 'success' : 'warning'}>
                        {tool.status}
                      </Badge>
                    </div>
                  ))}
                </div>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle className="text-lg">Related Proposals</CardTitle>
            </CardHeader>
            <CardContent>
              {relatedProposals.length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  This run has not produced any persisted safety proposals yet.
                </p>
              ) : (
                <div className="space-y-3">
                  {relatedProposals.map((proposal) => (
                    <div
                      key={proposal.proposal_id}
                      className="flex flex-wrap items-center justify-between gap-3 rounded border p-3"
                    >
                      <div className="space-y-1">
                        <Link
                          to={`/oversight/proposals/${encodeURIComponent(proposal.proposal_id)}`}
                          className="font-medium text-primary hover:underline"
                        >
                          {proposal.action_kind} {proposal.target_deployment_id}
                        </Link>
                        <div className="text-xs text-muted-foreground">
                          created {formatTimestamp(proposal.created_at)}
                        </div>
                      </div>
                      <Badge variant={proposal.status === 'approved' ? 'success' : 'warning'}>
                        {proposal.status}
                      </Badge>
                    </div>
                  ))}
                </div>
              )}
            </CardContent>
          </Card>
        </>
      )}
    </div>
  );
}

function SummaryList({ label, items }: { label: string; items: string[] }) {
  return (
    <div className="space-y-1">
      <div className="text-sm font-medium">{label}</div>
      {items.length === 0 ? (
        <p className="text-xs text-muted-foreground">None recorded</p>
      ) : (
        items.map((item) => (
          <div key={`${label}-${item}`} className="rounded bg-muted px-3 py-2 text-xs text-muted-foreground">
            {item}
          </div>
        ))
      )}
    </div>
  );
}
