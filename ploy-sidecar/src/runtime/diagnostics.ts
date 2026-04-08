export type DiagnosticsEvidence = {
  source: string;
  label: string;
  detail: string;
  observed_at?: string | null;
};

export type DiagnosticsFinding = {
  severity: string;
  kind: string;
  message: string;
  first_observed_at?: string | null;
  likely_causes?: string[];
  operator_command?: string | null;
  evidence?: DiagnosticsEvidence[];
};

export type PlatformDiagnosticsReport = {
  generated_at: string;
  platform_status: string;
  first_diverged_metric?: string | null;
  findings?: DiagnosticsFinding[];
  recent_evidence?: DiagnosticsEvidence[];
};

export type DeploymentDiagnosticsReport = {
  generated_at: string;
  deployment_id: string;
  bundle_id: string;
  runtime_mode: string;
  account_id: string;
  desired_state: string;
  observed_state: string;
  max_gross_exposure?: string | null;
  primary_diagnosis: string;
  first_diverged_metric?: string | null;
  findings?: DiagnosticsFinding[];
  recent_evidence?: DiagnosticsEvidence[];
};

export function collectDiagnosticCandidates(
  oversightSignals: Array<{ deployment_id?: string | null; severity?: string }>
): string[] {
  const ranked = oversightSignals
    .filter((signal): signal is { deployment_id: string; severity?: string } => Boolean(signal.deployment_id))
    .sort((left, right) => severityRank(right.severity) - severityRank(left.severity));

  return [...new Set(ranked.map((signal) => signal.deployment_id))].slice(0, 5);
}

function severityRank(severity?: string): number {
  switch (severity) {
    case "critical":
      return 3;
    case "warning":
      return 2;
    case "info":
      return 1;
    default:
      return 0;
  }
}
