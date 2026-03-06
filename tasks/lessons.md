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

- Pattern: A live trading template can drift from the operator's intended strategy style even when the code path is unchanged, especially when timing gates are disabled and sum caps are tuned for a different regime.
- Rule: When a user describes intended entry behavior ("opening-window directional leg1" vs "strict sum-based arb"), verify both the checked-in TOML and the strategy defaults before diagnosing production inactivity.

- Pattern: Aggregate entry reject counters can be dominated by structural timing reasons and hide the actual signal path, making live strategy diagnosis look like a pricing problem when the runtime is not even reaching signal evaluation.
- Rule: For live/dry-run diagnostics, separate `entry_timing_gates` from `entry_signal_gates` and sample across a real event boundary before concluding that `sum`, `OBI`, or model thresholds are the blocker.

- Pattern: Opening-window strategies can silently miss valid entries when evaluation is only triggered by quote callbacks and the venue does not emit a fresh quote inside the narrow entry window.
- Rule: If a strategy has a short timing window, the live runtime must re-evaluate on periodic ticks using the latest cached market state, not only on event-driven quote deltas.

- Pattern: A directional `LEG1 -> LEG2` strategy needs two different price caps: one for generic forced cleanup and another for protective stop-loss merges. Reusing one threshold for both either disables stops or allows bad timeout fills.
- Rule: Keep `force_complete_threshold` for timeout/time-safety/final-window cleanup, and use a separate `protective_close_threshold` for stop-loss / theta-driven capped-loss merges.

- Pattern: When adding volatility regime filters, old entry tests can start failing for the wrong reason because their synthetic sigma is far outside the new allowed band.
- Rule: Tests that are validating timing or quote scoping must explicitly widen `max_entry_sigma` or shrink synthetic vol so they only exercise the intended gate.

- Pattern: Protective `LEG2` stop-loss logic can cap downside, but it does not create edge if `LEG1` is opened too far from ATM or too expensively.
- Rule: For OBI-triggered long-gamma profiles, keep the entry band explicitly tight enough to preserve convexity. If replay flips from positive to negative after enabling capped-loss stops, tighten `max_leg1_price`, `max_initial_sum`, `max_trades_per_event`, and the fair-value band before touching exit logic again.

- Pattern: Once the long-gamma entry band is reasonably tight, the next failure mode is still overpaying for entries just above parity without enough directional edge.
- Rule: Treat `sum > 1.00` as premium inventory. Require stronger direction strength and stronger OBI confirmation as `sum` rises above parity, instead of only clipping with a hard `max_initial_sum`.

- Pattern: Enforcing historical Binance L2 / OBI gates in replay without checking data coverage can make a healthy strategy look broken by collapsing valid windows to zero trades.
- Rule: Before requiring replay-time OBI history, measure `binance_lob_ticks` coverage for the requested window. If fresh L2 history is absent, fall back to price/Greeks-only entry and log that fallback explicitly.

- Pattern: Validating a replay parity gate on a window without overlapping PM replay and Binance L2 history gives a false negative and wastes time.
- Rule: Use one window with current production replay behavior for regression checks, and a separate overlap window where PM replay plus `binance_lob_ticks` both exist to prove the new gate actually changes trade selection.

- Pattern: Changing a strategy profile in the checked-in TOML is not enough if the live parser defaults and replay defaults still point at the old regime.
- Rule: Whenever a strategy's intended defaults change, update all three layers together: checked-in TOML, `from_toml` parser fallbacks, and `BacktestConfig::default()`. Add a regression test for missing-field TOML parsing so old defaults cannot silently leak back in.

- Pattern: When delayed-entry logic becomes part of the core profile, legacy tests can fail for timing reasons instead of the behavior they were supposed to cover.
- Rule: Tests that are not explicitly about post-open observation delay must either set `entry_after_start_min_secs = 0` or choose timestamps safely past the minimum delay, so failures keep pointing at the intended gate.

- Pattern: Removing hard entry caps and per-event trade limits can dramatically increase turnover; a profile can stay strongly positive on long / L2-rich windows while degrading to near-flat on a noisy short window.
- Rule: For aggressive OBI long-gamma profiles, always validate at least one short production-like March window and one L2-overlap window before calling the change an improvement. Report turnover alongside PnL so overtrading is visible.
