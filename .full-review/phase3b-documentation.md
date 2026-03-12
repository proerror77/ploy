# Phase 3b — Documentation Review

Branch: `hotfix/staggered-arb-release-20260306` vs `main`
Date: 2026-03-11
Reviewer: Claude Code (claude-sonnet-4-6)

---

## Summary

The hotfix branch introduces a significant coordinator decomposition, a new live strategy (`staggered_arb_live`), a new control plane module, and a new foreground CLI execution path. Documentation coverage is uneven: the design intent is well-captured in `docs/plans/`, the staggered arb state machine has a dedicated doc, and several key modules carry `//!` crate-level comments. However, multiple new modules are entirely undocumented at the module level, public API surface is largely uncommented, the architecture decomposition is not reflected in `CLAUDE.md`/`AGENTS.md`, and there is no changelog for breaking changes.

---

## Finding 1 — Missing `//!` module-level docs on coordinator sub-modules

**Severity: High**

`src/coordinator/capital.rs`, `src/coordinator/governance.rs`, and `src/coordinator/journal.rs` have zero `//!` doc comments. These are non-trivial modules that own distinct responsibilities (capital allocation policy, governance/ingress control, execution journal/restore). A new contributor reading `src/coordinator/mod.rs` sees the module list but has no inline explanation of what each module does.

By contrast, `src/coordinator/risk.rs` has a bilingual `//!` block, `src/coordinator/position.rs` and `src/coordinator/queue.rs` have brief `//!` headers, and `src/coordinator/strategy_runtime.rs` and `src/coordinator/coordinator.rs` both have clear `//!` blocks.

**Recommendation:** Add `//!` doc blocks to `capital.rs`, `governance.rs`, and `journal.rs` at minimum. Each block should state: the module's single responsibility, what it owns at runtime, and what it does not own (boundary statement). Example for `capital.rs`:

```rust
//! Capital Policy — per-domain allocation accounting
//!
//! Tracks open and pending notional across Crypto, Sports, Politics, and Economics
//! domains. Enforces per-deployment and per-domain budget limits before an intent
//! reaches the risk gate. Does not make risk decisions; only tracks capital state.
```

---

## Finding 2 — `src/control_plane/` has no module-level documentation at all

**Severity: High**

`src/control_plane/` contains `trade_intent.rs` and `risk_decision.rs`. Neither file has a `//!` module-level comment, and there is no `mod.rs` or `lib.rs` with a crate-level description. The control plane is a new concept introduced in this branch (per the design doc at `docs/plans/2026-03-06-layered-live-runtime-design.md`), and its role — deployment matrix, config projection, strategy enable/disable — is not obvious from the file names alone.

`TradeIntent` in `trade_intent.rs` has a single-line struct comment (`/// Unified strategy output contract (agent -> coordinator).`) which is good, but the module itself has no context about where it fits in the four-layer architecture.

**Recommendation:** Add a `//!` block to both files. For `trade_intent.rs`:

```rust
//! Control Plane — TradeIntent
//!
//! `TradeIntent` is the canonical output type for strategies submitting orders
//! through the control plane ingress path. It converts to `OrderIntent` before
//! entering the coordinator queue. This is the only supported entry point for
//! new live strategies; do not use `OrderIntent` directly from strategy code.
```

---

## Finding 3 — `src/strategy/staggered_arb_live/` sub-modules have no `//!` docs

**Severity: Medium**

`src/strategy/staggered_arb_live.rs` (the adapter root) has a good `//!` block explaining the strategy concept and CLI usage. However, the six sub-modules under `src/strategy/staggered_arb_live/` — `entry.rs`, `leg2.rs`, `lifecycle.rs`, `order_updates.rs`, `runtime_flow.rs`, `tests.rs` — have no `//!` comments. These files are large (19–66 KB) and contain the core state machine logic.

**Recommendation:** Add brief `//!` headers to each sub-module. At minimum:

- `entry.rs`: entry gate evaluation and Leg1 submission logic
- `leg2.rs`: Leg2 gate evaluation and submission logic
- `lifecycle.rs`: `PaperPosition`, `LiveOrderTrack`, and `PaperTrade` state types
- `order_updates.rs`: fill/cancel/timeout event handling
- `runtime_flow.rs`: top-level tick dispatch and feed routing

---

## Finding 4 — Public API surface is largely undocumented

**Severity: Medium**

Key public types and functions lack `///` doc comments:

- `CoordinatorHandle` (`src/coordinator/coordinator.rs` line 55): no doc comment on the struct or any of its methods. This is the primary interface given to all agents.
- `AdmissionController` (`src/coordinator/admission.rs` line 20): no doc comment. Its methods `apply_kelly_sizing`, `apply_min_order_constraints`, and `enforce_live_buy_deployment_gate` are all undocumented.
- `CapitalPolicy` (`src/coordinator/capital.rs` line 23): no doc comment.
- `GovernancePolicy` (`src/coordinator/governance.rs` line 35): no doc comment.
- `ExecutionJournal` (`src/coordinator/journal.rs` line 19): no doc comment.
- `PlatformStartControl` (`src/coordinator/bootstrap.rs` line 95): has a one-line `///` comment, which is adequate.
- `start_platform` (`src/coordinator/bootstrap.rs` line 104): has a two-line `///` comment, adequate.

