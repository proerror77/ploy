# Phase 3: Testing & Documentation Review

**Branch:** `hotfix/staggered-arb-release-20260306` vs `main`
**Date:** 2026-03-11

---

## Test Coverage Findings (from phase3a-testing.md)

**Test inventory:** 80+ tests across coordinator, strategy, and integration layers. Key files: `staggered_arb_live/tests.rs` (43 tests, 2034 lines), `risk.rs` (13 tests), `coordinator/tests.rs` (9 tests), `bootstrap/tests.rs` (14 tests), `execution/engine/tests.rs` (14 tests), `duplicate_guard.rs` (10 tests). Integration tests: `tests/architecture_gateway_only.rs`, `tests/legacy_live_gate.rs`, `tests/strategy_evaluations_and_deployment_gate.rs`.

### Critical (5)

| ID | Area | Issue |
|----|------|-------|
| T-C1 | `risk/transitions.rs` | `record_loss` not resetting `consecutive_failures` is entirely untested — no test for loss-realizing sell between two failures |
| T-C2 | `risk/transitions.rs:68–71` | TOCTOU race on `Elevated→Normal` transition is untested — no concurrent `record_success` + `trigger_circuit_breaker` test |
| T-C3 | `position/transitions.rs:52–72` | Lock-ordering deadlock in `close_position` is untested — no concurrent close + reduce test with timeout |
| T-C4 | `coordinator/governance.rs` | Governance state not restored on restart is untested — no DB-backed restart round-trip test |
| T-C5 | `foreground_submit.rs` | Foreground path risk bypass partially tested (payload construction only) — `ForegroundIntentSubmitter::submit` lifecycle and direct-executor fallback have zero tests |

### High (4)

| ID | Area | Issue |
|----|------|-------|
| T-H1 | `staggered_arb_live` | No integration test for staggered arb intent flowing through coordinator risk gate |
| T-H2 | `staggered_arb_live` | No test for leg2 timeout / force-close path in live (non-dry-run) mode |
| T-H3 | `coordinator/bootstrap.rs` | `start_platform` bootstrap wiring is untested — only config rendering is tested |
| T-H4 | `coordinator/journal/restore.rs` | No end-to-end cold-start restore integration test (fill replay → position aggregator → risk counters) |

### Medium (5)

| ID | Area | Issue |
|----|------|-------|
| T-M1 | `coordinator/coordinator/ingress.rs:339–351` | `pending_buy_notional_excluding_domains` all-domains-excluded behavior undocumented — always returns zero for known domains |
| T-M2 | `coordinator/position.rs` | No concurrent enqueue/dequeue test for `OrderQueue` under load |
| T-M3 | `staggered_arb_live` | No test for partial fill → cancel lifecycle sequence |
| T-M4 | `coordinator/journal/restore.rs` | No test for restore with corrupt fills (zero shares, bad domain, negative price) |
| T-M5 | `coordinator/bootstrap/tests.rs` | No test for conflicting env var combinations in `PlatformBootstrapConfig::from_app_config` |

### Low (2)

| ID | Area | Issue |
|----|------|-------|
| T-L1 | `coordinator/governance.rs` | No test for `set_global_mode` clearing domain-level overrides |
| T-L2 | `coordinator/bootstrap/tests.rs` | Deprecated env var conflict test covers only one direction |

**Test Strengths:** `duplicate_guard.rs` (10 behavior-focused tests), `execution/engine/tests.rs` (14 tests with `RecordingStore` mock verifying optimistic-locking version sequences), `staggered_arb_live/tests.rs` (43 tests with comprehensive config/lifecycle coverage), `coordinator/tests.rs` (full buy→sell→position-reduce→pnl path).

**Test Weaknesses:** No concurrency tests anywhere in the coordinator; no integration tests wiring coordinator → risk → position → journal end-to-end; foreground path tests cover only JSON payload construction.

---

## Documentation Findings (from phase3b-documentation.md)

### High (3)

| ID | Area | Issue |
|----|------|-------|
| D-H1 | `coordinator/capital.rs`, `governance.rs`, `journal.rs` | Zero `//!` module-level docs on three non-trivial coordinator sub-modules |
| D-H2 | `src/control_plane/` | New module has no `//!` docs at all — role in four-layer architecture not explained |
| D-H3 | `CLAUDE.md` / `AGENTS.md` | Architecture decomposition not documented — no mention of coordinator sub-modules, canonical order path, foreground vs managed runtime distinction |

### Medium (4)

| ID | Area | Issue |
|----|------|-------|
| D-M1 | `staggered_arb_live/*.rs` | Six sub-modules (entry, leg2, lifecycle, order_updates, runtime_flow) have no `//!` docs despite being 19–66KB each |
| D-M2 | `CoordinatorHandle`, `AdmissionController`, `CapitalPolicy`, `GovernancePolicy`, `ExecutionJournal` | Key public types lack `///` doc comments |
| D-M3 | `docs/strategies/staggered_arb_state_machine.md` | State machine doc covers paper path only — `LiveOrderTrack` lifecycle, coordinator integration, and `--foreground` vs managed runtime not documented |
| D-M4 | No `CHANGELOG.md` | Breaking changes (bootstrap decomposition, new `control_plane` module, `TradeIntent` type) not documented |

### Low (3)

| ID | Area | Issue |
|----|------|-------|
| D-L1 | `cli/strategy/runtime_ops/foreground.rs` | No `//!` block warning that foreground bypasses coordinator risk gate — safety-critical omission |
| D-L2 | `MEMORY.md` | Auto-memory architecture section describes pre-decomposition coordinator structure |
| D-L3 | `docs/plans/2026-03-06-layered-live-runtime-design.md` | Primary architecture rationale document is in Traditional Chinese only |

---

## Phase 3 Summary

| Category | Critical | High | Medium | Low | Total |
|----------|----------|------|--------|-----|-------|
| Testing | 5 | 4 | 5 | 2 | 16 |
| Documentation | 0 | 3 | 4 | 3 | 10 |
| **Combined** | **5** | **7** | **9** | **5** | **26** |
