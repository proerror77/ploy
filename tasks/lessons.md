# Lessons

## 2026-03-02

- Pattern: Some links (including but not limited to X) fail with plain `curl` because content is JS-rendered, anti-bot protected, or behind login walls.
- Rule: If `curl` output is mostly boilerplate/error shell and no meaningful body text, switch to the `agent-browser` skill first instead of spending time on mirror/curl scraping.
- Reusable flow:
  - `agent-browser --session <name> open <target-url>`
  - `agent-browser --session <name> snapshot -i`
  - `agent-browser --session <name> get text body`
  - Extract article body from the first real article sentence onward; ignore boilerplate/error shell text.
  - Validate key numbers with a quick local calculation (Node/JS) before final analysis.
  - `agent-browser --session <name> close`
- Output standard:
  - Provide a structured analysis (core thesis, what is correct, what is inconsistent, actionable use).
  - Include relevant source URL(s) in the final answer.
  - If user asks to "save the article", default archive format should contain only two sections: `内文` and `分析` (omit capture metadata unless explicitly requested).

- Pattern: Deploying a locally-built macOS binary (`target/release/ploy` from Darwin) to a Linux server causes immediate runtime failure (`Exec format error` / binary gibberish in shell).
- Rule: All production releases must be built on Linux CI runners and validated as ELF before deploy. Never SCP a locally-built macOS binary to Linux.
- Release guardrail checklist:
  - Build on `ubuntu-latest` only for production release artifacts.
  - Use explicit production features: `claimer_daemon,api,pm_ctf,tokio/io-std`.
  - Run `file target/release/ploy` and require `ELF 64-bit LSB` in CI before deployment.
  - Deploy only the CI-built artifact, not local `target/release/ploy`.

- Pattern: On macOS, `cargo build --target x86_64-unknown-linux-gnu` can fail for crates with C build steps (for example `ring`) when `x86_64-linux-gnu-gcc` is missing.
- Rule: For local Linux artifacts from macOS, default to `cargo zigbuild --target x86_64-unknown-linux-gnu` and always verify with `file` before deploy.
- Local Linux build checklist:
  - `cargo zigbuild --release --target x86_64-unknown-linux-gnu --features "claimer_daemon,api,pm_ctf"`
  - `file target/x86_64-unknown-linux-gnu/release/ploy` must contain `ELF 64-bit LSB`.

- Pattern: 在共享工作目录直接改代码时，可能混入其他会话/任务生成的意外文件（例如未跟踪目录），导致变更污染与流程中断。
- Rule: 非 trivial 开发任务开始前，默认先创建并切换到独立 `git worktree` + 独立分支；不得在主工作目录直接实施多文件改动。
- Preflight checklist:
  - `git fetch origin`
  - `git worktree add ../<repo>-<task> -b session/<task> origin/main`
  - `cd ../<repo>-<task>`
  - `git status --short && git branch --show-current && git diff --name-only`

- Pattern: User prefers no confirmation prompts before commit/push for completed requested work.
- Rule: When requested outputs are complete and scoped changes are clear, commit and push directly without asking "要不要 commit" first.
- Commit hygiene:
  - Stage only files related to current request.
  - Keep commits atomic and avoid including unrelated local changes.
  - Report commit SHA and push result in final response.

- Pattern: User strategy operations run on remote host (`tango-1-1`), not local workstation.
- Rule: For strategy/environment checks, deployment, startup validation, and runtime diagnostics, default to remote execution on `tango-1-1`. Use local checks only when explicitly requested.
- Remote-first checklist:
  - Verify target context first: `ssh tango-1-1 'hostname; pwd'`.
  - Read env/config on remote paths before concluding readiness.
  - Run strategy/platform start/health checks on `tango-1-1` and report remote results as source of truth.

## 2026-03-05

- Pattern: User terminology correction (`ripple` -> `repo`) can change task interpretation immediately.
- Rule: When user corrects a key noun or scope term, restate the corrected scope and continue execution using that corrected term only.
- Quick check:
  - Re-anchor analysis/search to the corrected term.
  - Avoid carrying forward assumptions from the wrong term.

- Pattern: Multi-file fixes often happen in dirty worktrees.
- Rule: In dirty worktree sessions, keep progressing after explicit user approval, and isolate commits by staging only request-related files/hunks.
- Commit safety:
  - Capture preflight (`git status --short`, `git diff --name-only`) before staging.
  - Use partial staging for mixed files; do not revert unrelated edits.
  - Report remaining unstaged files after commit.

## 2026-03-06

- Pattern: Managed strategy bootstrap can silently drift from the checked-in live strategy template and override production sizing without touching host config files.
- Rule: Managed `staggered_arb` runtime config must derive from the canonical `config/strategies/staggered_arb.toml` template and only override runtime-scoped fields like `symbols` and `series_ids`.
- Guardrail:
  - Add a regression test that asserts managed runtime config still contains `shares_per_trade = 20`.
  - Add a regression test that asserts managed runtime config does not inject `fixed_amount_usd` unless explicitly configured.

- Pattern: A release workflow can partially deploy to production and then fail late because the remote shell script is too clever.
- Rule: Keep remote deploy scripts explicit and small; prefer fixed service lists over dynamic shell pipelines for `systemctl` restarts, and syntax-check the extracted remote script before rerunning a production release.

- Pattern: In live trading flows, a partially-filled closing leg can be cancelled or fail after filling some shares; if the strategy does not accumulate those fills, the next retry can incorrectly submit the full original size again.
- Rule: For multi-step live orders, always treat partial fills as state transitions that mutate remaining exposure immediately. Retry logic must submit only residual shares and finalize the position from cumulative fills, not from the last order attempt alone.
- Verification checklist:
  - Persist cumulative filled shares/avg price/fees on every `Filled`, `Cancelled`, and `Failed` update that carries fills.
  - Clear in-flight markers after a partial close so the residual can retry cleanly.
  - Log `filled/target` progress on retries so live acceptance can prove the fix.

- Pattern: A release workflow can partially deploy the new binary and still fail availability because installed but inactive services are not explicitly started.
- Rule: Remote deploy steps must treat installed `ploy` services as start/restart targets, wait for `active`, and only then declare rollout success.