**Recommendation:** At minimum, add `///` doc comments to `CoordinatorHandle` and `AdmissionController` since these are the most frequently referenced types by contributors adding new strategies or agents. The doc should state: what the type is, who creates it, and who consumes it.

---

## Finding 5 — Architecture decomposition not documented in `CLAUDE.md` / `AGENTS.md`

**Severity: High**

`CLAUDE.md` and `AGENTS.md` are identical and contain no mention of:

- The four-layer architecture (Strategy / Capital Governance / Execution / Control Plane)
- The coordinator decomposition into `admission`, `capital`, `governance`, `journal`, `position`, `queue`, `risk`, `strategy_runtime`
- The canonical live order path: `TradeIntent → AdmissionController → RiskGate → OrderQueue → OrderExecutor`
- The `src/control_plane/` module and its role
- The `staggered_arb_live` strategy and its foreground vs managed runtime modes
- The `--foreground` flag on `ploy strategy start` and when to use it vs `ploy platform start`

The design intent exists in `docs/plans/2026-03-06-layered-live-runtime-design.md` (Chinese), but that document is a design proposal, not a contributor-facing architecture reference. New contributors or agents working on this codebase will not know which runtime path to use for a new strategy.

**Recommendation:** Add an "Architecture" section to `CLAUDE.md` (and mirror to `AGENTS.md`) covering:

1. The four-layer model with one-line descriptions of each layer.
2. The canonical live order path as a single line: `Strategy → TradeIntent → Coordinator (admission → risk → queue) → Executor`.
3. Which module owns each layer (`src/strategy`, `src/coordinator`, `src/control_plane`).
4. The two runtime modes: `ploy strategy start --foreground` (lightweight, no coordinator) vs `ploy platform start` (full coordinator, managed runtime).
5. A pointer to `docs/plans/2026-03-06-layered-live-runtime-design.md` for the full rationale.

---

## Finding 6 — Foreground mode has no operational documentation

**Severity: High**

`src/cli/strategy/runtime_ops/foreground.rs` implements a standalone strategy execution path that bypasses the coordinator entirely (no risk gate, no position aggregation, no governance). This is a significant operational difference from `ploy platform start`. The code is clear but there is no documentation explaining:

- When foreground mode should be used (development, single-strategy testing)
- What it does NOT provide (no coordinator risk gate, no cross-strategy position awareness, no governance policy enforcement, no crash recovery)
- Whether it is safe for production use
- How it differs from the managed runtime in `src/coordinator/strategy_runtime.rs`

The `--foreground` flag is surfaced in `src/cli/strategy/runtime_ops.rs` but has no help text beyond the flag name.

**Recommendation:**

1. Add a `//!` block to `foreground.rs` explicitly stating it is a development/testing path and listing what coordinator features are absent.
2. Add a `/// WARNING: foreground mode bypasses the coordinator risk gate...` comment to `run_strategy_foreground`.
3. Add a note in `CLAUDE.md` under the architecture section distinguishing the two modes.

---

## Finding 7 — Staggered arb state machine doc is accurate but incomplete for the live path

**Severity: Medium**

`docs/strategies/staggered_arb_state_machine.md` documents the state machine well for the backtest/paper path. However, it does not cover:

- The `LiveOrderTrack` lifecycle (how live orders differ from paper positions)
- The `acknowledged_filled_qty` partial fill tracking
- The `cancel_requested_at` timeout/cancel flow for live orders
- How the live path interacts with the coordinator (via `StrategyAction::SubmitIntent` → `CoordinatorHandle::submit_order`)
- The `--foreground` vs managed runtime distinction for this strategy

The doc also references `entry_gate_*` and `leg2_gate_*` counters but does not explain how to query them at runtime (e.g., via the observability API or log fields).

**Recommendation:** Add a "Live Order Path" section to `staggered_arb_state_machine.md` covering `LiveOrderTrack` states and the coordinator integration. Add a note on how to access gate counters in production (log grep pattern or API endpoint if one exists).

---

## Finding 8 — No changelog for breaking changes

**Severity: Medium**

The hotfix branch introduces changes that break the previous coordinator API surface:

- `bootstrap.rs` is now a thin wrapper; the previous monolithic bootstrap logic has been split across `coordinator_bootstrap`, `runtime_orchestration`, `runtime_spawns`, and `startup_context` sub-modules.
- `run_managed_strategy_runtime` has moved from `bootstrap` internals to `src/coordinator/strategy_runtime.rs` as a `pub(crate)` function.
- The `src/control_plane/` module is new and introduces `TradeIntent` as the canonical strategy output type.

