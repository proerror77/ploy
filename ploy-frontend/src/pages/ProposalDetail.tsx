import { useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Link, useParams } from 'react-router-dom';
import { api } from '@/services/api';
import { Badge } from '@/components/ui/Badge';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { formatTimestamp } from '@/lib/utils';

function statusVariant(status: string) {
  switch (status) {
    case 'approved':
      return 'success' as const;
    case 'rejected':
    case 'failed':
      return 'destructive' as const;
    default:
      return 'warning' as const;
  }
}

export function ProposalDetail() {
  const { proposalId = '' } = useParams();

  const proposalQuery = useQuery({
    queryKey: ['proposal', proposalId],
    queryFn: () => api.getProposal(proposalId),
    enabled: Boolean(proposalId),
    refetchInterval: 15000,
  });

  const auditLogsQuery = useQuery({
    queryKey: ['audit-logs'],
    queryFn: () => api.getAuditLogs(),
    refetchInterval: 15000,
  });

  const proposal = proposalQuery.data;
  const deploymentDiagnosticsQuery = useQuery({
    queryKey: ['deployment-diagnostics', proposal?.target_deployment_id],
    queryFn: () => api.getDeploymentDiagnostics(proposal!.target_deployment_id),
    enabled: Boolean(proposal?.target_deployment_id),
    refetchInterval: 15000,
  });

  const sourceRunQuery = useQuery({
    queryKey: ['agent-run', proposal?.source_run_id],
    queryFn: () => api.getAgentRun(proposal!.source_run_id!),
    enabled: Boolean(proposal?.source_run_id),
    refetchInterval: 15000,
  });

  const relatedAuditLogs = useMemo(() => {
    const rows = auditLogsQuery.data ?? [];
    if (!proposal) return [];
    return rows
      .filter((entry) => {
        if (entry.path.includes(proposal.proposal_id)) return true;
        return entry.path.includes(proposal.target_deployment_id);
      })
      .slice(-20)
      .reverse();
  }, [auditLogsQuery.data, proposal]);

  return (
    <div className="space-y-8 p-8">
      <div className="space-y-2">
        <Link to="/oversight" className="text-sm text-primary hover:underline">
          ← Back to Oversight
        </Link>
        <div>
          <h1 className="text-3xl font-bold">Proposal Decision Detail</h1>
          <p className="text-muted-foreground">
            Audit, evidence, source run, and target deployment context for one safety proposal.
          </p>
        </div>
      </div>

      {proposalQuery.isLoading ? (
        <Card>
          <CardContent className="pt-6 text-sm text-muted-foreground">
            Loading proposal detail...
          </CardContent>
        </Card>
      ) : proposalQuery.isError || !proposal ? (
        <Card>
          <CardContent className="pt-6 text-sm text-destructive">
            Unable to load proposal detail for {proposalId || 'unknown proposal'}.
          </CardContent>
        </Card>
      ) : (
        <>
          <Card>
            <CardHeader>
              <CardTitle className="text-lg">Proposal Summary</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="flex flex-wrap items-center gap-2">
                <div className="font-medium">{proposal.proposal_id}</div>
                <Badge variant={statusVariant(proposal.status)}>{proposal.status}</Badge>
                <Badge variant="outline">{proposal.action_kind}</Badge>
                <Badge variant="outline">{proposal.target_deployment_id}</Badge>
              </div>
              <div className="text-sm text-muted-foreground">{proposal.rationale}</div>
              <div className="text-xs text-muted-foreground">
                created {formatTimestamp(proposal.created_at)}
                {proposal.decided_at ? ` · decided ${formatTimestamp(proposal.decided_at)}` : ''}
              </div>
              {proposal.decision_note && (
                <div className="rounded bg-muted px-3 py-2 text-sm text-muted-foreground">
                  decision note {proposal.decision_note}
                </div>
              )}
              {proposal.proposed_max_gross_exposure && (
                <div className="rounded bg-muted px-3 py-2 text-sm text-muted-foreground">
                  proposed max exposure {proposal.proposed_max_gross_exposure}
                </div>
              )}
            </CardContent>
          </Card>

          <div className="grid grid-cols-1 gap-6 xl:grid-cols-2">
            <Card>
              <CardHeader>
                <CardTitle className="text-lg">Evidence</CardTitle>
              </CardHeader>
              <CardContent>
                {proposal.evidence.length === 0 ? (
                  <p className="text-sm text-muted-foreground">No evidence recorded</p>
                ) : (
                  <div className="space-y-2">
                    {proposal.evidence.map((item, index) => (
                      <div
                        key={`${proposal.proposal_id}-evidence-${index}`}
                        className="rounded bg-muted px-3 py-2 text-xs text-muted-foreground"
                      >
                        {item}
                      </div>
                    ))}
                  </div>
                )}
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle className="text-lg">Source Run</CardTitle>
              </CardHeader>
              <CardContent>
                {!proposal.source_run_id ? (
                  <p className="text-sm text-muted-foreground">No source run recorded</p>
                ) : sourceRunQuery.isLoading ? (
                  <p className="text-sm text-muted-foreground">Loading source run...</p>
                ) : sourceRunQuery.isError || !sourceRunQuery.data ? (
                  <p className="text-sm text-destructive">
                    Unable to load source run {proposal.source_run_id}.
                  </p>
                ) : (
                  <div className="space-y-3">
                    <Link
                      to={`/research-runs/${encodeURIComponent(proposal.source_run_id)}`}
                      className="font-medium text-primary hover:underline"
                    >
                      {proposal.source_run_id}
                    </Link>
                    <div className="text-sm text-muted-foreground">
                      {sourceRunQuery.data.cycle_kind} · {sourceRunQuery.data.model} ·{' '}
                      {sourceRunQuery.data.platform_status ?? 'unknown platform state'}
                    </div>
                    <div className="text-xs text-muted-foreground">
                      started {formatTimestamp(sourceRunQuery.data.started_at)}
                    </div>
                  </div>
                )}
              </CardContent>
            </Card>
          </div>

          <Card>
            <CardHeader>
              <CardTitle className="text-lg">Target Deployment Diagnostics</CardTitle>
            </CardHeader>
            <CardContent>
              {deploymentDiagnosticsQuery.isLoading ? (
                <p className="text-sm text-muted-foreground">Loading deployment diagnostics...</p>
              ) : deploymentDiagnosticsQuery.isError || !deploymentDiagnosticsQuery.data ? (
                <p className="text-sm text-destructive">
                  Unable to load diagnostics for {proposal.target_deployment_id}.
                </p>
              ) : (
                <div className="space-y-4">
                  <div className="grid grid-cols-1 gap-3 text-sm md:grid-cols-2 xl:grid-cols-4">
                    <div className="rounded bg-muted px-3 py-2">
                      pending intents {deploymentDiagnosticsQuery.data.metrics.pending_intents}
                    </div>
                    <div className="rounded bg-muted px-3 py-2">
                      active orders {deploymentDiagnosticsQuery.data.metrics.active_orders}
                    </div>
                    <div className="rounded bg-muted px-3 py-2">
                      open positions {deploymentDiagnosticsQuery.data.metrics.open_positions}
                    </div>
                    <div className="rounded bg-muted px-3 py-2">
                      gross exposure {deploymentDiagnosticsQuery.data.metrics.gross_exposure}
                    </div>
                  </div>
                  <div className="text-sm text-muted-foreground">
                    primary diagnosis {deploymentDiagnosticsQuery.data.primary_diagnosis}
                  </div>
                  {deploymentDiagnosticsQuery.data.recent_evidence &&
                    deploymentDiagnosticsQuery.data.recent_evidence.length > 0 && (
                      <div className="space-y-2">
                        {deploymentDiagnosticsQuery.data.recent_evidence.map((item, index) => (
                          <div
                            key={`${item.source}-${item.label}-${index}`}
                            className="rounded border p-3 text-xs text-muted-foreground"
                          >
                            <div className="font-medium text-foreground">
                              {item.source} · {item.label}
                            </div>
                            <div>{item.detail}</div>
                            {item.observed_at && (
                              <div className="mt-1">{formatTimestamp(item.observed_at)}</div>
                            )}
                          </div>
                        ))}
                      </div>
                    )}
                </div>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle className="text-lg">Audit Timeline</CardTitle>
            </CardHeader>
            <CardContent>
              {relatedAuditLogs.length === 0 ? (
                <p className="text-sm text-muted-foreground">No related audit entries recorded yet</p>
              ) : (
                <div className="space-y-3">
                  {relatedAuditLogs.map((entry, index) => (
                    <div
                      key={`${entry.timestamp}-${entry.path}-${index}`}
                      className="rounded border p-3"
                    >
                      <div className="flex flex-wrap items-center justify-between gap-2">
                        <div className="font-medium">
                          {entry.method} {entry.path}
                        </div>
                        <Badge variant={entry.status_code < 400 ? 'success' : 'destructive'}>
                          {entry.status_code}
                        </Badge>
                      </div>
                      <div className="mt-1 text-xs text-muted-foreground">
                        {formatTimestamp(entry.timestamp)} · auth {entry.auth_level} · required{' '}
                        {entry.required_access}
                      </div>
                      <div className="mt-2 text-sm text-muted-foreground">
                        outcome {entry.outcome}
                        {entry.message ? ` · ${entry.message}` : ''}
                      </div>
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
