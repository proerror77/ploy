# Agent Instructions

This repository supports both Codex-style `AGENTS.md` and Claude-style `CLAUDE.md`.
Keep `AGENTS.md` and the repo-root `CLAUDE.md` aligned (same intent, same rules).

## Tool Mapping

When instructions mention Claude Code tools, map them like this in Codex:

- Read: use shell reads (`cat`, `sed`) or `rg`
- Write: create files via shell redirection or `apply_patch`
- Edit/MultiEdit: use `apply_patch`
- Bash: use `functions.exec_command`
- Grep: use `rg` (fallback: `grep`)
- Glob: use `rg --files` or `find`
- LS: use `ls` via `functions.exec_command`
- WebFetch/WebSearch: use `curl` (and Context7 for library docs when relevant)
- If `curl` cannot fetch meaningful page content (JS-rendered pages, anti-bot/Cloudflare, login walls), switch to the `agent-browser` skill workflow (`open` -> `snapshot -i` -> `get text body`) before trying mirrors.
- Parallel: use `multi_tool_use.parallel` for parallel shell reads/searches

## Git / Atomic Commits

Prefer **atomic commits**:

- Any code or docs modification must be committed atomically.
- One commit should represent one logical change.
- Keep refactors, formatting, and behavior changes in separate commits.
- Each commit should build (and run relevant tests when available).
- Avoid WIP commits on shared branches.

### Atomic Commit Execution Standard

- Do not commit per command; commit per completed intent.
- One commit unit should be independently testable and reversible.
- Before commit: stage explicit paths, review staged diff, run smallest relevant validation.
- If work is incomplete, keep it uncommitted or stash it; do not push partial WIP.
- Use commit message format `<scope>: <intent>` with concrete scope (for example `build`, `api`, `docs`, `strategy`, `ci`).

### Parallel Agent Isolation (Required)

- Use one branch and one worktree per agent for parallel work.
- Assign file ownership before coding; avoid overlapping edits across agents.
- Reserve high-conflict files (for example `Cargo.lock`, root workflows, route registries) to a single integrator.
- Integrate agent work with `cherry-pick` into the integration branch.
- If overlap is unavoidable, run those file changes sequentially instead of in parallel.

### Multi-Session Workflow (Required)

- Treat each session as isolated: one session = one worktree + one branch.
- Create sessions from updated main (example: `git fetch origin && git worktree add ../ploy-s1 -b session/s1 origin/main`).
- Define file ownership in `tasks/todo.md` before implementation; avoid cross-session overlap.
- Before editing, run preflight checks: `git status --short`, `git branch --show-current`, `git diff --name-only`.
- Stage explicit paths only, then verify with `git diff --cached` before commit.
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

## Workflow Orchestration

### 1. Plan Mode Default

- Enter plan mode for any non-trivial task (3+ steps or architectural decisions).
- If something goes sideways, stop and re-plan immediately instead of pushing through.
- Use plan mode for verification steps, not only implementation.
- Write detailed specs upfront to reduce ambiguity.

### 2. Subagent Strategy

- Prefer Agent team execution by default for non-trivial work.
- Use subagents liberally to keep the main context window clean.
- Offload research, exploration, and parallel analysis to subagents.
- For complex problems, use subagents to increase parallel compute.
- Keep one task per subagent for focused execution.

### 3. Self-Improvement Loop

- After any correction from the user, update `tasks/lessons.md` with the pattern.
- Write explicit rules that prevent repeating the same mistake.
- Iteratively refine lessons until error rate drops.
- Review relevant lessons at session start.

### 4. Verification Before Done

- Never mark a task complete without proving it works.
- Diff behavior between main and your changes when relevant.
- Ask yourself: "Would a staff engineer approve this?"
- Run tests, check logs, and demonstrate correctness.

### 5. Demand Elegance (Balanced)

- For non-trivial changes, pause and ask if there is a more elegant way.
- If a fix feels hacky, re-implement the elegant solution using current understanding.
- Skip this for simple, obvious fixes to avoid over-engineering.
- Challenge your own work before presenting it.

### 6. Autonomous Bug Fixing

- When given a bug report, fix it directly without requiring hand-holding.
- Start from logs, errors, and failing tests, then resolve root causes.
- Minimize context switching for the user.
- Fix failing CI tests proactively.

## Task Management

1. **Plan First**: Write a plan in `tasks/todo.md` with checkable items.
2. **Verify Plan**: Check in before starting implementation.
3. **Track Progress**: Mark items complete as you go.
4. **Explain Changes**: Provide a high-level summary at each step.
5. **Document Results**: Add a review section to `tasks/todo.md`.
6. **Capture Lessons**: Update `tasks/lessons.md` after corrections.

## Issue Tracking

- If a problem cannot be resolved in the current change, create or update an issue.
- Include clear context, impact, reproduction steps, and proposed next action.
- Link follow-up work to the issue so later corrections can be executed cleanly.

## Core Principles

- **Agent Team First**: Prefer Agent team/subagents for parallelizable or non-trivial tasks.
- **Skill-First Execution**: Use relevant skills whenever available before custom ad-hoc workflows.
- **Simplicity First**: Make every change as simple as possible with minimal code impact.
- **No Laziness**: Find root causes. Avoid temporary fixes. Maintain senior-engineer standards.
- **Minimal Impact**: Touch only what is necessary and avoid introducing new bugs.
