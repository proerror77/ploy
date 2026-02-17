/**
 * Structured output schema for the Claude Commander agent.
 *
 * Forces Claude to return structured JSON with trading opportunities,
 * ensuring deterministic parsing by the sidecar orchestrator.
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
    orders_submitted: {
      type: "array" as const,
      items: {
        type: "object" as const,
        properties: {
          market_slug: { type: "string" as const },
          side: { type: "string" as const },
          shares: { type: "number" as const },
          price: { type: "number" as const },
          dry_run: { type: "boolean" as const },
          status: { type: "string" as const },
        },
        required: ["market_slug", "shares", "price", "dry_run", "status"],
      },
    },
  },
  required: ["scan_summary", "opportunities", "orders_submitted"],
};
