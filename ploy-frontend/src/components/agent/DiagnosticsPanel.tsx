import type {
  DeploymentDiagnosticsReport,
  DiagnosticsFinding,
  PlatformDiagnosticsReport,
} from '@/types';
import { Badge } from '@/components/ui/Badge';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { formatTimestamp } from '@/lib/utils';

interface DiagnosticsPanelProps {
  platformReport?: PlatformDiagnosticsReport | null;
  deploymentReport?: DeploymentDiagnosticsReport | null;
}

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

function FindingsList({ findings }: { findings?: DiagnosticsFinding[] }) {
  if (!findings || findings.length === 0) {
    return <p className="text-sm text-muted-foreground">No diagnostics findings</p>;
  }

  return (
    <div className="space-y-3">
      {findings.map((finding, index) => (
        <div key={`${finding.kind}-${index}`} className="rounded-lg border p-4">
          <div className="mb-2 flex flex-wrap items-center gap-2">
            <div className="font-medium">{finding.message}</div>
            <Badge variant={severityVariant(finding.severity)}>{finding.severity}</Badge>
          </div>
          <div className="text-sm text-muted-foreground">{finding.kind}</div>
          {finding.first_observed_at && (
            <div className="mt-1 text-xs text-muted-foreground">
              first seen {formatTimestamp(finding.first_observed_at)}
            </div>
          )}
          {finding.operator_command && (
            <div className="mt-2 rounded bg-muted px-3 py-2 font-mono text-xs">
              {finding.operator_command}
            </div>
          )}
          {finding.likely_causes && finding.likely_causes.length > 0 && (
            <div className="mt-2 flex flex-wrap gap-2">
              {finding.likely_causes.map((cause, causeIndex) => (
                <Badge key={`${finding.kind}-cause-${causeIndex}`} variant="outline">
                  {cause}
                </Badge>
              ))}
            </div>
          )}
          {finding.evidence && finding.evidence.length > 0 && (
            <div className="mt-3 space-y-2">
              {finding.evidence.map((item, evidenceIndex) => (
                <div
                  key={`${finding.kind}-evidence-${evidenceIndex}`}
                  className="rounded border border-dashed px-3 py-2 text-xs text-muted-foreground"
                >
                  <div className="font-medium text-foreground">{item.label}</div>
                  <div>{item.detail}</div>
                </div>
              ))}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

function EvidenceList({
  title,
  evidence,
}: {
  title: string;
  evidence?: Array<{ label: string; detail: string; observed_at?: string | null }>;
}) {
  if (!evidence || evidence.length === 0) {
    return null;
  }

  return (
    <div className="space-y-2">
      <div className="text-sm font-medium">{title}</div>
      {evidence.map((item, index) => (
        <div key={`${title}-${index}`} className="rounded border border-dashed px-3 py-2 text-xs">
          <div className="font-medium text-foreground">{item.label}</div>
          <div className="text-muted-foreground">{item.detail}</div>
          {item.observed_at && (
            <div className="mt-1 text-muted-foreground">{formatTimestamp(item.observed_at)}</div>
          )}
        </div>
      ))}
    </div>
  );
}

export function DiagnosticsPanel({
  platformReport,
  deploymentReport,
}: DiagnosticsPanelProps) {
  return (
    <div className="grid grid-cols-1 gap-6 xl:grid-cols-2">
      <Card>
        <CardHeader>
          <CardTitle className="text-lg">Platform Diagnostics</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          {!platformReport ? (
            <p className="text-sm text-muted-foreground">Platform diagnostics unavailable</p>
          ) : (
            <>
              <div className="flex flex-wrap items-center gap-3 text-sm text-muted-foreground">
                <span>status {platformReport.platform_status}</span>
                <span>generated {formatTimestamp(platformReport.generated_at)}</span>
                {platformReport.first_diverged_metric && (
                  <span>first diverged {platformReport.first_diverged_metric}</span>
                )}
              </div>
              <FindingsList findings={platformReport.findings} />
              <EvidenceList title="Recent platform evidence" evidence={platformReport.recent_evidence} />
            </>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-lg">Deployment Diagnostics</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          {!deploymentReport ? (
            <p className="text-sm text-muted-foreground">
              Select a deployment with active oversight signals to load deployment diagnostics
            </p>
          ) : (
            <>
              <div className="space-y-1 text-sm text-muted-foreground">
                <div>
                  {deploymentReport.deployment_id} · {deploymentReport.bundle_id} ·{' '}
                  {deploymentReport.runtime_mode}
                </div>
                <div>
                  desired {deploymentReport.desired_state} · observed {deploymentReport.observed_state}
                </div>
                <div>{deploymentReport.primary_diagnosis}</div>
              </div>
              <div className="grid grid-cols-2 gap-3 text-sm">
                <div className="rounded border p-3">
                  <div className="text-muted-foreground">Gross Exposure</div>
                  <div className="mt-1 font-medium">{deploymentReport.metrics.gross_exposure}</div>
                </div>
                <div className="rounded border p-3">
                  <div className="text-muted-foreground">Net PnL</div>
                  <div className="mt-1 font-medium">{deploymentReport.metrics.net_pnl}</div>
                </div>
                <div className="rounded border p-3">
                  <div className="text-muted-foreground">Active Orders</div>
                  <div className="mt-1 font-medium">{deploymentReport.metrics.active_orders}</div>
                </div>
                <div className="rounded border p-3">
                  <div className="text-muted-foreground">Open Positions</div>
                  <div className="mt-1 font-medium">{deploymentReport.metrics.open_positions}</div>
                </div>
              </div>
              <FindingsList findings={deploymentReport.findings} />
              <EvidenceList
                title="Recent deployment evidence"
                evidence={deploymentReport.recent_evidence}
              />
            </>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
