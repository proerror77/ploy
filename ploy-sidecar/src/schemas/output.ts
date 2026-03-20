/**
 * Structured output schema for the Claude Commander agent.
 *
 * Forces Claude to return structured JSON with trading opportunities and
 * explicit operator actions, ensuring deterministic parsing by the sidecar.
 */

export const tradingOutputSchema = {
  type: "object" as const,
  properties: {
    scan_summary: {
      type: "object" as const,
      properties: {
        games_scanned: { type: "number" as const },
        in_progress_games: { type: "number" as const },
        comeback_candidates: { type: "number" as const },
        markets_checked: { type: "number" as const },
        timestamp: { type: "string" as const },
      },
      required: ["games_scanned", "in_progress_games", "timestamp"],
    },
    opportunities: {
      type: "array" as const,
      items: {
        type: "object" as const,
        properties: {
          game_id: { type: "string" as const },
          game_name: { type: "string" as const },
          trailing_team: { type: "string" as const },
          trailing_abbrev: { type: "string" as const },
          deficit: { type: "number" as const },
          quarter: { type: "number" as const },
          clock: { type: "string" as const },
          // Market data
          market_slug: { type: "string" as const },
          market_price: { type: "number" as const },
          // Risk metrics
          reward_risk_ratio: { type: "number" as const },
          estimated_win_prob: { type: "number" as const },
          expected_value: { type: "number" as const },
          kelly_fraction: { type: "number" as const },
          // Decision
          action: {
            type: "string" as const,
            enum: ["TRADE", "PASS", "MONITOR"],
          },
          grok_decision: {
            type: "string" as const,
            enum: ["trade", "pass", "not_queried"],
          },
          confidence: {
            type: "string" as const,
            enum: ["low", "medium", "high"],
          },
          reasoning: { type: "string" as const },
          risk_factors: {
            type: "array" as const,
            items: { type: "string" as const },
          },
        },
        required: [
          "game_id",
          "trailing_team",
          "deficit",
          "action",
          "confidence",
          "reasoning",
        ],
      },
    },
    operator_actions: {
      type: "array" as const,
      items: {
        type: "object" as const,
        properties: {
          kind: { type: "string" as const },
          target: { type: "string" as const },
          status: { type: "string" as const },
          details: { type: "string" as const },
        },
        required: ["kind", "target", "status"],
      },
    },
  },
  required: ["scan_summary", "opportunities", "operator_actions"],
};
