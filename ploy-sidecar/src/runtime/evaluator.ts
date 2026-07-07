type StructuredOutput = {
  research_reports?: Array<unknown>;
  oversight_alerts?: Array<unknown>;
  operator_recommendations?: Array<unknown>;
};

export type RunEvaluation = {
  usefulness: "low" | "medium" | "high";
  research_reports: number;
  oversight_alerts: number;
  operator_recommendations: number;
};

export function evaluateRun(output: StructuredOutput): RunEvaluation {
  const researchReports = output.research_reports?.length ?? 0;
  const oversightAlerts = output.oversight_alerts?.length ?? 0;
  const operatorRecommendations = output.operator_recommendations?.length ?? 0;
  const score = researchReports + oversightAlerts + operatorRecommendations;
  return {
    usefulness: score >= 3 ? "high" : score >= 1 ? "medium" : "low",
    research_reports: researchReports,
    oversight_alerts: oversightAlerts,
    operator_recommendations: operatorRecommendations,
  };
}
