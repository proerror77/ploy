/**
 * Risk Guard Hook — Intercepts order submissions to enforce safety limits.
 *
 * Runs BEFORE the submit_order tool executes. Denies orders that:
 * - Exceed per-trade size limit ($50)
 * - Have price outside valid range
 * - Are live orders when SIDECAR_DRY_RUN is true
 */

const MAX_ORDER_SIZE_USD = 50;
const MAX_PRICE = 0.20; // Matches our 4x reward-to-risk threshold

export interface RiskGuardInput {
  hook_event_name: string;
  tool_name: string;
  tool_input: Record<string, unknown>;
}

export interface RiskGuardOutput {
  hookSpecificOutput?: {
    hookEventName: string;
    permissionDecision: "allow" | "deny";
    permissionDecisionReason?: string;
    updatedInput?: Record<string, unknown>;
  };
}

export async function riskGuardHook(
  input: RiskGuardInput
): Promise<RiskGuardOutput> {
  if (input.hook_event_name !== "PreToolUse") return {};
  if (!input.tool_name.includes("submit_order")) return {};

  const toolInput = input.tool_input as {
    shares?: number;
    price?: number;
    dry_run?: boolean;
  };

  // Calculate order cost
  const shares = toolInput.shares || 0;
  const price = toolInput.price || 0;
  const orderCost = shares * price;

  // Block oversized orders
  if (orderCost > MAX_ORDER_SIZE_USD) {
    return {
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "deny",
        permissionDecisionReason: `Order cost $${orderCost.toFixed(2)} exceeds limit $${MAX_ORDER_SIZE_USD}`,
      },
    };
  }

  // Block high-priced entries (below reward-to-risk threshold)
  if (price > MAX_PRICE) {
    return {
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "deny",
        permissionDecisionReason: `Price $${price} exceeds max $${MAX_PRICE} (reward-to-risk < 4x)`,
      },
    };
  }

  // Force dry_run when SIDECAR_DRY_RUN is set
  const forceDryRun = process.env.SIDECAR_DRY_RUN === "true";
  if (forceDryRun && !toolInput.dry_run) {
    return {
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "allow",
        updatedInput: { ...toolInput, dry_run: true },
      },
    };
  }

  return {};
}
