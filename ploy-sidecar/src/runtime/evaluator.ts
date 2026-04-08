type StructuredOutput = {
  research_reports?: Array<unknown>;
  oversight_alerts?: Array<unknown>;
  operator_recommendations?: Array<unknown>;
};

export type RunEvaluation = {
  usefulness: "high" | "medium" | "low";
  research_reports: number;
  oversight_alerts: number;
  operator_recommendations: number;
};

export function evaluateRun(output: StructuredOutput | null | undefined): RunEvaluation {
  const researchReports = Array.isArray(output?.research_reports) ? output.research_reports.length : 0;
  const oversightAlerts = Array.isArray(output?.oversight_alerts) ? output.oversight_alerts.length : 0;
  const operatorRecommendations = Array.isArray(output?.operator_recommendations)
    ? output.operator_recommendations.length
    : 0;

  const totalSignals = researchReports + oversightAlerts + operatorRecommendations;
  const usefulness =
    totalSignals >= 4 ? "high" : totalSignals >= 1 ? "medium" : "low";

  return {
    usefulness,
    research_reports: researchReports,
    oversight_alerts: oversightAlerts,
    operator_recommendations: operatorRecommendations,
  };
}
