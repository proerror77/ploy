# Harness Engineering Trading Platform Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Evolve Ploy from a control-plane-centered trading platform with a light sidecar into a harness-engineered trading platform where agent behavior is traceable, policy-gated, replayable, and operator-visible without letting the agent bypass the canonical execution path.

**Architecture:** Keep `ployd` as the canonical control plane and only live execution ingress. Turn `ploy-sidecar` into a harnessed operator agent runtime rather than a thin polling loop. Reuse `crates/ploy-operator-contracts` and the existing SSE/event broker so frontend, CLI, and sidecar all observe the same agent/runtime events. Agent authority should grow through explicit policy and approval contracts, not through prompt-only conventions.

**Non-goals:**
- Do not move synchronous live HFT entry/exit decisions into the LLM loop.
- Do not create any direct sidecar-to-venue execution path.
- Do not bypass governance, risk, queueing, audit, or deployment lifecycle controls.

**Tech Stack:** Rust control-plane/event contracts, TypeScript sidecar runtime, existing SSE operator stream, JSONL or DB-backed trace persistence, frontend operator console, targeted Rust + TypeScript build/test checks

---

### Task 1: Add a first-class agent trace and outcome model

**Files:**
- Create: `crates/ploy-operator-contracts/src/agent.rs`
- Modify: `crates/ploy-operator-contracts/src/lib.rs`
- Modify: `crates/ploy-operator-contracts/src/events.rs`
- Modify: `apps/ployd/src/http.rs`
- Modify: `apps/ployd/src/main.rs`
- Modify: `ploy-sidecar/src/index.ts`
- Create: `ploy-sidecar/src/runtime/run-recorder.ts`
- Create: `ploy-sidecar/src/runtime/run-types.ts`

- [ ] Define shared agent run contracts: run start, tool call, recommendation, finish, failure, and policy denial.
- [ ] Extend `OperatorEvent` so agent events ride the same SSE pipe as system/deployment/trading snapshots.
- [ ] Assign a stable `run_id` per sidecar cycle and emit structured trace records instead of stdout-only observability.
- [ ] Persist sidecar traces to `run/sidecar/agent-runs.jsonl` so runs are replayable and diffable.
- [ ] Normalize terminal states so every run ends as one of: `success`, `no_action`, `policy_denied`, `tool_error`, `parse_error`, `budget_exceeded`, or `runtime_error`.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-harness rtk cargo test -p ploy-operator-contracts -- --nocapture
cd ploy-sidecar && npm run build
```

Expected: shared contracts compile, sidecar builds, and agent run traces become serializable and durable.

### Task 2: Move sidecar safety from prompt discipline into runtime policy gates

**Files:**
- Modify: `ploy-sidecar/src/index.ts`
- Modify: `ploy-sidecar/src/hooks/risk-guard.ts`
- Create: `ploy-sidecar/src/runtime/policy.ts`
- Create: `ploy-sidecar/src/runtime/tool-gate.ts`
- Modify: `apps/ployd/src/http.rs`
- Modify: `README.md`

- [ ] Wire `risk-guard` or an equivalent pre-tool gate into the actual sidecar runtime, not just as an optional helper file.
- [ ] Expand policy checks beyond `apply_deployment` to cover non-paper mutation, missing fresh runtime context, and forbidden deployment/runtime combinations.
- [ ] Emit explicit policy-denial events and persist them in the agent trace log.
- [ ] Document the sidecar authority bands (`observe_only`, `paper_mutation`, `approval_required_live`) and map them onto the existing `read_only` / `operator` / `admin` control-plane access model.
- [ ] Keep daemon-side auth as the hard boundary; sidecar runtime policy should be an additional guardrail, not a replacement.

Run:

```bash
cd ploy-sidecar && npm run build
rg -n "observe_only|paper_mutation|approval_required_live|policy denial" README.md ploy-sidecar/src
```

Expected: non-paper or out-of-policy actions are denied by runtime policy even if the prompt or model output is wrong.

### Task 3: Add a minimal harness runtime to the sidecar

**Files:**
- Create: `ploy-sidecar/src/runtime/session-store.ts`
- Create: `ploy-sidecar/src/runtime/planner.ts`
- Create: `ploy-sidecar/src/runtime/context-builder.ts`
- Create: `ploy-sidecar/src/runtime/evaluator.ts`
- Modify: `ploy-sidecar/src/index.ts`
- Modify: `ploy-sidecar/README.md`

- [ ] Replace the one-shot cycle shape with a tiny harness runtime that tracks plan, memory, recent failures, and final outcomes.
- [ ] Persist per-session artifacts under `run/sidecar/sessions/<session_id>/` with at least `plan.json`, `memory.json`, `last-context.json`, and `outcomes.jsonl`.
- [ ] Build context from both live control-plane state and recent agent history rather than only from fresh snapshots.
- [ ] Add a lightweight evaluator that scores whether the run produced a useful recommendation, a valid mutation, or a repeated failure pattern.
- [ ] Keep the first version narrow: no autonomous subagents, no speculative self-modification, no hidden live authority expansion.

Run:

```bash
cd ploy-sidecar && npm run build
rg -n "session-store|planner|context-builder|evaluator" ploy-sidecar/src
```

Expected: the sidecar can resume context between cycles and measure whether it is improving or just repeating itself.

### Task 4: Add an approval-based parity path for higher-risk actions

**Files:**
- Create: `crates/ploy-operator-contracts/src/approvals.rs`
- Modify: `crates/ploy-operator-contracts/src/lib.rs`
- Modify: `apps/ployd/src/http.rs`
- Modify: `apps/ployctl/src/main.rs`
- Create: `apps/ployctl/src/approvals.rs`
- Modify: `ploy-sidecar/src/tools/ploy-backend.ts`
- Modify: `ploy-sidecar/src/schemas/output.ts`

- [ ] Decide explicitly whether Ploy will remain an `agent-assisted paper-first platform` or become an `approval-gated live harness`.
- [ ] If approval-gated live actions are in scope, add proposal/approve/reject contracts rather than exposing raw live mutation directly to the sidecar.
- [ ] Keep `submit_paper_intent` as the default fast path; live mutation should become `proposal -> operator approval -> canonical control-plane action`.
- [ ] Extend `ployctl` with approval inspection and action commands so terminal operators are not forced into the browser.
- [ ] Record approved/rejected proposal outcomes in the same audit/event stream as other operator actions.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-harness rtk cargo test -p ployctl -- --nocapture
cd ploy-sidecar && npm run build
```

