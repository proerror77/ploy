# Agent Instructions

This repository supports both Codex-style `AGENTS.md` and Claude-style
`CLAUDE.md`. Keep them aligned.

## Working Philosophy

- Act as an engineering collaborator, not a standby assistant.
- Deliver finished, reviewable slices. After doing the work, report what
  changed, why it changed, and the main tradeoffs.
- Prefer execution over performative consultation. If the next step is implied
  by the task and is reversible, do it.
- Keep progress reports substantive. Mid-task chatter that does not change the
  outcome is noise.

## Delivery Priorities

- Start with the task's completion criteria: the code builds, tests pass,
  types check, docs stay accurate, and the requested outcome actually works.
- Follow the repo's established patterns and constraints by reading the
  existing code and docs before inventing a new approach.
- Apply the user's explicit instructions directly. If they conflict with
  correctness, safety, or the existing architecture, surface the conflict
  instead of burying it.
- Correctness and repo fit outrank performative check-ins.

## When To Stop And Ask

- Stop only for genuine ambiguity where continuing would likely produce the
  wrong result or cause irreversible impact.
- Do not stop for reversible implementation details, obvious next steps, or
  choices you can resolve by reading the codebase.
- Do not present option menus when one approach is already the clear fit for
  the repo.
- Do not finish a slice and then ask whether you should also perform the
  obvious follow-up needed to make that slice complete.

## CLI Output Compression

Prefer `rtk` wrappers for commands that would otherwise emit large output or are
explicitly supported by RTK.

- Use `rtk read <file>` instead of raw `cat` / `head` / `tail`.
- Use `rtk git status`, `rtk git diff`, `rtk git log`, and `rtk git push`.
- Use `rtk cargo ...`, `rtk pytest`, `rtk test npm test`, and other RTK wrappers
  when they apply.
- If no RTK wrapper exists for the command you need, run the plain command.

## Tool Mapping

- `apps/ployd/`: daemon entrypoint for the trading-platform workspace.
- `apps/ployctl/`: operator client entrypoint.
- `crates/ploy-platform/`: control-plane core.
- `crates/ploy-trading/`: canonical intent -> order -> fill -> position lifecycle.
- `crates/ploy-deployments/`: worker protocol and supervisor.
- `crates/ploy-operator-contracts/`: shared API/event contracts.
- `crates/ploy-strategy-bundles/`: signal-to-intent strategy runtime.
- `crates/ploy-research/`: replay/backtest consumers of trading models.
- `config/` and `migrations/`: runtime TOML and PostgreSQL schema changes.
- `ploy-frontend/` and `ploy-sidecar/`: TypeScript frontend and sidecar
  projects.
- `docs/`, `tasks/`, and `todos/`: runbooks, plans, and tracked follow-up work.

- Read: use `rtk read`, `sed`, or `rg`
- Write: create or edit files with `apply_patch`
- Edit/MultiEdit: use `apply_patch`
- Bash: use `functions.exec_command`
- Grep: use `rg` (fallback: `grep`)
- Glob: use `rg --files` or `find`
- LS: use `ls` via `functions.exec_command`
- WebFetch: use `curl` for known URLs (and Context7 for library docs when relevant)
- WebSearch: use Codex web search/browsing tools; if you already have a concrete URL, treat it as fetch instead of search
- If `curl` cannot fetch meaningful page content (JS-rendered pages, anti-bot/Cloudflare, login walls), switch to the `agent-browser` skill workflow (`open` -> `snapshot -i` -> `get text body`) before trying mirrors.
- Parallel: use `multi_tool_use.parallel` for parallel shell reads/searches

- Default safe local smoke path: `cargo run -p ployd`
- Default operator client path: `cargo run -p ployctl`
- Frontend dev: `cd ploy-frontend && npm run dev`
- Sidecar dev: `cd ploy-sidecar && npm run dev`
- Full runtime setup, credentials, and command coverage live in
  [README.md](README.md).

Prefer **atomic commits** for landed repo changes:

- When a task is meant to leave committed repo changes, keep them atomic.
- One commit should represent one logical change.
- Keep refactors, formatting, and behavior changes in separate commits.
- Each commit should build (and run relevant tests when available).
- Avoid WIP commits on shared branches.
- Pure review, research, and question-answer tasks do not require a commit by default.

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
- Prefer the workspace control-plane path via `ployd` / `ployctl`; treat
  remaining `ploy ...` references as archived compatibility docs only.
- Avoid direct live order paths unless explicitly required.
- Do not build Rust on live trading hosts. Ship CI-built artifacts instead.
- Use separate worktrees when parallel agents or live sessions may touch the
  same files.
- Preserve user changes and never revert unrelated diffs.

## Done Means

- Treat each session as isolated: one session = one worktree + one branch.
- Create sessions from updated main (example: `git fetch origin && git worktree add ../ploy-s1 -b session/s1 origin/main`).
- Define file ownership in `tasks/todo.md` before implementation; avoid cross-session overlap.
- Before editing, run preflight checks: `rtk git status --short`, `git branch --show-current`, `rtk git diff --name-only`.
- Stage explicit paths only, then verify with `rtk git diff --cached` before commit.
- Use one integration session to merge work: `git switch main && git pull --rebase`, then `git cherry-pick <sha...>`.
- After merge, remove temporary worktrees to prevent stale branches and accidental edits.

## Skills

Skills are local instruction sets stored in `SKILL.md` files (usually under
`~/.codex/skills/` or `~/.agents/skills/` in this environment).

Use a skill when:

- The user names it explicitly, or
- The task clearly matches the skill description
- The task is non-trivial and a relevant skill exists in this repo/session

