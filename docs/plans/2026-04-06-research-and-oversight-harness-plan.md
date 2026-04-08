# Research And Oversight Harness Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a harness layer to Ploy that helps with replay, backtesting, optimization orchestration, runtime monitoring, drift detection, and diagnostics without giving the agent ownership of alpha, realtime strategy judgment, or execution decisions.

**Architecture:** Keep trading logic deterministic and rule-owned. The `Strategy Plane` remains the only place that decides entries, exits, sizing, and state transitions. The `Execution Plane` remains the only live order ingress and enforcement path. The harness layer sits outside the hot path and uses the control plane, database, replay/backtest modules, and operator surfaces to analyze, monitor, and propose actions.

**Hard Rules:**
- Do not move realtime trade judgment into the agent loop.
- Do not let the sidecar bypass the canonical execution path.
- Do not give the agent direct ownership of alpha discovery.
- Do not let the agent mutate live strategy behavior without an explicit operator-mediated control-plane contract.

**System Principle:**
- Human owns alpha.
- Rules decide trades.
- Governance owns capital policy.
- Execution owns hard enforcement.
- Agent evaluates, monitors, and explains.

**Tech Stack:** Rust strategy/runtime modules, `ployd` control plane, PostgreSQL market/trading history, `crates/ploy-research`, `crates/ploy-strategy-bundles`, TypeScript sidecar runtime, frontend/CLI operator surfaces, JSONL or DB-backed trace persistence

---

### Task 1: Build a research harness surface over replay, backtest, and optimization tooling

**Files:**
- Create: `apps/ployctl/src/research.rs`
- Modify: `apps/ployctl/src/main.rs`
- Modify: `apps/ployctl/src/lib.rs`
- Create: `ploy-sidecar/src/tools/research.ts`
- Modify: `ploy-sidecar/src/index.ts`
- Modify: `ploy-sidecar/src/schemas/output.ts`
- Modify: `README.md`

- [ ] Expose deterministic research actions through CLI and sidecar tools: replay a deployment, run a bounded backtest, compare two parameter/config versions, and summarize results.
- [ ] Keep research tools operator-safe: no implicit live writes, no hidden strategy mutation, no venue connectivity assumptions.
- [ ] Make the sidecar use these tools for “test this idea” workflows instead of inventing strategy logic itself.
- [ ] Return structured research outputs with run id, data range, config hash, result metrics, and caveats.
- [ ] Document that the agent is allowed to orchestrate research jobs, not define alpha.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-research-harness rtk cargo test -p ployctl -- --nocapture
cd ploy-sidecar && npm run build
```

Expected: replay/backtest/compare workflows are available through stable operator-facing tools without touching live execution.

### Task 2: Add an oversight harness for runaway detection and runtime safety monitoring

**Files:**
- Create: `crates/ploy-operator-contracts/src/oversight.rs`
- Modify: `crates/ploy-operator-contracts/src/lib.rs`
- Modify: `crates/ploy-operator-contracts/src/events.rs`
- Modify: `apps/ployd/src/http.rs`
- Modify: `apps/ployd/src/main.rs`
- Create: `ploy-sidecar/src/runtime/oversight.ts`
- Modify: `ploy-sidecar/src/index.ts`

- [ ] Define oversight event contracts for drift alerts, runaway warnings, anomalous turnover, degraded fill quality, and policy suggestions.
- [ ] Use control-plane snapshots plus historical DB reads to detect when a deployment deviates from expected behavior.
- [ ] Keep oversight read-mostly: the default output is an alert or proposal, not an automatic live mutation.
- [ ] Publish oversight events on the same SSE stream as deployment and trading snapshots.
- [ ] Classify alerts by severity and recommended operator action: `monitor`, `review`, `pause_candidate`, `drain_candidate`.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-research-harness rtk cargo test -p ploy-operator-contracts -- --nocapture
cd ploy-sidecar && npm run build
```

Expected: the system can surface strategy instability as first-class operator events before it becomes a live-loss incident.

### Task 3: Add diagnostics and root-cause workflows for operator investigations

**Files:**
- Create: `ploy-sidecar/src/tools/diagnostics.ts`
- Create: `ploy-sidecar/src/runtime/diagnostics.ts`
- Modify: `ploy-sidecar/src/index.ts`
- Modify: `ploy-sidecar/README.md`
- Modify: `apps/ployctl/src/system.rs`
- Modify: `apps/ployctl/src/trading.rs`

