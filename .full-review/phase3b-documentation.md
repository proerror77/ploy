# Phase 3b: Documentation & API Review — Ploy Trading System

**Date**: 2026-03-08
**Scope**: Full Ploy trading system (Rust ~165K lines, TypeScript sidecar, React frontend)
**Focus**: Documentation completeness, accuracy, and API contracts

---

## Executive Summary

The project has a solid documentation foundation: a well-structured `CONTRIBUTING.md`, module-level `CLAUDE.md` files for AI-assisted development, design documents in `docs/plans/`, and a comprehensive frontend README in Chinese. However, there is no root-level README, no architecture documentation (ADRs), no API reference beyond the frontend README's endpoint list, and inline documentation (doc comments) is sparse — only ~1,110 `///` doc comments across 165K lines of Rust. The previous comprehensive review (Feb 2026) produced detailed findings in `docs/review-*.md` but these are point-in-time snapshots, not living documentation.

**Finding Distribution**: 2 Critical, 3 High, 4 Medium, 3 Low

---

## Critical Findings

### D-01: No root-level README.md

**Severity**: Critical
**Impact**: New contributors, auditors, and the developer's future self have no entry point to understand the system

The repository has no `README.md` at the root. The only README is `ploy-frontend/README.md` (in Chinese, covering the React dashboard). A developer cloning this repo sees no description of:
- What Ploy is and what it does
- How to build and run it
- Architecture overview
- Available commands and modes
- Configuration requirements
- Deployment instructions

The `CONTRIBUTING.md` partially fills this gap but is focused on workflow conventions, not system understanding.

**Recommendation**: Create a root `README.md` covering: purpose, architecture diagram (text-based), build instructions, configuration, available modes (`ploy platform start`, `ploy strategy`, etc.), and deployment.

### D-02: No Architecture Decision Records (ADRs)

**Severity**: Critical
**Impact**: Key architectural decisions are undocumented — why three agent abstractions? Why coordinator pattern? Why bootstrap DDL?

The system has several non-obvious architectural choices that are only discoverable by reading code:
- Three competing agent abstractions (DomainAgent, TradingAgent, Strategy) — no ADR explaining why
- Coordinator as central gateway vs. direct agent execution — no ADR
- Bootstrap DDL alongside sqlx migrations — no ADR explaining the shadow schema
- EIP-712 signing with custom nonce management — no ADR
- Sidecar architecture (TypeScript Claude Agent SDK) — no ADR

The `docs/plans/` directory has design documents but these are forward-looking proposals, not records of decisions made.

**Recommendation**: Create `docs/adr/` directory with ADRs for at least:
1. ADR-001: Coordinator gateway pattern for order execution
2. ADR-002: Three-layer agent abstraction (DomainAgent/TradingAgent/Strategy)
3. ADR-003: Bootstrap DDL for schema management
4. ADR-004: Sidecar architecture for AI-assisted trading

---

## High Severity Findings

### D-03: API endpoints undocumented beyond frontend README

**Severity**: High
**Location**: `src/api/` (routes.rs, handlers/)
**Impact**: No API reference for the Rust backend; frontend README lists endpoints but without auth requirements, error responses, or rate limits

The Axum API server exposes 20+ endpoints across admin, sidecar, system, and strategy namespaces. The only documentation is the endpoint list in `ploy-frontend/README.md`, which shows paths and HTTP methods but omits:
- Authentication requirements (which endpoints need admin vs sidecar token)
- Request/response schemas with examples
- Error response formats
- Rate limiting (currently none, per M-02)
- WebSocket protocol details beyond event names

**Recommendation**: Add OpenAPI/Swagger documentation or at minimum a `docs/API.md` with endpoint reference including auth, schemas, and examples.

### D-04: Inline documentation sparse on critical modules

**Severity**: High
**Impact**: ~1,110 doc comments across 165K lines = ~0.7% documentation density; critical modules like coordinator.rs (6,508 lines) have minimal doc comments

Documentation density by critical module:
- `coordinator.rs` (6,508 lines): Minimal — mostly on public structs, not on the complex order pipeline methods
- `bootstrap.rs` (7,761 lines): Sparse — the god module's internal functions are undocumented
- `staggered_arb_live.rs`: Some doc comments on public API, none on the 470-line entry evaluation
- `postgres.rs` (1,364 lines): 62 doc comments — reasonable for the store methods
- `polymarket_ws.rs`: 118 doc comments — well-documented (best in codebase)

**Recommendation**: Prioritize doc comments on:
1. All public trait methods (especially `Strategy`, `EngineStore`, `TradingAgent`)
2. The coordinator's order pipeline methods
3. Risk gate decision logic
4. Emergency stop behavior and guarantees

### D-05: Configuration documentation incomplete

