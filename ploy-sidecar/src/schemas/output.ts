/**
 * Structured output schema for the research-and-oversight sidecar.
 *
 * Keeps the sidecar focused on research runs, oversight alerts, and operator
 * recommendations rather than trade decisions.
 */

export const researchOutputSchema = {
  type: "object" as const,
  properties: {
    summary: {
      type: "object" as const,
      properties: {
        timestamp: { type: "string" as const },
        platform_status: { type: "string" as const },
        deployments_reviewed: { type: "number" as const },
        research_tasks: { type: "number" as const },
        oversight_alerts: { type: "number" as const },
        operator_recommendations: { type: "number" as const },
      },
      required: [
        "timestamp",
        "platform_status",
        "deployments_reviewed",
        "research_tasks",
        "oversight_alerts",
        "operator_recommendations",
      ],
    },
    research_reports: {
      type: "array" as const,
      items: {
        type: "object" as const,
        properties: {
          subject: { type: "string" as const },
          kind: {
            type: "string" as const,
            enum: ["replay", "backtest", "config_compare", "market_scan", "diagnostic"],
          },
          status: {
            type: "string" as const,
            enum: ["completed", "skipped", "failed"],
          },
          finding: { type: "string" as const },
          evidence: {
            type: "array" as const,
            items: { type: "string" as const },
          },
        },
        required: ["subject", "kind", "status", "finding"],
      },
    },
    oversight_alerts: {
      type: "array" as const,
      items: {
        type: "object" as const,
        properties: {
          severity: {
            type: "string" as const,
            enum: ["info", "warning", "critical"],
          },
          deployment_id: { type: "string" as const },
          kind: {
            type: "string" as const,
            enum: [
              "drift",
              "runaway_risk",
              "pnl_regression",
              "exposure_watch",
              "config_mismatch",
              "data_gap",
              "none",
            ],
          },
          message: { type: "string" as const },
          recommended_action: { type: "string" as const },
        },
        required: ["severity", "kind", "message"],
      },
    },
    operator_recommendations: {
      type: "array" as const,
      items: {
        type: "object" as const,
        properties: {
          kind: {
            type: "string" as const,
            enum: [
              "monitor",
              "replay",
              "backtest",
              "diagnose",
              "compare_configs",
              "create_proposal",
              "pause_review",
              "human_follow_up",
            ],
          },
          target: { type: "string" as const },
          rationale: { type: "string" as const },
          evidence: {
            type: "array" as const,
            items: { type: "string" as const },
          },
        },
        required: ["kind", "target", "rationale"],
      },
    },
  },
  required: ["summary", "research_reports", "oversight_alerts", "operator_recommendations"],
};
