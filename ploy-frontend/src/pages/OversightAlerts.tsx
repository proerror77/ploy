import { useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { api } from '@/services/api';
import { useStore } from '@/store';
import { DiagnosticsPanel } from '@/components/agent/DiagnosticsPanel';
import { ProposalQueue } from '@/components/agent/ProposalQueue';
import { Badge } from '@/components/ui/Badge';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { formatTimestamp } from '@/lib/utils';

function severityVariant(severity: string) {
  switch (severity) {
    case 'critical':
      return 'destructive' as const;
    case 'warning':
      return 'warning' as const;
    default:
      return 'secondary' as const;
  }
}

export function OversightAlerts() {
  const { oversightReport, deployments, proposals } = useStore();
  const [selectedDeploymentId, setSelectedDeploymentId] = useState<string>('');

  const platformDiagnosticsQuery = useQuery({
    queryKey: ['system-diagnostics'],
    queryFn: () => api.getSystemDiagnostics(),
    refetchInterval: 15000,
  });

  const proposalsQuery = useQuery({
    queryKey: ['proposals'],
    queryFn: () => api.getProposals(),
    refetchInterval: 15000,
  });

  const auditLogsQuery = useQuery({
    queryKey: ['audit-logs'],
    queryFn: () => api.getAuditLogs(),
    refetchInterval: 15000,
  });

  const deploymentOptions = useMemo(() => {
    const fromSignals =
      oversightReport?.signals
        .map((signal) => signal.deployment_id)
        .filter((deploymentId): deploymentId is string => Boolean(deploymentId)) ?? [];
    const ordered = [...new Set([...fromSignals, ...deployments.map((item) => item.deployment_id)])];
    return ordered;
  }, [deployments, oversightReport]);

  const activeDeploymentId =
    selectedDeploymentId || deploymentOptions[0] || deployments[0]?.deployment_id || '';

  const deploymentDiagnosticsQuery = useQuery({
    queryKey: ['deployment-diagnostics', activeDeploymentId],
    queryFn: () => api.getDeploymentDiagnostics(activeDeploymentId),
    enabled: Boolean(activeDeploymentId),
    refetchInterval: 15000,
  });

  const liveProposals = proposalsQuery.data ?? proposals;
  const relevantAuditLogs = useMemo(() => {
    const rows = auditLogsQuery.data ?? [];
    const targets = new Set(liveProposals.map((proposal) => proposal.target_deployment_id));
    return rows
      .filter((entry) => {
        if (entry.path.startsWith('/api/proposals/')) return true;
        for (const target of targets) {
          if (entry.path.includes(target)) return true;
        }
        return false;
      })
      .slice(-20)
      .reverse();
  }, [auditLogsQuery.data, liveProposals]);

  return (
    <div className="space-y-8 p-8">
      <div>
        <h1 className="text-3xl font-bold">Oversight Alerts</h1>
        <p className="text-muted-foreground">
          Drift, runaway risk, diagnostics evidence, and operator approval queue
        </p>
      </div>

      <div className="grid grid-cols-1 gap-4 md:grid-cols-4">
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Signals</CardTitle>
          </CardHeader>
          <CardContent className="text-3xl font-bold">
            {oversightReport?.signal_count ?? 0}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Critical</CardTitle>
          </CardHeader>
          <CardContent className="text-3xl font-bold">
            {oversightReport?.signals.filter((signal) => signal.severity === 'critical').length ?? 0}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Playbook Actions</CardTitle>
          </CardHeader>
          <CardContent className="text-3xl font-bold">
            {oversightReport?.recommended_actions.length ?? 0}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Pending Proposals</CardTitle>
          </CardHeader>
          <CardContent className="text-3xl font-bold">
            {liveProposals.filter((proposal) => proposal.status === 'pending').length}
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-lg">Active Oversight Signals</CardTitle>
        </CardHeader>
        <CardContent>
          {!oversightReport || oversightReport.signals.length === 0 ? (
            <p className="text-sm text-muted-foreground">No current oversight signals</p>
          ) : (
            <div className="space-y-4">
              {oversightReport.signals.map((signal, index) => (
                <div
                  key={`${signal.kind}-${signal.deployment_id ?? 'platform'}-${index}`}
                  className="rounded-lg border p-4"
                >
                  <div className="flex flex-wrap items-center gap-2">
                    <div className="font-medium">{signal.message}</div>
                    <Badge variant={severityVariant(signal.severity)}>{signal.severity}</Badge>
                    {signal.deployment_id && <Badge variant="outline">{signal.deployment_id}</Badge>}
                  </div>
                  <div className="mt-2 text-sm text-muted-foreground">
                    next action {signal.recommended_action}
                  </div>
                  {signal.evidence.length > 0 && (
                    <div className="mt-3 space-y-2">
                      {signal.evidence.map((item, evidenceIndex) => (
                        <div
                          key={`${signal.kind}-evidence-${evidenceIndex}`}
                          className="rounded bg-muted px-3 py-2 text-xs text-muted-foreground"
                        >
                          {item}
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-lg">Diagnostics Focus</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="flex flex-wrap items-center gap-3">
            <label className="text-sm text-muted-foreground" htmlFor="deployment-diagnostics">
              Deployment
            </label>
            <select
              id="deployment-diagnostics"
              value={activeDeploymentId}
              onChange={(event) => setSelectedDeploymentId(event.target.value)}
              className="rounded border bg-background px-3 py-2 text-sm"
            >
              {deploymentOptions.length === 0 && <option value="">No deployment candidates</option>}
              {deploymentOptions.map((deploymentId) => (
                <option key={deploymentId} value={deploymentId}>
                  {deploymentId}
                </option>
              ))}
            </select>
          </div>
        </CardContent>
      </Card>

      <DiagnosticsPanel
        platformReport={platformDiagnosticsQuery.data}
        deploymentReport={deploymentDiagnosticsQuery.data}
      />

      <ProposalQueue proposals={liveProposals} />

      <Card>
        <CardHeader>
          <CardTitle className="text-lg">Proposal Audit Trail</CardTitle>
        </CardHeader>
        <CardContent>
          {relevantAuditLogs.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No proposal-related audit entries yet
            </p>
          ) : (
            <div className="space-y-3">
              {relevantAuditLogs.map((entry, index) => (
                <div
                  key={`${entry.timestamp}-${entry.path}-${index}`}
                  className="rounded-lg border p-4"
                >
                  <div className="flex flex-wrap items-center justify-between gap-3">
                    <div className="space-y-1">
                      <div className="font-medium">
                        {entry.method} {entry.path}
                      </div>
                      <div className="text-xs text-muted-foreground">
                        {formatTimestamp(entry.timestamp)} · auth {entry.auth_level} · required{' '}
                        {entry.required_access}
                      </div>
                    </div>
                    <Badge variant={entry.status_code < 400 ? 'success' : 'destructive'}>
                      {entry.status_code}
                    </Badge>
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
    </div>
  );
}
