import { useEffect } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Link } from 'react-router-dom';
import { api } from '@/services/api';
import { useStore } from '@/store';
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

export function ResearchRuns() {
  const { agentRuns, setAgentRuns } = useStore();
  const runsQuery = useQuery({
    queryKey: ['agent-runs'],
    queryFn: () => api.getAgentRuns(),
    refetchInterval: 15000,
  });

  useEffect(() => {
    if (runsQuery.data) {
      setAgentRuns(runsQuery.data);
    }
  }, [runsQuery.data, setAgentRuns]);

  const runs = runsQuery.data ?? agentRuns;

  return (
    <div className="space-y-8 p-8">
      <div>
        <h1 className="text-3xl font-bold">Research Runs</h1>
        <p className="text-muted-foreground">
          Replayable sidecar runs with cost, tool usage, and usefulness scoring
        </p>
      </div>

      <div className="grid grid-cols-1 gap-4 md:grid-cols-4">
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Runs</CardTitle>
          </CardHeader>
          <CardContent className="text-3xl font-bold">{runs.length}</CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Succeeded</CardTitle>
          </CardHeader>
          <CardContent className="text-3xl font-bold">
            {runs.filter((run) => run.status === 'succeeded').length}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Alerts</CardTitle>
          </CardHeader>
          <CardContent className="text-3xl font-bold">
            {runs.reduce((sum, run) => sum + run.oversight_alerts, 0)}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Spend</CardTitle>
          </CardHeader>
          <CardContent className="text-3xl font-bold">
            {formatCurrency(
              runs.reduce((sum, run) => sum + (run.total_cost_usd ?? 0), 0)
            )}
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-lg">Recent Runs</CardTitle>
        </CardHeader>
        <CardContent>
          {runsQuery.isLoading ? (
            <p className="text-sm text-muted-foreground">Loading agent runs...</p>
          ) : runs.length === 0 ? (
            <p className="text-sm text-muted-foreground">No agent runs recorded yet</p>
          ) : (
            <div className="space-y-4">
              {runs.map((run) => (
                <div key={run.run_id} className="rounded-lg border p-4">
                  <div className="flex flex-wrap items-start justify-between gap-3">
                    <div className="space-y-2">
                      <div className="flex flex-wrap items-center gap-2">
                        <Link
                          to={`/research-runs/${encodeURIComponent(run.run_id)}`}
                          className="font-medium text-primary hover:underline"
                        >
                          {run.run_id}
                        </Link>
                        <Badge variant={statusVariant(run.status)}>{run.status}</Badge>
                        {run.evaluation?.usefulness && (
                          <Badge variant="outline">{run.evaluation.usefulness}</Badge>
                        )}
                      </div>
                      <div className="text-sm text-muted-foreground">
                        {run.cycle_kind} · {run.model} ·{' '}
                        {run.platform_status ?? 'unknown platform state'}
                      </div>
                      <div className="text-xs text-muted-foreground">
                        started {formatTimestamp(run.started_at)}
                        {run.finished_at ? ` · finished ${formatTimestamp(run.finished_at)}` : ''}
                      </div>
                    </div>
                    <div className="grid grid-cols-2 gap-3 text-sm">
                      <div className="rounded bg-muted px-3 py-2">deployments {run.deployment_count}</div>
                      <div className="rounded bg-muted px-3 py-2">
                        signals {run.oversight_signal_count}
                      </div>
                      <div className="rounded bg-muted px-3 py-2">
                        recommendations {run.operator_recommendations}
                      </div>
                      <div className="rounded bg-muted px-3 py-2">
                        cost {formatCurrency(run.total_cost_usd ?? 0)}
                      </div>
                    </div>
                  </div>

                  {run.tool_calls.length > 0 && (
                    <div className="mt-4">
                      <div className="mb-2 text-sm font-medium">Tool calls</div>
                      <div className="flex flex-wrap gap-2">
                        {run.tool_calls.map((tool, index) => (
                          <Badge key={`${run.run_id}-${tool.name}-${index}`} variant="secondary">
                            {tool.name}
                          </Badge>
                        ))}
                      </div>
                    </div>
                  )}

                  {run.runtime_context && (
                    <div className="mt-4 grid grid-cols-1 gap-4 xl:grid-cols-2">
                      <div className="space-y-2">
                        <div className="text-sm font-medium">Runtime context</div>
                        {run.runtime_context.deployment_sample.length > 0 && (
                          <div className="space-y-1">
                            <div className="text-xs text-muted-foreground">deployments</div>
                            <div className="flex flex-wrap gap-2">
                              {run.runtime_context.deployment_sample.map((item) => (
                                <Badge key={`${run.run_id}-deployment-${item}`} variant="outline">
                                  {item}
                                </Badge>
                              ))}
                            </div>
                          </div>
                        )}
                        {run.runtime_context.oversight_signal_summary.length > 0 && (
                          <div className="space-y-1">
                            <div className="text-xs text-muted-foreground">oversight signals</div>
                            {run.runtime_context.oversight_signal_summary.map((item) => (
                              <div
                                key={`${run.run_id}-signal-${item}`}
                                className="rounded bg-muted px-3 py-2 text-xs text-muted-foreground"
                              >
                                {item}
                              </div>
                            ))}
                          </div>
                        )}
                        {run.runtime_context.diagnostic_candidates.length > 0 && (
                          <div className="space-y-1">
                            <div className="text-xs text-muted-foreground">diagnostic candidates</div>
                            <div className="flex flex-wrap gap-2">
                              {run.runtime_context.diagnostic_candidates.map((item) => (
                                <Badge key={`${run.run_id}-diagnostic-${item}`} variant="secondary">
                                  {item}
                                </Badge>
                              ))}
                            </div>
                          </div>
                        )}
                      </div>

                      {run.output_summary && (
                        <div className="space-y-2">
                          <div className="text-sm font-medium">Output summary</div>
                          {run.output_summary.research_report_summaries.length > 0 && (
                            <div className="space-y-1">
                              <div className="text-xs text-muted-foreground">research reports</div>
                              {run.output_summary.research_report_summaries.map((item) => (
                                <div
                                  key={`${run.run_id}-report-${item}`}
                                  className="rounded bg-muted px-3 py-2 text-xs text-muted-foreground"
                                >
                                  {item}
                                </div>
                              ))}
                            </div>
                          )}
                          {run.output_summary.oversight_alert_summaries.length > 0 && (
                            <div className="space-y-1">
                              <div className="text-xs text-muted-foreground">alerts</div>
                              {run.output_summary.oversight_alert_summaries.map((item) => (
                                <div
                                  key={`${run.run_id}-alert-${item}`}
                                  className="rounded bg-muted px-3 py-2 text-xs text-muted-foreground"
                                >
                                  {item}
                                </div>
                              ))}
                            </div>
                          )}
                          {run.output_summary.operator_recommendation_summaries.length > 0 && (
                            <div className="space-y-1">
                              <div className="text-xs text-muted-foreground">recommendations</div>
                              {run.output_summary.operator_recommendation_summaries.map((item) => (
                                <div
                                  key={`${run.run_id}-recommendation-${item}`}
                                  className="rounded bg-muted px-3 py-2 text-xs text-muted-foreground"
                                >
                                  {item}
                                </div>
                              ))}
                            </div>
                          )}
                        </div>
                      )}
                    </div>
                  )}

                  {run.failure_reason && (
                    <div className="mt-4 rounded bg-destructive/10 px-3 py-2 text-sm text-destructive">
                      {run.failure_reason}
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
