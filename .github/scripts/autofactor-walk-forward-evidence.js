const fs = require("fs");

function readJson(path) {
  if (!fs.existsSync(path)) return null;
  const raw = fs.readFileSync(path, "utf8");
  try {
    return JSON.parse(raw);
  } catch (error) {
    const sanitized = raw
      .replace(/:\s*NaN\b/g, ": null")
      .replace(/:\s*Infinity\b/g, ": null")
      .replace(/:\s*-Infinity\b/g, ": null");
    return JSON.parse(sanitized);
  }
}

function stringifyBlockers(blockers, limit = 3) {
  const values = Array.isArray(blockers) ? blockers.filter(Boolean) : [];
  if (values.length === 0) return "none";
  const shown = values.slice(0, limit);
  const suffix = values.length > shown.length ? `, +${values.length - shown.length} more` : "";
  return `${shown.join("; ")}${suffix}`;
}

function decisionFromArtifacts({ promotion, handoff }) {
  if (!promotion && !handoff) {
    return {
      decision: "fix-workflow-or-artifact",
      reason: "missing AutoFactor promotion and handoff artifacts",
    };
  }

  if (handoff && handoff.status === "ready" && Array.isArray(handoff.strategies) && handoff.strategies.length > 0) {
    return {
      decision: "promote-to-dry-run",
      reason: "qualified AutoFactor strategy handoff is ready",
    };
  }

  const gate = (handoff && handoff.promotion_gate) || (promotion && promotion.promotion_gate) || {};
  const blockedGates = Array.isArray(gate.blocked_gates) ? gate.blocked_gates : [];
  const blockerText = blockedGates.join(" ").toLowerCase();

  if (blockerText.includes("data_quality") || blockerText.includes("deribit")) {
    return {
      decision: "fix-data",
      reason: stringifyBlockers(blockedGates),
    };
  }
  if (blockerText.includes("replay") || blockerText.includes("runtime") || blockerText.includes("parity")) {
    return {
      decision: "fix-runtime",
      reason: stringifyBlockers(blockedGates),
    };
  }
  if (blockedGates.length > 0) {
    return {
      decision: "revise",
      reason: stringifyBlockers(blockedGates),
    };
  }

  if (promotion && promotion.decision === "blocked") {
    return {
      decision: "revise",
      reason: "no AutoFactor row qualified under the requested target/profile",
    };
  }

  return {
    decision: "pending-review",
    reason: "promotion artifacts were produced but no terminal decision was detected",
  };
}

function topStrategies(handoff, limit = 3) {
  if (!handoff || !Array.isArray(handoff.strategies)) return [];
  return handoff.strategies.slice(0, limit).map((strategy) => {
    const metrics = strategy.metrics || {};
    return [
      strategy.name || "<unknown>",
      strategy.runtime_score || "<missing-runtime-score>",
      `icir=${metrics.icir ?? "n/a"}`,
      `top_bucket_avg_label=${metrics.top_bucket_avg_label ?? "n/a"}`,
    ].join(" | ");
  });
}

function actionableBlockers({ handoff, promotion }) {
  const gate = (handoff && handoff.promotion_gate) || (promotion && promotion.promotion_gate) || {};
  const blockedGates = Array.isArray(gate.blocked_gates) ? gate.blocked_gates : [];
  if (handoff && handoff.status === "ready") {
    return blockedGates.filter((item) => {
      const value = String(item || "");
      return !value.startsWith("symbol_holdout:") && !value.startsWith("walk_forward_oos:");
    });
  }
  return blockedGates;
}

function buildWalkForwardEvidence({ title, metadata, artifactDir, runnerLabel }) {
  const promotion = readJson(`${artifactDir}/autofactor-strategy-promotion.json`);
  const handoff = readJson(`${artifactDir}/autofactor-strategy-handoff.json`);
  const outcome = decisionFromArtifacts({ promotion, handoff });
  const gate = (handoff && handoff.promotion_gate) || (promotion && promotion.promotion_gate) || {};
  const strategies = topStrategies(handoff);
  const blockers = actionableBlockers({ handoff, promotion });

  const body = [
    `${title}:`,
    "",
    `- Workflow: ${metadata.workflow}`,
    `- Run URL: ${metadata.runUrl}`,
    `- Git ref: ${metadata.gitRef}`,
    `- Source snapshot run: \`${metadata.snapshotRunId}\``,
    `- Dataset/window: ${metadata.startDate} -> ${metadata.endDate}`,
    `- Symbols: ${metadata.symbols}`,
    `- Artifact: \`${metadata.artifactName}\``,
    `- Handoff status: \`${handoff ? handoff.status : "missing"}\``,
    `- Promotion decision: \`${promotion ? promotion.decision : "missing"}\``,
    `- Promotion gate ready: \`${gate.ready === true}\``,
    `- Actionable blockers: \`${stringifyBlockers(blockers)}\``,
    `- Decision: ${outcome.decision}`,
    `- Next action: ${outcome.reason}`,
  ];

  if (strategies.length > 0) {
    body.push("", "Qualified strategies:");
    for (const strategy of strategies) body.push(`- ${strategy}`);
  }

  return {
    body: body.join("\n"),
    decision: outcome.decision,
    labels: ["evidence:walk-forward", runnerLabel].filter(Boolean),
  };
}

module.exports = {
  buildWalkForwardEvidence,
  decisionFromArtifacts,
};