- [ ] Give the sidecar explicit diagnostics tools for “why did this deployment degrade,” “what changed,” and “which metric diverged first.”
- [ ] Make diagnostics compare across deployment versions, time windows, and account scope where possible.
- [ ] Require diagnostics outputs to cite concrete evidence: deployment id, time window, metric deltas, and source tables or snapshots.
- [ ] Keep diagnostics explanatory rather than prescriptive by default; recommendations should stay operator-facing.
- [ ] Add CLI renderers so the same evidence can be inspected without the agent.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-research-harness rtk cargo test -p ployctl diagnostics -- --nocapture
cd ploy-sidecar && npm run build
```

Expected: operators can ask the system why a strategy behaved badly and get evidence-backed answers instead of prompt-only narratives.

### Task 4: Add traceability and evaluation for research and oversight runs

**Files:**
- Create: `crates/ploy-operator-contracts/src/agent.rs`
- Modify: `crates/ploy-operator-contracts/src/lib.rs`
- Modify: `crates/ploy-operator-contracts/src/events.rs`
- Create: `ploy-sidecar/src/runtime/run-recorder.ts`
- Create: `ploy-sidecar/src/runtime/session-store.ts`
- Modify: `ploy-sidecar/src/index.ts`
- Create: `ploy-sidecar/src/runtime/evaluator.ts`

- [ ] Assign a stable run id to every research, oversight, or diagnostics session.
- [ ] Persist tool calls, outputs, failure reasons, and final recommendations so runs are replayable and reviewable.
- [ ] Score runs by usefulness and correctness signals such as operator acceptance, false-positive rate, and repeated alert quality.
- [ ] Keep this trace layer outside the trading hot path.
- [ ] Surface the trace model through shared contracts so frontend, CLI, and daemon can all consume it later.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-research-harness rtk cargo test -p ploy-operator-contracts -- --nocapture
cd ploy-sidecar && npm run build
```

Expected: agent-assisted research and monitoring becomes observable, debuggable, and improvable instead of anecdotal.

### Task 5: Add a proposal-only control path for operator-approved safety actions

**Files:**
- Create: `crates/ploy-operator-contracts/src/proposals.rs`
- Modify: `crates/ploy-operator-contracts/src/lib.rs`
- Modify: `apps/ployd/src/http.rs`
- Create: `apps/ployctl/src/proposals.rs`
- Modify: `apps/ployctl/src/main.rs`
- Modify: `ploy-sidecar/src/tools/diagnostics.ts`
- Modify: `ploy-sidecar/src/schemas/output.ts`

- [ ] Add a first-class proposal model for actions like `pause deployment`, `drain deployment`, `reduce max exposure`, or `increase monitoring`.
- [ ] Keep proposals operator-approved; the agent should not directly execute live safety actions by default.
- [ ] Ensure every proposal cites the evidence that motivated it.
- [ ] Add CLI and API surfaces for listing, approving, and rejecting proposals.
- [ ] Preserve the control-plane principle: approved proposals still execute through the canonical runtime path.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-research-harness rtk cargo test -p ployctl -- --nocapture
```

Expected: the agent can help prevent strategy blowups without becoming the autonomous owner of live trading decisions.

### Task 6: Turn the frontend into a research and oversight console

**Files:**
- Create: `ploy-frontend/src/pages/ResearchRuns.tsx`
- Create: `ploy-frontend/src/pages/OversightAlerts.tsx`
- Create: `ploy-frontend/src/components/agent/DiagnosticsPanel.tsx`
- Create: `ploy-frontend/src/components/agent/ProposalQueue.tsx`
- Modify: `ploy-frontend/src/services/websocket.ts`
- Modify: `ploy-frontend/src/App.tsx`
- Modify: `README.md`

- [ ] Add views for research runs, oversight alerts, diagnostics evidence, and operator proposal review.
- [ ] Consume the new SSE events for oversight and traceability.
- [ ] Keep the UI focused on “what changed, why it matters, what should the operator review.”
- [ ] Do not present the agent as a trading engine; present it as a research and safety copilot.
- [ ] Make it easy to drill from a deployment to its recent backtests, alerts, and proposals.

Run:

```bash
cd ploy-frontend && npm run build
rg -n "ResearchRuns|OversightAlerts|DiagnosticsPanel|ProposalQueue" ploy-frontend/src
```

Expected: the operator UI becomes the place where human traders inspect agent-assisted research and safety signals.

### Task 7: Document the operating model and rollout constraints

**Files:**
- Modify: `README.md`
- Modify: `docs/runbooks/platform-startup.md`
- Modify: `docs/runbooks/live-deployment-checklist.md`
- Modify: `docs/agent-workflow.md`
- Modify: `tasks/todo.md`

- [ ] Document that strategy logic remains rule-driven and deterministic.
- [ ] Document the agent’s permitted domain: replay, backtest, optimization orchestration, monitoring, diagnostics, and proposal generation.
- [ ] Add rollout stages: trace-only research mode, oversight mode, proposal mode.
- [ ] Add an explicit checklist item forbidding agent-owned live strategy mutation.
- [ ] Keep AGENTS/CLAUDE workflow language aligned with this narrower harness model when implementation begins.

Run:

```bash
rg -n "Human owns alpha|Rules decide trades|Agent evaluates, monitors, and explains|proposal mode" README.md docs tasks/todo.md
```

Expected: the system’s role split is explicit in docs and operationally enforceable.

---

## Suggested Delivery Order

1. Task 1 — research harness surfaces
2. Task 2 — oversight and runaway detection
3. Task 4 — traceability and run evaluation
4. Task 6 — frontend research/oversight console
5. Task 3 — diagnostics depth and evidence workflows
6. Task 5 — proposal-only safety actions
7. Task 7 — docs and rollout hardening

## Exit Criteria

- Realtime trade judgment and execution remain fully rule-driven.
- Agent tooling can run replay, backtest, and bounded optimization workflows against existing data and modules.
- Strategy instability and drift can be surfaced as operator-visible alerts before they turn into uncontrolled losses.
- Agent conclusions are evidence-backed, replayable, and auditable.
- Any agent-suggested live safety action flows through a proposal/approval path rather than direct autonomous mutation.