Expected: action parity can expand safely without giving the sidecar an unreviewed direct live path.

### Task 5: Turn the operator UI into a harness console

**Files:**
- Create: `ploy-frontend/src/pages/AgentRuns.tsx`
- Create: `ploy-frontend/src/components/agent/RunTracePanel.tsx`
- Create: `ploy-frontend/src/components/agent/ProposalQueue.tsx`
- Modify: `ploy-frontend/src/services/websocket.ts`
- Modify: `ploy-frontend/src/App.tsx`
- Modify: `README.md`

- [ ] Consume the new `agent_*` SSE events in the frontend.
- [ ] Add an `Agent Runs` view showing run status, cost, latency, final outcome, and recommendation summary.
- [ ] Add a trace panel showing tool calls, policy denials, parse failures, and final outputs.
- [ ] Add a proposal queue if approval-gated live actions are enabled.
- [ ] Keep the UI operator-facing: explain what the agent did, why it stopped, and whether human intervention is required.

Run:

```bash
cd ploy-frontend && npm run build
rg -n "Agent Runs|RunTracePanel|ProposalQueue|agent_" ploy-frontend/src
```

Expected: the frontend stops being only a trading dashboard and becomes the primary harness observability surface.

### Task 6: Add verification, docs, and rollout guardrails

**Files:**
- Modify: `README.md`
- Modify: `docs/runbooks/platform-startup.md`
- Modify: `docs/runbooks/live-deployment-checklist.md`
- Modify: `docs/agent-workflow.md`
- Modify: `tasks/todo.md`

- [ ] Document the intended end-state clearly: agent owns research, monitoring, proposal generation, and paper mutation; execution remains deterministic and control-plane-mediated.
- [ ] Add a rollout order that starts with trace-only mode, then policy-enforced paper mode, then optional approval-based live proposals.
- [ ] Record the exact verification commands and expected artifacts for each phase.
- [ ] Add an operational checklist item that explicitly forbids enabling live agent mutation before trace, policy, and approval surfaces are verified.
- [ ] Keep AGENTS/CLAUDE guidance aligned with the new harness model if the implementation changes workflow expectations.

Run:

```bash
rg -n "harness|agent trace|paper mutation|approval-based|control-plane-mediated" README.md docs tasks/todo.md
```

Expected: the architecture, rollout order, and safety model are explicit in operator-facing docs rather than living only in code or chat history.

---

## Suggested Delivery Order

1. Task 1 — trace model and persistence
2. Task 2 — runtime policy gates
3. Task 5 — frontend harness visibility
4. Task 3 — planner/memory/evaluator runtime
5. Task 4 — approval-based parity expansion
6. Task 6 — docs, rollout, and verification hardening

## Exit Criteria

- Every sidecar run is traceable, replayable, and classifiable.
- Sidecar safety boundaries are enforced in runtime code, not only in prompts.
- Agent actions and runtime state are visible through the same operator event stream as the rest of the platform.
- Live execution remains mediated by the canonical control plane.
- Any expansion of agent authority beyond paper mode happens through explicit approval contracts.