**Severity**: High
**Location**: `src/config.rs` (1,168 lines), `config/` directory
**Impact**: 30+ config fields in `MomentumConfig` alone; no reference for what each field does, valid ranges, or defaults

The configuration system has:
- `config.rs` with 1,168 lines of config structs
- TOML config files in `config/` and `config/strategies/`
- Environment variable overrides scattered across 7+ files
- No single reference documenting all configuration options

A developer adding a new strategy must reverse-engineer config fields from struct definitions and `env::var()` calls.

**Recommendation**: Generate a configuration reference from the config structs (either via `serde` introspection or manually) documenting each field's purpose, type, default, and valid range.

---

## Medium Severity Findings

### D-06: Module-level CLAUDE.md files are AI-context, not human documentation

**Severity**: Medium
**Location**: `src/CLAUDE.md`, `src/adapters/CLAUDE.md`, `src/coordination/CLAUDE.md`, etc.
**Impact**: 9 module-level CLAUDE.md files provide AI assistant context but are not substitutes for human-readable module documentation

These files are valuable for AI-assisted development but don't serve as traditional module documentation. They contain instructions for Claude Code, not architectural explanations for human developers.

### D-07: Deployment documentation fragmented

**Severity**: Medium
**Location**: `docs/AWS_EC2_DEPLOYMENT_RUNBOOK.md`, `.github/workflows/deploy-*.yml`, `CONTRIBUTING.md`
**Impact**: Deployment knowledge is split across 6+ workflow files, a runbook, and contributing guide

There are 7 deployment-related workflow files (`deploy.yml`, `deploy-aws-jp.yml`, `deploy-prebuilt.yml`, `deploy-tango21.yml`, `release.yml`, `release-aliyun.yml`, `rollback.yml`) but no single document explaining:
- Which workflow to use for which environment
- The relationship between workflows
- How to perform a manual deployment
- How to verify a deployment succeeded

### D-08: Strategy documentation exists but is scattered

**Severity**: Medium
**Location**: `docs/strategies/`, `docs/plans/`, config files
**Impact**: Strategy-specific documentation is split between design docs, config comments, and code comments

The `docs/` directory has strategy-related files (`STRATEGY_FRAMEWORK_4_PILLARS.md`, `liquidity_vacuum_strategy.md`) but no unified strategy catalog documenting all available strategies, their parameters, and operational characteristics.

### D-09: Migration documentation missing

**Severity**: Medium
**Location**: `migrations/` (22 SQL files)
**Impact**: 22 migrations with no changelog or migration guide; shadow schema in bootstrap.rs not documented

The migrations directory has a `CLAUDE.md` but no human-readable documentation explaining:
- What each migration does
- The relationship between migrations and bootstrap DDL
- How to run migrations on a fresh database vs. existing production

---

## Low Severity Findings

### D-10: No CHANGELOG or release notes

**Severity**: Low
**Impact**: No record of what changed between deployments

### D-11: Previous review findings not tracked as issues

**Severity**: Low
**Impact**: The Feb 2026 review produced 4 detailed review documents in `docs/review-*.md` but findings aren't tracked in GitHub Issues for resolution

### D-12: Sidecar documentation minimal

**Severity**: Low
**Location**: `ploy-sidecar/`
**Impact**: No README for the sidecar; its purpose, configuration, and relationship to the Rust backend are undocumented

---

## Documentation Inventory

| Category | Status | Files |
|----------|--------|-------|
| Root README | MISSING | — |
| Contributing guide | GOOD | `docs/CONTRIBUTING.md` |
| Architecture docs | MISSING | No ADRs |
| API reference | PARTIAL | Frontend README only |
| Deployment guide | FRAGMENTED | Runbook + 7 workflows |
| Strategy docs | PARTIAL | `docs/strategies/`, scattered |
| Configuration ref | MISSING | — |
| Migration guide | MISSING | — |
| Module CLAUDE.md | GOOD (for AI) | 9 files |
| Design documents | GOOD | `docs/plans/` (3 design docs) |
| Inline doc comments | SPARSE | ~1,110 across 165K lines |
| Frontend README | GOOD | Chinese, comprehensive |
| Sidecar README | MISSING | — |
| Changelog | MISSING | — |

## Positive Patterns

- **CONTRIBUTING.md** is well-structured with build instructions, git workflow, CI/CD overview, code style, and feature flags
- **Module CLAUDE.md files** provide excellent AI-assisted development context for 9 key modules
- **Design documents** in `docs/plans/` capture forward-looking architectural proposals
- **Frontend README** is comprehensive with project structure, API requirements, deployment options, and troubleshooting
- **Previous review documents** (`docs/review-*.md`) provide detailed point-in-time analysis
- **Brainstorm documents** capture strategic thinking (`docs/brainstorms/`)
