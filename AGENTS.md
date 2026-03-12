# Agent Instructions

This repository supports both Codex-style `AGENTS.md` and Claude-style
`CLAUDE.md`. Keep them aligned.

## Repo Layout

- `src/coordinator/`: coordinator-managed order ingress, risk, queueing, and
  runtime control.
- `src/strategy/`: trading strategies, runtime specs, and backtests.
- `src/persistence/`: event store, checkpoints, and schema helpers.
- `src/adapters/`: Polymarket, Binance, Postgres, and other external clients.
- `src/api/` and `src/tui/`: API surface and terminal dashboard.
- `config/` and `migrations/`: runtime TOML and PostgreSQL schema changes.
- `ploy-frontend/` and `ploy-sidecar/`: TypeScript frontend and sidecar
  projects.
- `docs/`, `tasks/`, and `todos/`: runbooks, plans, and tracked follow-up work.

## Run The Project

- Default safe local smoke path: `cargo run --bin ploy -- platform start
  --crypto --dry-run`
- Demo dashboard: `cargo run --bin ploy -- dashboard --demo`
- Frontend dev: `cd ploy-frontend && npm run dev`
- Sidecar dev: `cd ploy-sidecar && npm run dev`
- Full runtime setup, credentials, and command coverage live in
  [README.md](README.md).

## Build, Test, And Lint

- Rust edits: `rtk cargo check --bin ploy`
- Rust behavior changes before landing: `cargo fmt --check`,
  `cargo clippy -- -D warnings`, then `rtk cargo test`
- Rust release build when needed: `rtk cargo build --bin ploy`
- Frontend edits: `cd ploy-frontend && npm run build && npm run lint`
- Sidecar edits: `cd ploy-sidecar && npm run build`
- Docs or instruction edits: review links, examples, and keep `AGENTS.md` and
  `CLAUDE.md` aligned

## Engineering Conventions

- Keep changes small, focused, and atomic.
- Prefer `rg` for search and `apply_patch` for manual edits.
- Use `rtk` wrappers for supported high-output commands.
- For non-trivial work, write and maintain a short plan in `tasks/todo.md`.
- Use a relevant `SKILL.md` when the user names it or the task clearly matches
  one.
- Follow [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) for broader contributor
  guidance.
- Detailed workflow rules live in [docs/agent-workflow.md](docs/agent-workflow.md).

## Constraints And Do-Not Rules

- Default to dry-run and safe local validation. Do not enable live trading
  paths without explicit user intent and the required credentials.
- Prefer coordinator-managed live ingress via `ploy platform start`; avoid
  direct live order paths unless explicitly required.
- Do not build Rust on live trading hosts. Ship CI-built artifacts instead.
- Use separate worktrees when parallel agents or live sessions may touch the
  same files.
- Preserve user changes and never revert unrelated diffs.

## Done Means

- The requested behavior or docs change is complete and consistent with nearby
  files.
- Relevant tests or checks have been run, or any skipped validation is called
  out explicitly.
- The diff has been reviewed for regressions, risky patterns, and accidental
  scope creep.