There is no `CHANGELOG.md` in the repo root, and `tasks/todo.md` was not reviewed for release notes. Contributors integrating this branch into downstream forks or dependent tooling have no structured record of what changed.

**Recommendation:** Create or update a `CHANGELOG.md` (or add a section to `tasks/todo.md`) documenting:

- The coordinator decomposition and which sub-modules replaced what
- The new `src/control_plane/` module and `TradeIntent` type
- The `--foreground` flag addition to `ploy strategy start`
- The `staggered_arb_live` strategy as a new live-tradeable strategy

---

## Finding 9 — `MEMORY.md` architecture section is stale

**Severity: Low**

`~/.claude/projects/-Users-proerror-Documents-ploy/memory/MEMORY.md` (the auto-memory file) describes the coordinator architecture under "Multi-Agent Coordinator (2026-02-11)" with the old structure: `state.rs`, `command.rs`, `config.rs`, `coordinator.rs`, `bootstrap.rs`. It does not reflect the new decomposition into `admission`, `capital`, `governance`, `journal`, `position`, `queue`, `risk`, `strategy_runtime` sub-modules.

This is a low-severity finding because `MEMORY.md` is auto-managed, but it will cause confusion for agents reading it in future sessions.

**Recommendation:** Update the "Multi-Agent Coordinator" section in `MEMORY.md` to reflect the current module structure. This can be done via a `save_memory` call or by editing the file directly.

---

## Finding 10 — Design doc is in Chinese only

**Severity: Low**

`docs/plans/2026-03-06-layered-live-runtime-design.md` is entirely in Traditional Chinese. This is the primary architectural rationale document for the coordinator decomposition. While the codebase has bilingual comments (Chinese + English), the design doc being monolingual limits accessibility for non-Chinese-reading contributors or agents.

**Recommendation:** Either add an English summary section at the top of the design doc, or create a companion `docs/plans/2026-03-06-layered-live-runtime-design-en.md` with the key decisions translated. At minimum, the "Acceptance Criteria" section (lines 316–323) should be available in English as it defines the done-state for the refactor.

---

## Coverage Matrix

| Area | Module-level `//!` | Public API `///` | Architecture doc | Operational doc |
|---|---|---|---|---|
| `coordinator/mod.rs` | Yes (5-line) | N/A | Partial (design doc) | No |
| `coordinator/admission.rs` | No | No | No | No |
| `coordinator/capital.rs` | No | No | No | No |
| `coordinator/governance.rs` | No | No | No | No |
| `coordinator/journal.rs` | No | No | No | No |
| `coordinator/risk.rs` | Yes (bilingual) | Partial | No | No |
| `coordinator/position.rs` | Yes (bilingual) | No | No | No |
| `coordinator/queue.rs` | Yes (1-line) | No | No | No |
| `coordinator/strategy_runtime.rs` | Yes (clear) | No | No | No |
| `coordinator/coordinator.rs` | Yes (clear) | Partial | No | No |
| `coordinator/bootstrap.rs` | Yes (clear) | Yes (start_platform) | No | No |
| `control_plane/trade_intent.rs` | No | Partial (struct only) | No | No |
| `control_plane/risk_decision.rs` | No | Unknown | No | No |
| `strategy/staggered_arb_live.rs` | Yes (good) | No | Partial (state machine doc) | Partial (CLI usage) |
| `strategy/staggered_arb_live/*.rs` | No | No | No | No |
| `cli/strategy/runtime_ops/foreground.rs` | No | No | No | No |
| `CLAUDE.md` / `AGENTS.md` | N/A | N/A | Stale (pre-decomposition) | No |
| `docs/plans/2026-03-06-*` | N/A | N/A | Yes (Chinese only) | No |
| `docs/strategies/staggered_arb_state_machine.md` | N/A | N/A | Partial (paper path only) | No |

---

## Priority Order for Remediation

1. **Finding 5** (CLAUDE.md/AGENTS.md architecture section) — highest leverage; affects every future agent session
2. **Finding 6** (foreground mode operational docs) — safety-critical; foreground bypasses risk gate
3. **Finding 1** (capital/governance/journal `//!` blocks) — quick wins, high signal-to-effort ratio
4. **Finding 2** (control_plane `//!` blocks) — new module, no context at all
5. **Finding 8** (changelog) — needed before merge to main
6. **Finding 4** (public API `///` comments) — important for long-term maintainability
7. **Finding 7** (staggered arb live path in state machine doc) — medium effort, medium value
8. **Finding 3** (staggered_arb_live sub-module `//!` blocks) — low effort
9. **Finding 9** (MEMORY.md staleness) — low effort, low urgency
10. **Finding 10** (design doc language) — low urgency
