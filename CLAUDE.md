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
- Parallel: use `multi_tool_use.parallel` for parallel shell reads/searches

## Skills

Skills are local instruction sets stored in `SKILL.md` files (usually under
`~/.codex/skills/` or `~/.agents/skills/` in this environment).

Use a skill when:

- The user names it explicitly, or
- The task clearly matches the skill description

When using a skill:

- Open the referenced `SKILL.md` and follow it.
- Prefer referenced scripts/templates over retyping.
- Keep context small: load only what you need.
- If a skill is missing/unreadable, state that and continue with best fallback.

