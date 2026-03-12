# Agent Instructions

This repository supports both Codex-style `AGENTS.md` and Claude-style `CLAUDE.md`.
Keep `AGENTS.md` and the repo-root `CLAUDE.md` aligned (same intent, same rules).

## CLI Output Compression

Prefer `rtk` wrappers for commands that would otherwise emit large output or are
explicitly supported by RTK.

- Use `rtk read <file>` instead of raw `cat` / `head` / `tail`.
- Use `rtk git status`, `rtk git diff`, `rtk git log`, and `rtk git push`.
- Use `rtk cargo ...`, `rtk pytest`, `rtk test npm test`, and other RTK wrappers
  when they apply.
- If no RTK wrapper exists for the command you need, run the plain command.

## Tool Mapping

When instructions mention Claude Code tools, map them like this in Codex:

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

## Git / Atomic Commits

Prefer **atomic commits** for landed repo changes:

- When a task is meant to leave committed repo changes, keep them atomic.
- One commit should represent one logical change.
- Keep refactors, formatting, and behavior changes in separate commits.
- Each commit should build (and run relevant tests when available).
- Avoid WIP commits on shared branches.
- Pure review, research, and question-answer tasks do not require a commit by default.

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