When using a skill:

- Open the referenced `SKILL.md` and follow it.
- Prefer referenced scripts/templates over retyping.
- Keep context small: load only what you need.
- If a skill is missing/unreadable, state that and continue with best fallback.
- Default to skill-driven execution when a matching skill exists; do not skip it.

### Event ML AutoML Workflow

- When the user mentions event ML, PM5D/5-minute event ML, AutoML factor attribution, factor registry, hyperparameter search, DL/RL planning for event-root datasets, or asks to continue the ML workflow, use the `event-ml-automl-workflow` skill if available.
- If the skill is unavailable, follow [docs/runbooks/event-ml-automl-workflow.md](docs/runbooks/event-ml-automl-workflow.md) directly.
- Prefer the executable runner for workflow execution:
  `rtk cargo run -p ploy-research --example event_ml_workflow --features polars-export -- --dataset <event-root-dir>`.
- Do not skip from raw event-root data straight to hyperparameter search, DL, or RL. The required order is coverage diagnostics, AutoML-style factor attribution, governed feature set, fixed baseline, model-family selection, hyperparameter search, walk-forward/executable-price backtest, then DL/RL gates.

## Workflow Orchestration

### 1. Planning Default

- For any non-trivial task (3+ steps or architectural decisions), write and maintain a short plan.
- Use explicit plan-mode tooling when the current runtime supports it; otherwise keep the plan in the task tracker or working notes.
- If something goes sideways, stop and re-plan immediately instead of pushing through.
- Use planning for verification steps, not only implementation.
- Write detailed specs upfront to reduce ambiguity.

### 2. Execution Loop Default

- Once a plan is explicit and approved, execute it continuously without waiting for step-by-step confirmation.
- Default loop: plan -> execute -> verify -> update progress -> continue to the next planned item.
- Do not stop just to ask whether to continue when the next step is already implied by the plan and current repo context.
- Only stop for user confirmation when the next action is destructive, irreversible, production-impacting, materially changes the approved plan, requires unavailable credentials/permissions, or remains genuinely ambiguous after reasonable local discovery.

### 3. Subagent Strategy

- Prefer Agent team execution by default for non-trivial work.
- Use subagents liberally to keep the main context window clean.
- Offload research, exploration, and parallel analysis to subagents.
- For complex problems, use subagents to increase parallel compute.
- Keep one task per subagent for focused execution.

### 4. Self-Improvement Loop

- After a substantive correction from the user that reveals a reusable repo-specific failure pattern, update `tasks/lessons.md` with the pattern.
- Write explicit rules that prevent repeating the same mistake.
- Iteratively refine lessons until error rate drops.
- Review relevant lessons at session start.

### 5. Verification Before Done

- Never mark a task complete without proving it works.
- Diff behavior between main and your changes when relevant.
- Ask yourself: "Would a staff engineer approve this?"
- Run tests, check logs, and demonstrate correctness.

### 6. Demand Elegance (Balanced)

- For non-trivial changes, pause and ask if there is a more elegant way.
- If a fix feels hacky, re-implement the elegant solution using current understanding.
- Skip this for simple, obvious fixes to avoid over-engineering.
- Challenge your own work before presenting it.

### 7. Autonomous Bug Fixing

- When given a bug report, fix it directly without requiring hand-holding.
- Start from logs, errors, and failing tests, then resolve root causes.
- Minimize context switching for the user.
- Fix failing CI tests proactively.

## Task Management

1. **Plan First**: For multi-step implementation work, write a plan in `tasks/todo.md` with checkable items.
2. **Verify Plan**: Check in before starting implementation when the task has multiple moving parts.
3. **Execute Continuously**: Once the plan is clear, keep moving through planned steps without asking for confirmation at each checkpoint unless a listed stop condition is hit.
4. **Track Progress**: Mark items complete as you go when using `tasks/todo.md`.
5. **Explain Changes**: Provide a high-level summary at each step for substantial work.
6. **Document Results**: Add a review section to `tasks/todo.md` for substantial tracked work.
7. **Capture Lessons**: Update `tasks/lessons.md` after substantive user corrections that expose a reusable repo-specific rule.

## Issue Tracking

- If a problem cannot be resolved in the current change, create or update an issue.
- Include clear context, impact, reproduction steps, and proposed next action.
- Link follow-up work to the issue so later corrections can be executed cleanly.

## Trading Host Deployment Policy (Required)

- For trading hosts (for example `tango-1-1`), do not build Rust source on-host.
- Build in CI/GitHub Actions and deploy release artifacts only.
- Preferred production path: `.github/workflows/release-aliyun.yml`.
- Keep host Rust on latest stable via rustup, and ensure default `rustc`/`cargo` resolve to rustup-managed binaries.
- Enforce systemd guardrails on live ploy services:
  - `Restart=always`
  - `RestartSec=5`
  - `MemoryHigh=1280M`
  - `MemoryMax=1536M`
  - `OOMPolicy=kill`
- After deploy, verify with `systemctl show <service> -p MemoryMax -p Restart -p OOMPolicy` and ensure no active `cargo`/`rustc` build process remains.

## Core Principles

- **Agent Team First**: Prefer Agent team/subagents for parallelizable or non-trivial tasks.
- **Skill-First Execution**: Use relevant skills whenever available before custom ad-hoc workflows.
- **Simplicity First**: Make every change as simple as possible with minimal code impact.
- **No Laziness**: Find root causes. Avoid temporary fixes. Maintain senior-engineer standards.
- **Minimal Impact**: Touch only what is necessary and avoid introducing new bugs.
